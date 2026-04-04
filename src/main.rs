use std::path::PathBuf;
use std::sync::Arc;

use abdb::server::config::AbdbConfig;
use abdb::server::tcp::TcpServer;

fn main() {
    let config = AbdbConfig {
        port: 8080,
        buffer_frame_size: 1024,
        data_dir: PathBuf::from("/var/lib/abdb"),
        evictor_lru_k_size: 2,
    };

    tokio::task::spawn_blocking(async || {
        let server = Arc::new(TcpServer::new(config).await);
        server.listen().await;
    });

    print!("Database server exiting.");
}
