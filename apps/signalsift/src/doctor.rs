//! `signalsift doctor`: report what this machine can and cannot do.
//!
//! Every check prints one line and never aborts the others, so a single run
//! shows the whole picture. Missing optional tools are warnings; only problems
//! that stop the daemon from serving count as failures.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use signalsift_core::Config;
use tokio::process::Command;

enum Level {
    Ok,
    Warn,
    Fail,
}

struct Report {
    failures: usize,
}

impl Report {
    fn line(&mut self, level: Level, what: &str, detail: impl AsRef<str>) {
        let tag = match level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => {
                self.failures += 1;
                "FAIL"
            }
        };
        println!("[{tag}] {what:<16} {}", detail.as_ref());
    }
}

pub async fn run(config: &Config) -> anyhow::Result<ExitCode> {
    let mut r = Report { failures: 0 };

    r.line(
        Level::Ok,
        "config",
        format!(
            "listen {}, profile {}, heavy concurrency {}",
            config.server.listen, config.runtime.profile, config.runtime.heavy_concurrency
        ),
    );

    for (name, dir) in [
        ("state dir", config.state_dir()),
        ("cache dir", config.cache_dir()),
    ] {
        match dir {
            Ok(dir) => match check_writable(&dir) {
                Ok(()) => r.line(Level::Ok, name, dir.display().to_string()),
                Err(e) => r.line(Level::Fail, name, format!("{}: {e}", dir.display())),
            },
            Err(e) => r.line(Level::Fail, name, e.to_string()),
        }
    }

    match config.database_path() {
        Ok(path) => match signalsift_storage::Db::open(&path) {
            Ok(db) => match db.summary().await {
                Ok(s) => r.line(
                    Level::Ok,
                    "database",
                    format!("{} (schema {})", path.display(), s.schema_version),
                ),
                Err(e) => r.line(Level::Fail, "database", e.to_string()),
            },
            Err(e) => r.line(Level::Fail, "database", format!("{}: {e}", path.display())),
        },
        Err(e) => r.line(Level::Fail, "database", e.to_string()),
    }

    match tokio::net::TcpListener::bind(config.server.listen).await {
        Ok(_) => r.line(Level::Ok, "listen address", "free"),
        Err(e) => r.line(
            Level::Warn,
            "listen address",
            format!(
                "{}: {e} (is the daemon already running?)",
                config.server.listen
            ),
        ),
    }

    // Tools are optional per feature: subtitles need yt-dlp, the audio
    // fallback needs ffmpeg and whisper.cpp, analysis needs llama.cpp.
    let tools: [(&str, &Path, &str, &str); 4] = [
        (
            "yt-dlp",
            &config.media.yt_dlp,
            "--version",
            "subtitles and metadata",
        ),
        (
            "ffmpeg",
            &config.media.ffmpeg,
            "-version",
            "audio extraction",
        ),
        (
            "whisper-cli",
            Path::new("whisper-cli"),
            "--help",
            "local speech recognition",
        ),
        (
            "llama-server",
            Path::new("llama-server"),
            "--version",
            "local LLM",
        ),
    ];
    for (name, program, arg, purpose) in tools {
        match probe(program, arg).await {
            Some(v) => r.line(Level::Ok, name, v),
            None => r.line(
                Level::Warn,
                name,
                format!("not found; needed for {purpose} (try `nix develop .#inference`)"),
            ),
        }
    }

    // Models are optional; a configured path that does not exist is a
    // mistake worth flagging loudly, an unset one is a feature left off.
    let models: [(&str, Option<&Path>, &str); 3] = [
        ("asr model", config.asr.model.as_deref(), "asr.model"),
        (
            "vad model",
            config.asr.vad_model.as_deref(),
            "asr.vad_model",
        ),
        (
            "llm model",
            config.analysis.model.as_deref(),
            "analysis.model",
        ),
    ];
    for (name, path, key) in models {
        match path {
            Some(p) if p.is_file() => r.line(Level::Ok, name, p.display().to_string()),
            Some(p) => r.line(
                Level::Fail,
                name,
                format!("{key} = {} does not exist", p.display()),
            ),
            None => r.line(
                Level::Warn,
                name,
                format!("{key} not set; feature disabled"),
            ),
        }
    }
    if config.analysis.model.is_some() {
        match config.analysis.interests_path() {
            Some(p) if p.is_file() => match signalsift_analysis::InterestProfile::load(&p) {
                Ok(profile) => r.line(
                    Level::Ok,
                    "interests",
                    format!("{} ({} topics)", p.display(), profile.topics.len()),
                ),
                Err(e) => r.line(Level::Fail, "interests", e.to_string()),
            },
            Some(p) => r.line(
                Level::Fail,
                "interests",
                format!(
                    "{} missing; copy config/interests.example.toml",
                    p.display()
                ),
            ),
            None => r.line(
                Level::Fail,
                "interests",
                "cannot resolve the interests file path",
            ),
        }
    }

    let render_nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("renderD"))
                .collect()
        })
        .unwrap_or_default();
    if render_nodes.is_empty() {
        r.line(
            Level::Warn,
            "gpu",
            "no /dev/dri render node; inference will run on CPU",
        );
    } else {
        r.line(
            Level::Ok,
            "gpu",
            format!("render nodes: {}", render_nodes.join(", ")),
        );
    }

    if r.failures == 0 {
        println!("\nno blocking problems found");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("\n{} blocking problem(s)", r.failures);
        Ok(ExitCode::FAILURE)
    }
}

fn check_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".doctor-write-test");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(probe)
}

/// Runs `program arg` with a short timeout and returns the first output line.
async fn probe(program: &Path, arg: &str) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(program).arg(arg).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = if out.stdout.is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    String::from_utf8_lossy(&text)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(80).collect())
}
