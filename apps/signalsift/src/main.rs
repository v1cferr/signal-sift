//! `signalsift` — daemon and command-line interface.

mod collect;
mod doctor;
mod media;
mod serve;
mod sources;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use signalsift_core::Config;

#[derive(Parser)]
#[command(version, about = "Local-first content intelligence daemon")]
struct Cli {
    /// Config file. Defaults to $XDG_CONFIG_HOME/signalsift/config.toml when it exists.
    #[arg(long, short, global = true, env = "SIGNALSIFT_CONFIG")]
    config: Option<PathBuf>,

    /// Log output format. `json` suits log shippers; journald prefers `text`.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: HTTP API plus background work, until SIGINT/SIGTERM.
    Serve,
    /// Print database and daemon status as JSON.
    Status,
    /// Check configuration, directories, external tools and the GPU.
    Doctor,
    /// Feed sources: add, import, list, poll.
    Sources {
        #[command(subcommand)]
        command: sources::SourcesCommand,
    },
    /// Video and podcast pipeline: captions, local transcription, segments.
    Media {
        #[command(subcommand)]
        command: media::MediaCommand,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.log_format);

    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let config = Config::load(cli.config.as_deref()).context("loading configuration")?;
    let runtime = build_runtime()?;
    match cli.command {
        Command::Serve => runtime
            .block_on(serve::run(config))
            .map(|()| ExitCode::SUCCESS),
        Command::Status => runtime.block_on(status(config)).map(|()| ExitCode::SUCCESS),
        Command::Doctor => runtime.block_on(doctor::run(&config)),
        Command::Sources { command } => runtime
            .block_on(sources::run(config, command))
            .map(|()| ExitCode::SUCCESS),
        Command::Media { command } => runtime
            .block_on(media::run(config, command))
            .map(|()| ExitCode::SUCCESS),
    }
}

/// A single-threaded runtime. The daemon's async work is I/O waiting; heavy
/// computation happens in child processes and SQLite calls go to the blocking
/// pool. One event-loop thread keeps idle RSS and wakeups to a minimum. The
/// blocking pool is capped and its threads exit after a short idle period, so
/// a burst of database work does not leave threads (and their stacks) behind.
fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .thread_keep_alive(Duration::from_secs(10))
        .build()
        .context("starting the async runtime")
}

fn init_logging(format: LogFormat) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("SIGNALSIFT_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn"));
    // Colors only on a terminal; under systemd stderr goes to the journal,
    // where escape codes are noise.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr);
    match format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}

/// Reads the database directly, so it works whether or not the daemon runs.
async fn status(config: Config) -> anyhow::Result<()> {
    let path = config.database_path()?;
    if !path.exists() {
        anyhow::bail!(
            "no database at {}; run `signalsift serve` once to create it",
            path.display()
        );
    }
    let db = signalsift_storage::Db::open(&path)?;
    let summary = db.summary().await?;
    let reachable = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(config.server.listen),
    )
    .await
    .is_ok_and(|r| r.is_ok());
    let out = serde_json::json!({
        "database_path": path,
        "database": summary,
        "daemon_listening": reachable,
        "listen": config.server.listen,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
