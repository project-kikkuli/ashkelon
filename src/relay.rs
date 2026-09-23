mod client;
mod decode;
mod pipeline;
mod route;

use std::sync::Arc;

use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::hooks::Engine;
use crate::telemetry::Writer;

/// Serves the relay on `listener` until it errors (the listener is closed) or a fatal setup error
/// occurs. Each connection is handled independently; a panic or error on one never takes down
/// another in-flight call.
pub async fn serve(cfg: Arc<Config>, listener: TcpListener, engine: Arc<Engine>) -> anyhow::Result<()> {
    let client = client::build()?;
    let telemetry = Arc::new(Writer::new(cfg.log_dir())?);

    loop {
        let (stream, _addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let cfg = cfg.clone();
        let engine = engine.clone();
        let client = client.clone();
        let telemetry = telemetry.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                pipeline::handle(req, cfg.clone(), engine.clone(), client.clone(), telemetry.clone())
            });
            let builder = AutoBuilder::new(TokioExecutor::new());
            let conn = builder.serve_connection_with_upgrades(io, service);
            if let Err(err) = conn.await {
                tracing::debug!(error = %err, "relay connection closed");
            }
        });
    }
}
