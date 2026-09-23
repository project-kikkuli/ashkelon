use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config::Config;
use crate::hooks::Engine;

pub async fn serve(_cfg: Arc<Config>, _listener: TcpListener, _engine: Arc<Engine>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}
