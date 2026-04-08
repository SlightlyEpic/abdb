use std::path::PathBuf;
use std::sync::Arc;

use abdb::server::config::AbdbConfig;
use abdb::server::tcp::TcpServer;
use tokio::signal;

#[tokio::main]
async fn main() {
    let config = AbdbConfig {
        port: 8080,
        buffer_frame_size: 1024,
        data_dir: PathBuf::from("./abdb_data"),
        evictor_lru_k_size: 2,
    };

    let server = Arc::new(TcpServer::new(config).await);

    println!("Starting abdb server");

    // Run the listener and a shutdown signal handler concurrently.
    // Whichever resolves first tears down the other via `select!`.
    let server_for_listen = Arc::clone(&server);
    tokio::select! {
        _ = server_for_listen.listen() => {
            // listen() currently runs forever; if it returns, treat as exit.
        }
        res = shutdown_signal() => {
            match res {
                Ok(sig) => println!("\nReceived {}, shutting down gracefully...", sig),
                Err(e) => eprintln!("\nSignal handler error: {}; shutting down...", e),
            }
        }
    }

    // Flush any dirty state before exit so in-flight writes are durable.
    server.flush().await;

    println!("Database server exiting.");
}

/// Wait for SIGINT (Ctrl-C) or SIGTERM and return a label for logging.
async fn shutdown_signal() -> std::io::Result<&'static str> {
    #[cfg(unix)]
    {
        use signal::unix::{SignalKind, signal as unix_signal};
        let mut sigterm = unix_signal(SignalKind::terminate())?;
        let mut sigint = unix_signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = sigterm.recv() => Ok("SIGTERM"),
            _ = sigint.recv() => Ok("SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        signal::ctrl_c().await?;
        Ok("Ctrl-C")
    }
}
