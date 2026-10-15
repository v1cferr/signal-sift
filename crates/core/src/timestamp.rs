//! Media timestamps and deep links into the original video.

use url::Url;

/// Formats milliseconds as `H:MM:SS` (or `M:SS` under an hour), the way video
/// players display positions. Sub-second precision is dropped on purpose: it is
/// noise for a human jumping to a moment.
pub fn format_hms(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Builds a link that opens a YouTube video at `start_ms`.
///
/// Returns `None` when `video_id` does not look like a YouTube id, so callers
/// never emit a link built from untrusted text. The second is rounded *down*:
/// landing a moment early is harmless, landing late cuts the start of the topic.
pub fn youtube_link(video_id: &str, start_ms: u64) -> Option<Url> {
    if !is_youtube_video_id(video_id) {
        return None;
    }
    let mut url = Url::parse("https://www.youtube.com/watch").ok()?;
    url.query_pairs_mut()
        .append_pair("v", video_id)
        .append_pair("t", &format!("{}s", start_ms / 1000));
    Some(url)
}

/// YouTube video ids are 11 characters from the URL-safe base64 alphabet.
pub fn is_youtube_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Parses a `YYYYMMDD` date (yt-dlp's `upload_date`) as midnight UTC.
pub fn yyyymmdd_to_unix_ms(s: &str) -> Option<i64> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: u32 = s[4..6].parse().ok()?;
    let d: u32 = s[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400_000)
}

/// Formats Unix milliseconds as RFC 3339 UTC with second precision.
pub fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// Howard Hinnant's civil-calendar algorithms: exact for the proleptic
// Gregorian calendar, no lookup tables, no dependency for two conversions.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upload_dates() {
        assert_eq!(yyyymmdd_to_unix_ms("19700101"), Some(0));
        assert_eq!(yyyymmdd_to_unix_ms("20261015"), Some(1_792_022_400_000));
        assert_eq!(yyyymmdd_to_unix_ms("2026-10-15"), None);
        assert_eq!(yyyymmdd_to_unix_ms("20261315"), None);
    }

    #[test]
    fn formats_rfc3339_round_trip() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_792_022_400_000), "2026-10-15T00:00:00Z");
        assert_eq!(rfc3339(951_782_400_000 + 3_723_000), "2000-02-29T01:02:03Z");
    }

    #[test]
    fn formats_positions_like_a_player() {
        assert_eq!(format_hms(0), "0:00");
        assert_eq!(format_hms(65_999), "1:05");
        assert_eq!(format_hms(3_600_000), "1:00:00");
        assert_eq!(format_hms(7_384_500), "2:03:04");
    }

    #[test]
    fn links_round_down_to_the_second() {
        let url = youtube_link("N3eU-2VIByQ", 754_999).unwrap();
        assert_eq!(
            url.as_str(),
            "https://www.youtube.com/watch?v=N3eU-2VIByQ&t=754s"
        );
    }

    #[test]
    fn rejects_ids_that_could_inject_query_parameters() {
        assert!(youtube_link("abc&list=xyz", 0).is_none());
        assert!(youtube_link("short", 0).is_none());
        assert!(youtube_link("N3eU-2VIBy?", 0).is_none());
    }
}
