//! Resource sampling of a child process from procfs.
//!
//! Values are sampled (every 250 ms by the subprocess runner), so CPU time is a
//! lower bound and a sub-interval spike can be missed. Peak RSS is exact up to
//! the last sample because the kernel's high-water mark (VmHWM) is monotonic.
//!
//! GPU memory comes from DRM fdinfo (`drm-resident-vram0` / `drm-total-vram0`),
//! which the xe, i915 and amdgpu drivers expose per open render node. This is
//! how the "no VRAM held after unload" requirement is checked: the daemon
//! itself never opens a render node, and a child's VRAM is gone once it exits.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Usage {
    /// Number of samples taken; zero means the process ended before the
    /// first sample and the other fields are unknown.
    pub samples: u32,
    pub peak_rss_kib: u64,
    /// User + system CPU time at the last sample, in milliseconds.
    pub cpu_time_ms: u64,
    /// Highest GPU memory seen across samples, in KiB.
    pub peak_vram_kib: u64,
}

pub struct Sampler {
    pid: Option<u32>,
    usage: Mutex<Usage>,
}

impl Sampler {
    pub fn new(pid: Option<u32>) -> Self {
        Self {
            pid,
            usage: Mutex::new(Usage::default()),
        }
    }

    pub fn sample(&self) {
        let Some(pid) = self.pid else { return };
        let Some(snap) = snapshot(pid) else { return };
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.samples += 1;
        u.peak_rss_kib = u.peak_rss_kib.max(snap.hwm_kib);
        u.cpu_time_ms = u.cpu_time_ms.max(snap.cpu_time_ms);
        u.peak_vram_kib = u.peak_vram_kib.max(snap.vram_kib);
    }

    pub fn finish(&self) -> Usage {
        self.usage.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

struct Snapshot {
    hwm_kib: u64,
    cpu_time_ms: u64,
    vram_kib: u64,
}

#[cfg(target_os = "linux")]
fn snapshot(pid: u32) -> Option<Snapshot> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let hwm_kib = status_field(&status, "VmHWM:").unwrap_or(0);
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(Snapshot {
        hwm_kib,
        cpu_time_ms: cpu_time_ms(&stat).unwrap_or(0),
        vram_kib: vram_kib(pid),
    })
}

#[cfg(not(target_os = "linux"))]
fn snapshot(_pid: u32) -> Option<Snapshot> {
    None
}

fn status_field(status: &str, name: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|l| l.strip_prefix(name))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn cpu_time_ms(stat: &str) -> Option<u64> {
    // Fields after the parenthesised comm: state is field 3, utime 14, stime 15.
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let mut f = rest.split_whitespace().skip(11);
    let utime: u64 = f.next()?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    Some((utime + stime) * 10) // USER_HZ = 100
}

/// Sums VRAM over distinct DRM clients of `pid`. Several fds can refer to the
/// same client, so values are keyed by `drm-client-id` to avoid double counts.
#[cfg(target_os = "linux")]
fn vram_kib(pid: u32) -> u64 {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fdinfo")) else {
        return 0;
    };
    let mut clients: HashMap<String, u64> = HashMap::new();
    for entry in dir.flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path())
            && let Some((client, kib)) = parse_drm_fdinfo(&text)
        {
            clients.insert(client, kib);
        }
    }
    clients.values().sum()
}

/// Returns `(client id, VRAM KiB)` for a DRM fdinfo, preferring resident
/// memory over the total allocation.
fn parse_drm_fdinfo(text: &str) -> Option<(String, u64)> {
    let mut client = None;
    let (mut resident, mut total) = (None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "drm-client-id" => client = Some(value.to_owned()),
            "drm-resident-vram0" => resident = parse_kib(value),
            "drm-total-vram0" => total = parse_kib(value),
            _ => {}
        }
    }
    Some((client?, resident.or(total)?))
}

/// fdinfo memory values look like `1234 KiB` or `56 MiB`.
fn parse_kib(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let n: u64 = parts.next()?.parse().ok()?;
    Some(match parts.next() {
        None => n / 1024,
        Some("KiB") => n,
        Some("MiB") => n * 1024,
        Some("GiB") => n * 1024 * 1024,
        Some(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xe_fdinfo() {
        let text = "pos:\t0\nflags:\t02100002\ndrm-driver:\txe\ndrm-client-id:\t42\n\
                    drm-total-vram0:\t1048576 KiB\ndrm-resident-vram0:\t524288 KiB\n";
        assert_eq!(parse_drm_fdinfo(text), Some(("42".into(), 524_288)));
    }

    #[test]
    fn non_drm_fds_are_ignored() {
        assert_eq!(parse_drm_fdinfo("pos:\t0\nflags:\t0100000\n"), None);
    }

    #[test]
    fn units_are_normalized_to_kib() {
        assert_eq!(parse_kib("3 MiB"), Some(3072));
        assert_eq!(parse_kib("2048"), Some(2));
        assert_eq!(parse_kib("1 TB"), None);
    }

    #[test]
    fn cpu_time_survives_spaces_in_comm() {
        let stat = "123 (my (weird) prog) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0";
        assert_eq!(cpu_time_ms(stat), Some(3000));
    }
}
