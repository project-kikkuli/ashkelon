mod client;
mod decode;
mod pipeline;
mod route;
mod tracker;

use std::sync::Arc;

use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::hooks::Engine;
use crate::telemetry::Writer;

pub use tracker::Tracker;

#[cfg(feature = "test-util")]
pub use pipeline::test_hooks;

/// Serves the relay on `listener` until a fatal setup error occurs (there is no upstream client to
/// relay through). Each connection is handled independently; a panic or error on one never takes
/// down another in-flight call, and a transient `accept()` failure never ends the loop either —
/// this is the one task standing between the agent and its model, so it keeps running rather than
/// handing a setup or accept hiccup back to its caller as a reason to give up. `tracker` counts the
/// response-forwarding tasks `pipeline::handle` spawns per request, so a caller about to exit the
/// process (`ashkelon run`) can drain them first — see [`Tracker`]'s doc comment for why that matters.
pub async fn serve(
    cfg: Arc<Config>,
    listener: TcpListener,
    engine: Arc<Engine>,
    tracker: Arc<Tracker>,
) -> anyhow::Result<()> {
    let client = client::build()?;
    let telemetry = Arc::new(Writer::new(cfg.log_dir()).unwrap_or_else(|e| {
        tracing::error!(error = %e, "failed to set up the call log directory; relaying continues without it");
        Writer::degraded(cfg.log_dir())
    }));

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::error!(error = %err, "ashkelon: accept() failed; retrying instead of stopping the relay");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        let io = TokioIo::new(stream);
        let cfg = cfg.clone();
        let engine = engine.clone();
        let client = client.clone();
        let telemetry = telemetry.clone();
        let tracker = tracker.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                pipeline::handle(
                    req,
                    cfg.clone(),
                    engine.clone(),
                    client.clone(),
                    telemetry.clone(),
                    tracker.clone(),
                )
            });
            let builder = AutoBuilder::new(TokioExecutor::new());
            let conn = builder.serve_connection_with_upgrades(io, service);
            if let Err(err) = conn.await {
                tracing::debug!(error = %err, "relay connection closed");
            }
        });
    }
}
