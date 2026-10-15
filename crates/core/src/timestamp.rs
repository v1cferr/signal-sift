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

#[cfg(test)]
mod tests {
    use super::*;

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
