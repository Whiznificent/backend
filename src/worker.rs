#[tokio::main]
async fn main() {
    zenith_backend::init_tracing();
    let state = zenith_backend::init_worker_state().await;
    tracing::info!("Zenith background worker started");
    tokio::select! {
        _ = zenith_backend::jobs::run_loop(state) => {}
        _ = zenith_backend::shutdown_signal() => {
            tracing::info!("Zenith background worker shutting down");
        }
    }
}
