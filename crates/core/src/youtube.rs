//! YouTube video ids and URLs. Pure parsing, shared by ingestion (feed
//! canonicalization), media (yt-dlp input) and the API (queueing a video).

use url::Url;

use crate::timestamp::is_youtube_video_id;

/// Extracts the video id from a URL or bare id. Only YouTube hosts are
/// accepted; anything else returns `None`.
pub fn video_id(input: &str) -> Option<String> {
    let input = input.trim();
    if is_youtube_video_id(input) {
        return Some(input.to_owned());
    }
    video_id_from_url(&Url::parse(input).ok()?)
}

pub fn video_id_from_url(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let id = match host {
        "youtu.be" => url.path_segments()?.next()?.to_owned(),
        "youtube.com" | "music.youtube.com" => {
            let mut segs = url.path_segments()?;
            match segs.next()? {
                "watch" => url.query_pairs().find(|(k, _)| k == "v")?.1.into_owned(),
                "shorts" | "live" | "embed" | "v" => segs.next()?.to_owned(),
                _ => return None,
            }
        }
        _ => return None,
    };
    is_youtube_video_id(&id).then_some(id)
}

/// The canonical watch URL. `id` must already be validated.
pub fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ids_from_common_url_shapes() {
        for input in [
            "N3eU-2VIByQ",
            "https://youtu.be/N3eU-2VIByQ",
            "https://youtu.be/N3eU-2VIByQ?si=tracking",
            "https://www.youtube.com/watch?v=N3eU-2VIByQ&t=120s",
            "https://m.youtube.com/watch?v=N3eU-2VIByQ",
            "https://www.youtube.com/live/N3eU-2VIByQ",
            "https://www.youtube.com/shorts/N3eU-2VIByQ",
        ] {
            assert_eq!(video_id(input).as_deref(), Some("N3eU-2VIByQ"), "{input}");
        }
    }

    #[test]
    fn rejects_other_hosts_and_injection_attempts() {
        for input in [
            "https://evil.example/watch?v=N3eU-2VIByQ",
            "http://169.254.169.254/latest/meta-data",
            "--exec=rm -rf ~",
            "file:///etc/passwd",
            "https://www.youtube.com/watch?v=N3eU-2VIByQ%26list%3Dx",
            "https://www.youtube.com/@GutoGalego",
        ] {
            assert_eq!(video_id(input), None, "{input}");
        }
    }
}
