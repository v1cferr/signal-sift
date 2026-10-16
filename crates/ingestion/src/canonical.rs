//! URL canonicalization for deduplication.
//!
//! The same article reaches us through feed links, newsletter redirects and
//! share buttons, each decorated differently. Canonical form:
//!
//! * scheme and host lowercased (the `url` crate does this), default port and
//!   fragment dropped;
//! * tracking parameters removed (`utm_*`, click ids, share ids);
//! * remaining query parameters kept **in their original order**: some sites
//!   route on parameter order, and dedup only needs determinism, which the
//!   tracking-parameter removal already gives;
//! * YouTube video URLs of every shape become `https://www.youtube.com/watch?v=ID`.
//!
//! The path is left alone: a trailing slash or letter case can be meaningful
//! on some servers, and a false merge is worse than a missed one.

use signalsift_core::youtube::video_id_from_url;
use url::Url;

const TRACKING_EXACT: &[&str] = &[
    "fbclid", "gclid", "dclid", "msclkid", "mc_cid", "mc_eid", "igshid", "si", "ref_src",
    "ref_url", "_hsenc", "_hsmi", "yclid", "twclid",
];

pub fn canonicalize(input: &Url) -> Url {
    if let Some(id) = video_id_from_url(input) {
        return Url::parse(&format!("https://www.youtube.com/watch?v={id}")).expect("static URL");
    }
    let mut url = input.clone();
    url.set_fragment(None);
    if url.port() == url.port_or_known_default() {
        let _ = url.set_port(None);
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.starts_with("utm_") || TRACKING_EXACT.contains(&k.as_str()))
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> String {
        canonicalize(&Url::parse(s).unwrap()).to_string()
    }

    #[test]
    fn strips_tracking_fragment_and_default_port() {
        assert_eq!(
            c("HTTPS://Example.COM:443/Post?utm_source=x&id=7&fbclid=y#comments"),
            "https://example.com/Post?id=7"
        );
    }

    #[test]
    fn keeps_meaningful_query_in_order() {
        assert_eq!(c("https://e.com/p?b=2&a=1"), "https://e.com/p?b=2&a=1");
    }

    #[test]
    fn unifies_youtube_url_shapes() {
        for s in [
            "https://youtu.be/BZxHviFY_YY?si=abc",
            "https://m.youtube.com/watch?v=BZxHviFY_YY&feature=share",
            "https://www.youtube.com/shorts/BZxHviFY_YY",
        ] {
            assert_eq!(c(s), "https://www.youtube.com/watch?v=BZxHviFY_YY", "{s}");
        }
    }

    #[test]
    fn leaves_paths_alone() {
        assert_eq!(c("https://e.com/a/"), "https://e.com/a/");
        assert_ne!(c("https://e.com/a/"), c("https://e.com/a"));
    }
}
