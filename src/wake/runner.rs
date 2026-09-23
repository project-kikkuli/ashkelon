use std::future::Future;
use std::pin::Pin;

/// Result of running a short-lived local command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Result of a `GET`, kept as one named type rather than a tuple so the trait's return type
/// stays simple.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GetResponse {
    pub status: u16,
    pub body: Vec<u8>,
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

/// Talks to a local control endpoint (`opencode serve`'s HTTP API). A trait for the same reason
/// as [`CommandRunner`]: tests fake it instead of binding a real socket. `authorization`, when
/// present, is sent verbatim as the `Authorization` header value.
pub trait HttpPoster: Send + Sync {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: &'a [u8],
        authorization: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u16>> + Send + 'a>>;

    fn get_json<'a>(
        &'a self,
        url: &'a str,
        authorization: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<GetResponse>> + Send + 'a>>;
}

/// Talks to the endpoint for real, over loopback HTTP.
pub struct SystemPoster;

impl HttpPoster for SystemPoster {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: &'a [u8],
        authorization: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u16>> + Send + 'a>> {
        let body = body.to_vec();
        Box::pin(async move {
            let client = reqwest::Client::new();
            let mut req = client.post(url).header("content-type", "application/json").body(body);
            if let Some(auth) = authorization {
                req = req.header("authorization", auth);
            }
            let resp = req.send().await?;
            Ok(resp.status().as_u16())
        })
    }

    fn get_json<'a>(
        &'a self,
        url: &'a str,
        authorization: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<GetResponse>> + Send + 'a>> {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let mut req = client.get(url);
            if let Some(auth) = authorization {
                req = req.header("authorization", auth);
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            let body = resp.bytes().await?.to_vec();
            Ok(GetResponse { status, body })
        })
    }
}
