//! Segment and item analysis on top of any [`InferenceProvider`].

use crate::interests::InterestProfile;
use crate::prompt::{
    self, ContentContext, InvalidOutput, ItemSummary, SegmentAnalysis, item_schema, segment_schema,
};
use crate::provider::{ChatRequest, ChatStats, InferenceError, InferenceProvider};

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Inference(#[from] InferenceError),
    #[error(transparent)]
    Output(#[from] InvalidOutput),
}

pub struct Analyzer<'a, P> {
    pub provider: &'a P,
    pub profile: &'a InterestProfile,
    /// `None`: write in the content's own language.
    pub language: Option<&'a str>,
}

impl<P: InferenceProvider> Analyzer<'_, P> {
    /// Scores one segment. Output that fails validation is retried once
    /// with a little temperature (a different sample); a second failure is an
    /// error, recorded by the caller, never a guessed score.
    pub async fn segment(
        &self,
        ctx: &ContentContext<'_>,
        text: &str,
    ) -> Result<(SegmentAnalysis, ChatStats), AnalysisError> {
        let mut req = ChatRequest {
            system: prompt::segment_system_prompt(self.profile, self.language),
            user: prompt::wrap_content(ctx, text),
            schema: segment_schema(),
            schema_name: "segment_analysis",
            max_tokens: 700,
            temperature: 0.0,
        };
        let mut last = None;
        for _ in 0..2 {
            let res = self.provider.chat(&req).await?;
            match prompt::parse_segment(&res.content, self.profile) {
                Ok(a) => return Ok((a, res.stats)),
                Err(e) => {
                    tracing::warn!("invalid segment analysis, retrying: {e}");
                    last = Some(e);
                    req.temperature = 0.3;
                }
            }
        }
        Err(last.expect("loop ran").into())
    }

    /// Summarizes an item from its segment summaries (not its full text), so
    /// the call stays small however long the media is.
    pub async fn item(
        &self,
        ctx: &ContentContext<'_>,
        parts: &[(Option<String>, &str)],
    ) -> Result<(ItemSummary, ChatStats), AnalysisError> {
        let listing: String = parts
            .iter()
            .map(|(pos, s)| match pos {
                Some(p) => format!("[{p}] {s}\n"),
                None => format!("- {s}\n"),
            })
            .collect();
        let mut req = ChatRequest {
            system: prompt::item_system_prompt(self.language),
            user: prompt::wrap_content(ctx, &listing),
            schema: item_schema(),
            schema_name: "item_summary",
            max_tokens: 500,
            temperature: 0.0,
        };
        let mut last = None;
        for _ in 0..2 {
            let res = self.provider.chat(&req).await?;
            match prompt::parse_item(&res.content) {
                Ok(s) => return Ok((s, res.stats)),
                Err(e) => {
                    last = Some(e);
                    req.temperature = 0.3;
                }
            }
        }
        Err(last.expect("loop ran").into())
    }
}

/// Item relevance is the *maximum* over its segments, computed here rather
/// than asked of the model: a five-minute discussion that matters must not be
/// averaged away by four hours that do not.
pub fn item_relevance(segments: &[SegmentAnalysis]) -> Option<(usize, u8)> {
    segments
        .iter()
        .enumerate()
        .max_by_key(|(i, s)| (s.relevance, std::cmp::Reverse(*i)))
        .map(|(i, s)| (i, s.relevance))
}

/// Splits article text into parts of at most `max_chars`, on paragraph
/// boundaries where possible, so long posts are scored piece by piece like
/// long videos.
pub fn split_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for para in text.split('\n').map(str::trim).filter(|p| !p.is_empty()) {
        if !current.is_empty() && current.chars().count() + para.chars().count() + 1 > max_chars {
            parts.push(std::mem::take(&mut current));
        }
        if para.chars().count() > max_chars {
            // A single huge paragraph: cut it at word boundaries.
            for word in para.split_whitespace() {
                if current.chars().count() + word.chars().count() + 1 > max_chars
                    && !current.is_empty()
                {
                    parts.push(std::mem::take(&mut current));
                }
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
            continue;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(para);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::provider::ChatResponse;

    /// Replays canned replies in order.
    struct Scripted {
        replies: Mutex<Vec<String>>,
    }

    impl InferenceProvider for Scripted {
        fn model_id(&self) -> &str {
            "scripted"
        }
        async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
            let content = self.replies.lock().unwrap().remove(0);
            Ok(ChatResponse {
                content,
                stats: ChatStats::default(),
            })
        }
    }

    fn profile() -> InterestProfile {
        toml::from_str("[[topic]]\nname = \"Rust\"\n").unwrap()
    }

    const GOOD: &str = r#"{"summary":"s","topics":[],"matched_interests":["Rust"],"relevance":70,
        "rationale":"r","applications":[],"mentions":[]}"#;

    fn ctx() -> ContentContext<'static> {
        ContentContext {
            title: "t",
            source: None,
            kind: "video",
            position: None,
        }
    }

    #[tokio::test]
    async fn invalid_output_is_retried_once() {
        let p = Scripted {
            replies: Mutex::new(vec!["not json".into(), GOOD.into()]),
        };
        let profile = profile();
        let a = Analyzer {
            provider: &p,
            profile: &profile,
            language: None,
        };
        let (seg, _) = a.segment(&ctx(), "text").await.unwrap();
        assert_eq!(seg.relevance, 70);
    }

    #[tokio::test]
    async fn two_invalid_outputs_are_an_error_not_a_guess() {
        let p = Scripted {
            replies: Mutex::new(vec!["nope".into(), "still nope".into()]),
        };
        let profile = profile();
        let a = Analyzer {
            provider: &p,
            profile: &profile,
            language: None,
        };
        assert!(matches!(
            a.segment(&ctx(), "text").await,
            Err(AnalysisError::Output(_))
        ));
    }

    #[test]
    fn item_relevance_is_the_best_segment() {
        let seg = |r| SegmentAnalysis {
            summary: "s".into(),
            topics: vec![],
            matched_interests: vec![],
            relevance: r,
            rationale: "r".into(),
            applications: vec![],
            mentions: vec![],
        };
        // Mostly irrelevant, one strong segment: the item scores 92, not ~25.
        let segs = vec![seg(5), seg(10), seg(92), seg(3), seg(92)];
        assert_eq!(item_relevance(&segs), Some((2, 92)));
        assert_eq!(item_relevance(&[]), None);
    }

    #[test]
    fn splits_on_paragraphs_and_cuts_giant_ones() {
        let text = format!("a para\n\nb para\n{}\nc", "w ".repeat(40));
        let parts = split_text(&text, 30);
        assert!(parts.iter().all(|p| p.chars().count() <= 30), "{parts:?}");
        assert_eq!(
            parts.join(" ").split_whitespace().count(),
            text.split_whitespace().count()
        );
    }
}
