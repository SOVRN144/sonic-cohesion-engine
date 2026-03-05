use clap::Parser;
use sce_core::{api, storage, worker};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
struct Args {
    /// SQLite database URL, e.g. sqlite://./sce.db
    #[arg(long, default_value = "sqlite://./sce.db")]
    db_url: String,

    #[arg(long, default_value = "127.0.0.1:9911")]
    bind: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let db = storage::Db::connect(&args.db_url).await?;
    storage::enforce_startup_invariants(&db).await?;

    let app = api::router(api::AppState { db: db.clone() });
    let listener = tokio::net::TcpListener::bind(&args.bind).await?;
    tracing::info!(bind = %args.bind, db_url = %args.db_url, "sce daemon listening");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let worker_db = db.clone();
    let worker_handle = tokio::spawn(async move {
        if let Err(err) = worker::run_worker_loop(worker_db, shutdown_rx).await {
            tracing::error!(error = ?err, "worker terminated with error");
        }
    });

    let shutdown_tx_for_signal = shutdown_tx.clone();
    let shutdown_signal = async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx_for_signal.send(true);
        tracing::info!("shutdown signal received");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    let _ = shutdown_tx.send(true);
    if let Err(err) = worker_handle.await {
        tracing::error!(error = ?err, "failed to join worker task");
    }

    Ok(())
}
