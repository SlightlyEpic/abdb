use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::session::Session;
use crate::transaction::TransactionManager;
use crate::{
    accessor::AccessorImpl,
    buffer::{evictor::LruKEvictor, r#impl::BufferPool},
    db,
    response::Response,
    server::config::AbdbConfig,
    storage::{DiskManagerImpl, allocator::SimpleAllocator, directory::BTreePageDirectory},
};
use crate::storage::directory::PageDirectory;
use crate::binder::OidAllocator;
use crate::common::aliases::{FileId, OId};

impl OidAllocator for BTreePageDirectory {
    fn next_oid(&self) -> OId {
        self.allocate_oid()
    }
    fn next_file_id(&self) -> FileId {
        self.allocate_file_id()
    }
}

pub struct TcpServer {
    config: AbdbConfig,
    accessor: Arc<AccessorImpl<BufferPool<DiskManagerImpl<BTreePageDirectory, SimpleAllocator>>>>,
    txn_manager: Arc<TransactionManager>,
    oid_allocator: Arc<dyn OidAllocator>,
}

impl TcpServer {
    pub async fn new(config: AbdbConfig) -> Self {
        std::fs::create_dir_all(&config.data_dir).expect("Could not create data dir");

        let needs_bootstrap = !db::database_exists(&config.data_dir);

        let page_directory = Arc::new(
            BTreePageDirectory::open(config.data_dir.join("page.dir"))
                .await
                .expect("Could not create page directory"),
        );
        let oid_allocator: Arc<dyn OidAllocator> = Arc::clone(&page_directory) as Arc<dyn OidAllocator>;

        let next_lpage_id = page_directory.get_next_lpage_id().await.unwrap_or(0);

        let page_allocator = Arc::new(SimpleAllocator::new());
        let disk_manager = Arc::new(DiskManagerImpl::new(
            config.data_dir.clone(),
            page_directory,
            page_allocator,
            next_lpage_id,
        ));
        let eviction_policy = Box::new(LruKEvictor::new(config.evictor_lru_k_size));
        let buffer_pool = Arc::new(BufferPool::new(
            config.buffer_frame_size,
            Arc::clone(&disk_manager),
            eviction_policy,
        ));
        let accessor = Arc::new(AccessorImpl::new(Arc::clone(&buffer_pool)));

        let max_xmin = if needs_bootstrap {
            println!("Initializing new database...");
            db::bootstrap_database(&*buffer_pool, &*disk_manager, &accessor)
                .await
                .expect("Failed to bootstrap database");
            db::write_marker_file(&config.data_dir)
                .expect("Failed to write database marker file");
            println!("Database initialized successfully.");
            0
        } else {
            println!("Loading existing database...");
            let x = db::load_catalog(&*buffer_pool, &*disk_manager, &accessor)
                .await
                .expect("Failed to load catalog");
            println!("Catalog loaded successfully. max_xmin={}", x);
            x
        };

        Self {
            config,
            accessor,
            txn_manager: Arc::new(TransactionManager::with_next_txn_id(max_xmin + 1)),
            oid_allocator,
        }
    }

    pub async fn listen(self: Arc<Self>) {
        let host = "127.0.0.1";
        let port = self.config.port;

        let listener = TcpListener::bind(format!("{}:{}", host, port))
            .await
            .expect("Error while creating TcpListener");
        println!("Database server listening on {}:{}", host, port);

        loop {
            let (mut socket, addr) = listener
                .accept()
                .await
                .expect("Error while accepting connection");
            println!("New client connected: {}", addr);
            let mut session =
                Session::new(Arc::clone(&self.accessor), Arc::clone(&self.txn_manager), Arc::clone(&self.oid_allocator));

            tokio::spawn(async move {
                let (reader, mut writer) = socket.split();

                let welcome = "Connected to abdb\n\nabdb> ";
                if writer.write_all(welcome.as_bytes()).await.is_err() {
                    return;
                }

                let mut buf_reader = BufReader::new(reader);
                let mut line = String::new();

                loop {
                    line.clear();
                    match buf_reader.read_line(&mut line).await {
                        Ok(0) => {
                            println!("Client {} disconnected.", addr);
                            return;
                        }
                        Ok(_) => {
                            let sql = line.trim();
                            if sql.is_empty() {
                                let _ = writer.write_all(b"abdb> ").await;
                                continue;
                            }

                            let responses = session.execute_sql(sql).await;
                            let mut output = String::from("\n");

                            for r in responses {
                                match r {
                                    Ok(resp) => output.push_str(&format_response(resp)),
                                    Err(e) => output.push_str(&format!("ERROR: {}\n", e)),
                                }
                            }

                            output.push_str("\nabdb> ");
                            if writer.write_all(output.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            eprintln!("Error reading from client {}: {}", addr, e);
                            return;
                        }
                    }
                }
            });
        }
    }
}

fn format_response(resp: Response) -> String {
    match resp {
        Response::Notice(msg) => format!("{}\n", msg),
        Response::RowsAffected(n) => format!("{} row(s) affected\n", n),
        Response::BeginTransaction => "BEGIN\n".into(),
        Response::Commit => "COMMIT\n".into(),
        Response::Rollback => "ROLLBACK\n".into(),
        Response::Rows { columns, rows } => {
            if columns.is_empty() {
                return format!("({} row(s))\n", rows.len());
            }

            let mut col_widths: Vec<usize> = columns.iter().map(|c| c.name.len()).collect();
            let stringified: Vec<Vec<String>> = rows
                .iter()
                .map(|row| row.values.iter().map(|v| v.to_string()).collect())
                .collect();

            for str_row in &stringified {
                for (i, val) in str_row.iter().enumerate() {
                    if i < col_widths.len() {
                        col_widths[i] = col_widths[i].max(val.len());
                    }
                }
            }

            let mut out = String::new();

            let header: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c.name, width = col_widths[i]))
                .collect();
            out.push_str(&header.join(" | "));
            out.push('\n');

            let sep: Vec<String> = col_widths.iter().map(|&w| "-".repeat(w)).collect();
            out.push_str(&sep.join("-+-"));
            out.push('\n');

            for str_row in stringified {
                let cells: Vec<String> = str_row
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let w = col_widths.get(i).copied().unwrap_or(0);
                        format!("{:<width$}", v, width = w)
                    })
                    .collect();
                out.push_str(&cells.join(" | "));
                out.push('\n');
            }

            out.push_str(&format!("({} row(s))\n", rows.len()));
            out
        }
    }
}