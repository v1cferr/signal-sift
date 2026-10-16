//! Long-running child processes (e.g. `llama-server`) that the daemon starts
//! for a batch of work and stops afterwards.
//!
//! Dropping a [`Service`] kills its whole process group, so a cancelled or
//! panicking caller can never leave a model loaded in RAM or VRAM.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::usage::{Sampler, Usage};

pub struct Service {
    name: String,
    child: Child,
    pgid: Option<i32>,
    sampler: Sampler,
}

impl Service {
    /// Spawns `cmd` in its own process group with stdio discarded (services
    /// log to their own stderr, which nobody reads; failures surface through
    /// their health endpoint or exit status instead).
    pub fn spawn(name: &str, mut cmd: Command) -> std::io::Result<Self> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let pid = child.id();
        Ok(Self {
            name: name.to_owned(),
            pgid: pid.map(|p| p as i32),
            sampler: Sampler::new(pid),
            child,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// `Some(status)` if the process has already exited.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    pub fn sample(&self) {
        self.sampler.sample();
    }

    pub fn usage(&self) -> Usage {
        self.sampler.finish()
    }

    /// SIGTERM to the group, then SIGKILL after `grace`.
    pub async fn stop(mut self, grace: Duration) -> Usage {
        self.sampler.sample();
        self.signal(nix::sys::signal::Signal::SIGTERM);
        if tokio::time::timeout(grace, self.child.wait())
            .await
            .is_err()
        {
            self.signal(nix::sys::signal::Signal::SIGKILL);
            let _ = self.child.wait().await;
        }
        self.pgid = None;
        self.sampler.finish()
    }

    fn signal(&self, sig: nix::sys::signal::Signal) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), sig);
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.signal(nix::sys::signal::Signal::SIGKILL);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::subprocess::command;
    use std::path::Path;

    #[tokio::test]
    async fn stop_terminates_the_group() {
        let mut cmd = command(Path::new("sh"));
        cmd.arg("-c").arg("sleep 30 & sleep 30");
        let mut svc = Service::spawn("sleeper", cmd).unwrap();
        assert!(svc.exited().is_none());
        svc.stop(Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn drop_kills_the_service() {
        let mut cmd = command(Path::new("sleep"));
        cmd.arg("30");
        let svc = Service::spawn("sleeper", cmd).unwrap();
        let pid = svc.child.id().unwrap();
        drop(svc);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        // Either reaped or a zombie waiting for the runtime; never running.
        assert!(
            status.is_empty() || status.contains("State:\tZ"),
            "{status}"
        );
    }
}
