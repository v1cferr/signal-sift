//! RSS, Atom and JSON Feed parsing into SignalSift entries.

use signalsift_core::links::extract_urls;
use url::Url;

use crate::canonical::canonicalize;
use crate::html::to_text_and_links;

/// Text kept per entry. Full-content feeds can carry whole books' worth of
/// HTML; summaries and relevance need far less, and the original stays one
/// click away.
const MAX_TEXT_CHARS: usize = 100_000;

#[derive(Debug, Clone)]
pub struct Entry {
    pub url: Url,
    /// `video` for YouTube entries, `article` otherwise.
    pub kind: &'static str,
    /// Platform id when globally unique (YouTube video id). Article GUIDs are
    /// only unique per feed, so they are not used for cross-source dedup.
    pub external_id: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub published_ms: Option<i64>,
    pub text: Option<String>,
    pub links: Vec<Url>,
}

#[derive(Debug)]
pub struct ParsedFeed {
    pub title: Option<String>,
    pub entries: Vec<Entry>,
    /// Entries skipped because they had no usable http(s) link.
    pub skipped: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot parse feed: {0}")]
pub struct FeedError(#[from] feed_rs::parser::ParseFeedError);

pub fn parse(bytes: &[u8], feed_url: &Url) -> Result<ParsedFeed, FeedError> {
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(feed_url.as_str()))
        .build()
        .parse(bytes)?;
    let mut entries = Vec::with_capacity(feed.entries.len());
    let mut skipped = 0;
    for e in feed.entries {
        let Some(link) = e
            .links
            .iter()
            .filter(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
            .chain(e.links.iter())
            .find_map(|l| feed_url.join(&l.href).ok())
            .filter(|u| matches!(u.scheme(), "http" | "https"))
        else {
            skipped += 1;
            continue;
        };
        let url = canonicalize(&link);
        let youtube_id =
            e.id.strip_prefix("yt:video:")
                .map(str::to_owned)
                .or_else(|| {
                    url.query_pairs()
                        .find(|(k, _)| k == "v")
                        .map(|(_, v)| v.into_owned())
                })
                .filter(|_| url.host_str() == Some("www.youtube.com"));

        // Prefer full content, then summary, then media description (YouTube).
        let html = e
            .content
            .as_ref()
            .and_then(|c| c.body.clone())
            .or_else(|| e.summary.as_ref().map(|s| s.content.clone()))
            .or_else(|| {
                e.media
                    .iter()
                    .find_map(|m| m.description.as_ref().map(|d| d.content.clone()))
            });
        let (text, links) = match html {
            Some(h) => {
                let (t, mut l) = to_text_and_links(&h, Some(&link));
                // Plain-text descriptions (YouTube) carry bare URLs, not <a> tags.
                for u in extract_urls(&t) {
                    if !l.contains(&u) {
                        l.push(u);
                    }
                }
                (Some(truncate(t)), l)
            }
            None => (None, Vec::new()),
        };

        entries.push(Entry {
            kind: if youtube_id.is_some() {
                "video"
            } else {
                "article"
            },
            external_id: youtube_id,
            title: e
                .title
                .map(|t| crate::html::decode_entities(&t.content).trim().to_owned())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| url.to_string()),
            author: e.authors.first().and_then(|p| p.name.clone()),
            published_ms: e.published.or(e.updated).map(|d| d.timestamp_millis()),
            text: text.filter(|t| !t.is_empty()),
            links,
            url,
        });
    }
    Ok(ParsedFeed {
        title: feed.title.map(|t| t.content),
        entries,
        skipped,
    })
}

fn truncate(mut s: String) -> String {
    if let Some((idx, _)) = s.char_indices().nth(MAX_TEXT_CHARS) {
        s.truncate(idx);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_youtube_channel_feeds_as_videos() {
        let xml = include_bytes!("../../../tests/fixtures/feeds/youtube-channel.xml");
        let url = Url::parse(
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCLW51-XEzuOm5RwPMChHBMw",
        )
        .unwrap();
        let feed = parse(xml, &url).unwrap();
        let first = &feed.entries[0];
        assert_eq!(first.kind, "video");
        assert_eq!(first.external_id.as_deref(), Some("BZxHviFY_YY"));
        assert_eq!(
            first.url.as_str(),
            "https://www.youtube.com/watch?v=BZxHviFY_YY"
        );
        assert!(first.published_ms.is_some());
    }

    #[test]
    fn parses_rss_articles_with_text_and_links() {
        let xml = include_bytes!("../../../tests/fixtures/feeds/blog-rss.xml");
        let url = Url::parse("https://example.org/index.xml").unwrap();
        let feed = parse(xml, &url).unwrap();
        assert_eq!(feed.entries.len(), 2);
        assert_eq!(feed.skipped, 1);
        let e = &feed.entries[0];
        assert_eq!(e.kind, "article");
        assert_eq!(e.external_id, None);
        assert_eq!(e.url.as_str(), "https://example.org/posts/rust-on-nixos/");
        assert!(e.text.as_deref().unwrap().contains("flake"));
        assert!(!e.text.as_deref().unwrap().contains("<"));
        assert_eq!(e.links[0].as_str(), "https://nixos.org/manual/");
    }

    #[test]
    fn parses_json_feed() {
        let json = br#"{"version":"https://jsonfeed.org/version/1.1","title":"J",
            "items":[{"id":"1","url":"https://example.org/a?utm_source=x","title":"A",
                      "content_text":"plain","date_published":"2026-10-01T10:00:00Z"}]}"#;
        let feed = parse(json, &Url::parse("https://example.org/feed.json").unwrap()).unwrap();
        assert_eq!(feed.entries[0].url.as_str(), "https://example.org/a");
        assert_eq!(feed.entries[0].text.as_deref(), Some("plain"));
    }
}
