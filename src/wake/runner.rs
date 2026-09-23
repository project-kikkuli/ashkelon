use std::future::Future;
use std::pin::Pin;

/// Result of running a short-lived local command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs the small local commands `wake` shells out to (`tmux send-keys`, `codex queue`, ...).
/// A trait so tests can substitute a fake without touching the real `tmux`/`codex` binaries.
pub trait CommandRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CommandOutput>> + Send + 'a>>;
}

/// Runs commands for real via the OS.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CommandOutput>> + Send + 'a>> {
        Box::pin(async move {
            let output = tokio::process::Command::new(program).args(args).output().await?;
            Ok(CommandOutput { success: output.status.success(), stdout: output.stdout, stderr: output.stderr })
        })
    }
}

/// Posts a small JSON body to a local control endpoint (`opencode serve`'s HTTP API). A trait for
/// the same reason as [`CommandRunner`]: tests fake it instead of binding a real socket.
pub trait HttpPoster: Send + Sync {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u16>> + Send + 'a>>;
}

/// Posts for real, over loopback HTTP, via hyper.
pub struct SystemPoster;

impl HttpPoster for SystemPoster {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u16>> + Send + 'a>> {
        Box::pin(async move {
            use http_body_util::Full;
            use hyper_util::client::legacy::Client;
            use hyper_util::rt::TokioExecutor;

            let uri: hyper::Uri = url.parse()?;
            let client: Client<_, Full<bytes::Bytes>> = Client::builder(TokioExecutor::new()).build_http();
            let req = hyper::Request::builder()
                .method(hyper::Method::POST)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Full::new(bytes::Bytes::copy_from_slice(body)))?;
            let resp = client.request(req).await?;
            Ok(resp.status().as_u16())
        })
    }
}
