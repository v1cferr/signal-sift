//! Self-measurement from procfs, so resource targets can be checked from the
//! API itself instead of trusting an external tool's sampling.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProcStats {
    /// Resident set size (VmRSS) in KiB.
    pub rss_kib: u64,
    /// Peak resident set size (VmHWM) in KiB.
    pub rss_peak_kib: u64,
    pub threads: u64,
    /// User + system CPU time consumed since start, in milliseconds.
    pub cpu_time_ms: u64,
}

#[cfg(target_os = "linux")]
pub fn read_self_stats() -> Option<ProcStats> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    // /proc/self/stat fields 14 and 15 (utime, stime) are in clock ticks. The
    // comm field (2) may contain spaces, so split after its closing paren.
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let mut f = rest.split_whitespace().skip(11);
    let utime: u64 = f.next()?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    // USER_HZ is 100 on every Linux ABI Rust targets; reading sysconf would
    // need libc for a constant that does not vary in practice.
    const TICKS_PER_SEC: u64 = 100;
    Some(ProcStats {
        rss_kib: field("VmRSS:")?,
        rss_peak_kib: field("VmHWM:")?,
        threads: field("Threads:")?,
        cpu_time_ms: (utime + stime) * 1000 / TICKS_PER_SEC,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn read_self_stats() -> Option<ProcStats> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn reads_plausible_values() {
        let s = super::read_self_stats().expect("procfs available");
        assert!(s.rss_kib > 0);
        assert!(s.rss_peak_kib >= s.rss_kib);
        assert!(s.threads >= 1);
    }
}
