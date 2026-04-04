use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::binder::Binder;
use crate::optimizer::Optimizer;
use crate::planner::Planner;
// use crate::parser::Parser;
use crate::{
    accessor::AccessorImpl,
    buffer::{evictor::LruKEvictor, r#impl::BufferPool},
    server::config::AbdbConfig,
    storage::{DiskManagerImpl, allocator::SimpleAllocator, directory::BTreePageDirectory},
};

pub struct TcpServer {
    config: AbdbConfig,
    accessor: Arc<AccessorImpl<BufferPool<DiskManagerImpl<BTreePageDirectory, SimpleAllocator>>>>,
}

impl TcpServer {
    pub async fn new(config: AbdbConfig) -> Self {
        let page_directory = Arc::new(
            BTreePageDirectory::open(config.data_dir.join("/page.dir"))
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
            disk_manager,
            eviction_policy,
        ));
        let accessor = AccessorImpl::new(buffer_pool);

        Self {
            config,
            accessor: Arc::new(accessor),
        }
    }

    pub async fn listen(self: Arc<Self>) -> ! {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", self.config.port))
            .await
            .expect("Error while creating TcpListener");
        println!("Database server listening on 127.0.0.1:8080");

        loop {
            let (mut socket, addr) = listener
                .accept()
                .await
                .expect("Error while accepting connection");
            println!("New client connected: {}", addr);
            let server = Arc::clone(&self);

            tokio::spawn(async move {
                // 1. Split the socket so we can read and write independently
                let (reader, mut writer) = socket.split();

                // 2. Wrap the reader in a BufReader to handle newline buffering
                let mut buf_reader = BufReader::new(reader);
                let mut query = String::new();

                loop {
                    query.clear(); // Clear the buffer for the next query

                    // 3. Read until we hit a newline (\n)
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
                            let result = server.process_sql(sql).await;

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

    async fn process_sql(&self, sql: &str) -> String {
        let mut binder = Binder::new(&*self.accessor);
        let parser = Parser::new(&PostgreSqlDialect {});
        let planner = Planner::new(&*self.accessor);
        let optimizer = Optimizer::new(&*self.accessor);

        let stmt = parser
            .try_with_sql(sql)
            .expect("Parser error")
            .parse_statement()
            .expect("Parser error");
        let bound_stmt = binder.bind_statement(&stmt).expect("Binder error");
        let logical_plan = planner.plan(&bound_stmt).expect("Planner error");
        let physical_plan = optimizer.optimize(logical_plan);

        // Executor call

        return format!("{:#?}", physical_plan);
    }
}
