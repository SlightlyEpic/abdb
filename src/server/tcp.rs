use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::session::Session;
use crate::transaction::TransactionManager;
use crate::{
    accessor::AccessorImpl,
    buffer::{evictor::LruKEvictor, r#impl::BufferPool},
    db,
    server::config::AbdbConfig,
    storage::{DiskManagerImpl, allocator::SimpleAllocator, directory::BTreePageDirectory},
};

pub struct TcpServer {
    config: AbdbConfig,
    accessor: Arc<AccessorImpl<BufferPool<DiskManagerImpl<BTreePageDirectory, SimpleAllocator>>>>,
    txn_manager: Arc<TransactionManager>,
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
        let page_allocator = Arc::new(SimpleAllocator::new());
        let disk_manager = Arc::new(DiskManagerImpl::new(
            config.data_dir.clone(),
            page_directory,
            page_allocator,
            0,
        ));
        let eviction_policy = Box::new(LruKEvictor::new(config.evictor_lru_k_size));
        let buffer_pool = Arc::new(BufferPool::new(
            config.buffer_frame_size,
            Arc::clone(&disk_manager),
            eviction_policy,
        ));
        let accessor = Arc::new(AccessorImpl::new(Arc::clone(&buffer_pool)));

        if needs_bootstrap {
            println!("Initializing new database...");
            db::bootstrap_database(&*buffer_pool, &*disk_manager, &accessor)
                .await
                .expect("Failed to bootstrap database");
            db::write_marker_file(&config.data_dir)
                .expect("Failed to write database marker file");
            println!("Database initialized successfully.");
        } else {
            println!("Loading existing database...");
            db::load_catalog(&*buffer_pool, &accessor)
                .await
                .expect("Failed to load catalog");
            println!("Catalog loaded successfully.");
        }

        Self {
            config,
            accessor,
            txn_manager: Arc::new(TransactionManager::new()),
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
                Session::new(Arc::clone(&self.accessor), Arc::clone(&self.txn_manager));

            tokio::spawn(async move {
                // 1. Split the socket so we can read and write independently
                let (reader, mut writer) = socket.split();

                // 2. Wrap the reader in a BufReader to handle newline buffering
                let mut buf_reader = BufReader::new(reader);
                let mut query = String::new();

                loop {
                    query.clear(); // Clear the buffer for the next query

                    // 3. Read until we hit a newline (\n)
                    // TODO: use ';' as delimiter
                    match buf_reader.read_line(&mut query).await {
                        Ok(0) => {
                            println!("Client {} disconnected.", addr);
                            return; // Connection closed securely
                        }
                        Ok(_) => {
                            let sql = query.trim();
                            if sql.is_empty() {
                                continue;
                            } // Ignore empty lines

                            println!("Executing: {}", sql);

                            // 4. Send to your database engine
                            let result = match session.execute_sql(sql).await {
                                Ok(res) => res,
                                Err(e) => e.to_string(),
                            };

                            // 5. Send the result back to the client
                            if writer.write_all(result.as_bytes()).await.is_err() {
                                println!("Failed to write to client {}", addr);
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
