use std::path::PathBuf;
use std::sync::Arc;

use abdb::server::config::AbdbConfig;
use abdb::server::tcp::TcpServer;

#[tokio::main]
async fn main() {
    let config = AbdbConfig {
        port: 8080,
        buffer_frame_size: 1024,
        data_dir: PathBuf::from("./abdb_data"),
        evictor_lru_k_size: 2,
    };

    let server = Arc::new(TcpServer::new(config).await);

    print!("Starting abdb server");
    server.listen().await;

    print!("Database server exiting.");
}
