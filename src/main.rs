use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ashkelon", version, about = "Local gateway for coding agents")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the relay on its own, for harnesses configured by hand.
    Serve,
    /// Start a harness pointed at an in-process relay: `ashkelon run claude -- <args>`.
    Run {
        harness: String,
        /// Skip wiring Claude Code's MCP channel wake mechanism.
        #[arg(long)]
        no_channel: bool,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Call a configured model (for hooks): prompt on stdin, response text on stdout.
    Model {
        name: String,
        #[arg(long)]
        system: Option<String>,
    },
    /// The stdio MCP "channel" server `ashkelon run claude` registers itself as with Claude Code.
    /// Not meant to be run by hand.
    #[command(hide = true)]
    Channel {
        #[arg(long)]
        socket: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    let cli = Cli::parse();
    let cfg = Arc::new(ashkelon::config::Config::load(cli.config.as_deref())?);
    match cli.command {
        Command::Serve => serve(cfg).await,
        Command::Run { harness, no_channel, args } => run(cfg, &harness, no_channel, &args).await,
        Command::Model { name, system } => model(cfg, &name, system.as_deref()).await,
        Command::Channel { socket } => ashkelon::wake::channel::run(&socket).await,
    }
}

async fn serve(cfg: Arc<ashkelon::config::Config>) -> anyhow::Result<()> {
    let addr = cfg.listen.clone().unwrap_or_else(|| "127.0.0.1:8484".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await.with_context(|| format!("binding {addr}"))?;
    let engine = ashkelon::hooks::Engine::new(cfg.clone());

    ashkelon::hooks::Engine::start(&engine);

    ashkelon::relay::serve(cfg, listener, engine).await
}

async fn run(cfg: Arc<ashkelon::config::Config>, harness: &str, no_channel: bool, args: &[String]) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("binding relay listener")?;
    let port = listener.local_addr()?.port();
    let launch = random_hex_id();
    let relay_base = format!("http://127.0.0.1:{port}/s/{launch}");
    tracing::debug!("relay base: {relay_base}");

    let engine = ashkelon::hooks::Engine::new(cfg.clone());

    ashkelon::hooks::Engine::start(&engine);

    let relay_engine = engine.clone();
    let relay_cfg = cfg.clone();
    let relay_task = tokio::spawn(async move {
        if let Err(e) = ashkelon::relay::serve(relay_cfg, listener, relay_engine).await {
            tracing::debug!("relay stopped: {e:#}");
        }
    });

    let options = ashkelon::launch::LaunchOptions { state_dir: cfg.state_dir(), no_channel };
    let mut plan = ashkelon::launch::plan(harness, &relay_base, &launch, args, &options)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut companion_child = None;
    if let Some(companion) = plan.companion.take() {
        let mut cmd = tokio::process::Command::new(&companion.program);
        cmd.args(&companion.args);
        for (key, value) in &companion.env {
            cmd.env(key, value);
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("spawning companion process {}", companion.program))?;
        let stdout = child.stdout.take().expect("companion stdout was piped");
        let stderr = child.stderr.take().expect("companion stderr was piped");
        let companion_url = wait_for_companion_ready(stdout, stderr, &companion.ready_pattern).await?;
        plan.resolve_companion_url(&companion_url);
        companion_child = Some(child);
    }

    engine.register_launch(&launch, plan.wake.clone(), cwd);

    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args);
    for key in &plan.env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in &plan.env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().with_context(|| format!("spawning harness process {}", plan.program))?;

    #[cfg(unix)]
    let _ignore_sigint = {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        tokio::spawn(async move {
            loop {
                if sigint.recv().await.is_none() {
                    break;
                }
            }
        })
    };

    let status = child.wait().await.context("waiting for harness process")?;

    if let Some(mut companion) = companion_child {
        let _ = companion.start_kill();
        let _ = companion.wait().await;
    }
    relay_task.abort();

    if let Some(overlay) = &plan.overlay_home {
        if let Err(e) = ashkelon::launch::reconcile_home_overlay(overlay) {
            tracing::debug!("reconciling home overlay: {e:#}");
        }
        let _ = std::fs::remove_dir_all(&overlay.overlay_dir);
    }
    for temp_file in &plan.temp_files {
        let _ = std::fs::remove_file(temp_file);
    }

    std::process::exit(status.code().unwrap_or(1));
}

type LineStream<T> = tokio::io::Lines<tokio::io::BufReader<T>>;

/// Races the companion's stdout and stderr for the readiness banner (a live-run finding put
/// opencode's on stdout, contrary to what its own `--print-logs` help text would suggest — so
/// this never assumes which stream carries it), then keeps draining both for the rest of the
/// companion's life so a full pipe buffer never makes it block on a write once nothing is
/// actively reading its logs anymore.
async fn wait_for_companion_ready(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    pattern: &regex::Regex,
) -> anyhow::Result<String> {
    use tokio::io::AsyncBufReadExt;

    let out_lines = tokio::io::BufReader::new(stdout).lines();
    let err_lines = tokio::io::BufReader::new(stderr).lines();

    let (url, out_lines, err_lines) = tokio::time::timeout(std::time::Duration::from_secs(10), find_ready_line(out_lines, err_lines, pattern))
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for companion process to report its address"))??;

    tokio::spawn(drain(out_lines));
    tokio::spawn(drain(err_lines));

    Ok(url)
}

async fn find_ready_line(
    mut out_lines: LineStream<tokio::process::ChildStdout>,
    mut err_lines: LineStream<tokio::process::ChildStderr>,
    pattern: &regex::Regex,
) -> anyhow::Result<(String, LineStream<tokio::process::ChildStdout>, LineStream<tokio::process::ChildStderr>)> {
    let mut out_done = false;
    let mut err_done = false;
    loop {
        if out_done && err_done {
            anyhow::bail!("companion process exited before reporting its address");
        }
        tokio::select! {
            line = out_lines.next_line(), if !out_done => {
                match line.context("reading companion stdout")? {
                    Some(line) => if let Some(url) = match_ready(pattern, &line) {
                        return Ok((url, out_lines, err_lines));
                    },
                    None => out_done = true,
                }
            }
            line = err_lines.next_line(), if !err_done => {
                match line.context("reading companion stderr")? {
                    Some(line) => if let Some(url) = match_ready(pattern, &line) {
                        return Ok((url, out_lines, err_lines));
                    },
                    None => err_done = true,
                }
            }
        }
    }
}

fn match_ready(pattern: &regex::Regex, line: &str) -> Option<String> {
    let caps = pattern.captures(line)?;
    Some(format!("http://{}:{}", &caps["host"], &caps["port"]))
}

async fn drain<T: tokio::io::AsyncRead + Unpin>(mut lines: LineStream<T>) {
    while matches!(lines.next_line().await, Ok(Some(_))) {}
}

async fn model(cfg: Arc<ashkelon::config::Config>, name: &str, system: Option<&str>) -> anyhow::Result<()> {
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt).context("reading prompt from stdin")?;
    let response = ashkelon::model::complete(&cfg, name, system, &prompt).await?;
    println!("{response}");
    Ok(())
}

fn random_hex_id() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    bytes[..4].iter().map(|b| format!("{b:02x}")).collect()
}
