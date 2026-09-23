use std::path::PathBuf;

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
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Call a configured model (for hooks): prompt on stdin, response text on stdout.
    Model {
        name: String,
        #[arg(long)]
        system: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    let cli = Cli::parse();
    let _cfg = ashkelon::config::Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Serve | Command::Run { .. } | Command::Model { .. } => anyhow::bail!("not implemented"),
    }
}
