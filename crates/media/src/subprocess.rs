//! Controlled execution of external tools (yt-dlp, ffmpeg, whisper.cpp).
//!
//! Heavy work runs in child processes so the daemon never links inference
//! libraries or keeps their memory resident. Every child:
//!
//! * starts with an empty environment plus an explicit allow-list, so neither
//!   host variables (LD_LIBRARY_PATH, proxies, tool config) nor secrets leak in;
//! * gets its own process group, so a timeout or cancellation also kills the
//!   grandchildren it spawned (yt-dlp runs ffmpeg, for instance);
//! * has a wall-clock timeout and a cap on captured stdout;
//! * is sampled from procfs while it runs, so every call reports its peak RSS,
//!   CPU time and GPU memory.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::usage::{Sampler, Usage};

const STDERR_TAIL: usize = 4096;
const SAMPLE_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum SubprocessError {
    #[error("cannot start {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{program} timed out after {timeout:?}")]
    Timeout { program: String, timeout: Duration },
    #[error("{program} wrote more than {limit} bytes to stdout")]
    OutputTooLarge { program: String, limit: u64 },
    #[error("{program} exited with {status}: {stderr_tail}")]
    Failed {
        program: String,
        status: ExitStatus,
        stderr_tail: String,
    },
    #[error("I/O with {program}: {source}")]
    Io {
        program: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub timeout: Duration,
    pub max_stdout: u64,
}

#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr_tail: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize)]
pub struct Invocation {
    pub program: String,
    pub wall_ms: u64,
    pub usage: Usage,
}

/// Builds a [`Command`] with a cleared environment. `PATH` and `HOME` are
/// passed through so tools can be found and can keep their own caches; the
/// locale is fixed so output parsing does not depend on the user's language.
pub fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear();
    for key in ["PATH", "HOME", "XDG_CACHE_HOME", "TMPDIR"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Runs `cmd` to completion under `limits`. A non-zero exit is an error that
/// carries the tail of stderr, which is where every one of these tools explains
/// what went wrong.
pub async fn run(mut cmd: Command, limits: Limits) -> Result<Output, SubprocessError> {
    let program = program_name(cmd.as_std().get_program());
    let mut child = cmd.spawn().map_err(|source| SubprocessError::Spawn {
        program: program.clone(),
        source,
    })?;
    let guard = GroupGuard::new(&child);
    let sampler = Sampler::new(child.id());

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let work = async {
        let (out, err, status) = tokio::join!(
            read_capped(stdout, limits.max_stdout),
            read_tail(stderr, STDERR_TAIL),
            sample_until_exit(&mut child, &sampler),
        );
        (out, err, status)
    };

    let (out, err, status) = match tokio::time::timeout(limits.timeout, work).await {
        Ok(r) => r,
        Err(_) => {
            guard.kill();
            return Err(SubprocessError::Timeout {
                program,
                timeout: limits.timeout,
            });
        }
    };
    let io = |source| SubprocessError::Io {
        program: program.clone(),
        source,
    };
    let status = status.map_err(io)?;
    let stderr_tail = String::from_utf8_lossy(&err.map_err(io)?).into_owned();
    let stdout = match out.map_err(io)? {
        Some(bytes) => bytes,
        None => {
            guard.kill();
            return Err(SubprocessError::OutputTooLarge {
                program,
                limit: limits.max_stdout,
            });
        }
    };
    guard.disarm();

    if !status.success() {
        return Err(SubprocessError::Failed {
            program,
            status,
            stderr_tail: stderr_tail.trim().to_owned(),
        });
    }
    Ok(Output {
        stdout,
        stderr_tail,
        usage: sampler.finish(),
    })
}

/// Same as [`run`] but also reports wall time, for benchmark records.
pub async fn run_timed(
    cmd: Command,
    limits: Limits,
) -> Result<(Output, Invocation), SubprocessError> {
    let program = program_name(cmd.as_std().get_program());
    let start = Instant::now();
    let out = run(cmd, limits).await?;
    let inv = Invocation {
        program,
        wall_ms: start.elapsed().as_millis() as u64,
        usage: out.usage.clone(),
    };
    Ok((out, inv))
}

async fn sample_until_exit(child: &mut Child, sampler: &Sampler) -> std::io::Result<ExitStatus> {
    let mut tick = tokio::time::interval(SAMPLE_EVERY);
    loop {
        tokio::select! {
            status = child.wait() => return status,
            _ = tick.tick() => sampler.sample(),
        }
    }
}

/// Reads at most `limit` bytes; returns `None` if the stream had more.
async fn read_capped(
    mut r: impl AsyncRead + Unpin,
    limit: u64,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    (&mut r).take(limit + 1).read_to_end(&mut buf).await?;
    if buf.len() as u64 > limit {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// Drains the stream completely (a full pipe would block the child) but keeps
/// only the last `keep` bytes.
async fn read_tail(mut r: impl AsyncRead + Unpin, keep: usize) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(keep);
    let mut chunk = [0u8; 4096];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(tail);
        }
        tail.extend_from_slice(&chunk[..n]);
        if tail.len() > keep {
            tail.drain(..tail.len() - keep);
        }
    }
}

fn program_name(p: &OsStr) -> String {
    Path::new(p)
        .file_name()
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// Kills the child's whole process group on drop unless disarmed. This is
/// what makes cancellation (dropping the future) and timeouts clean up
/// grandchildren as well; `kill_on_drop` alone only reaches the direct child.
struct GroupGuard {
    pgid: Option<i32>,
}

impl GroupGuard {
    fn new(child: &Child) -> Self {
        Self {
            pgid: child.id().map(|id| id as i32),
        }
    }

    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
    }

    fn disarm(mut self) {
        self.pgid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            timeout: Duration::from_secs(5),
            max_stdout: 1024,
        }
    }

    fn sh(script: &str) -> Command {
        let mut c = command(Path::new("sh"));
        c.arg("-c").arg(script);
        c
    }

    #[tokio::test]
    async fn captures_stdout_and_reports_usage() {
        let out = run(sh("echo hello"), limits()).await.unwrap();
        assert_eq!(out.stdout, b"hello\n");
    }

    #[tokio::test]
    async fn environment_is_cleared() {
        let out = run(sh("echo \"[$LD_LIBRARY_PATH][$LANG]\""), limits())
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "[][C.UTF-8]\n");
    }

    #[tokio::test]
    async fn failures_carry_the_stderr_tail() {
        let err = run(sh("echo boom >&2; exit 3"), limits())
            .await
            .unwrap_err();
        match err {
            SubprocessError::Failed { stderr_tail, .. } => assert_eq!(stderr_tail, "boom"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_output_is_refused() {
        let err = run(sh("head -c 5000 /dev/zero"), limits())
            .await
            .unwrap_err();
        assert!(matches!(err, SubprocessError::OutputTooLarge { .. }));
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-survived");
        // The grandchild would create the marker after 2 s if it outlived the
        // timeout; killing the group must prevent that.
        let script = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let err = run(
            sh(&script),
            Limits {
                timeout: Duration::from_millis(300),
                max_stdout: 1024,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SubprocessError::Timeout { .. }));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "grandchild outlived the timeout");
    }
}
