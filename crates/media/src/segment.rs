//! Splitting a transcript into segments: the units scored for relevance.
//!
//! Without chapters, segments are time windows cut at pauses. This is a
//! deliberately simple first step: it already lets a five-minute discussion
//! inside a four-hour podcast be scored on its own, and a later topic-aware
//! pass (LLM-based) can merge adjacent windows that share a subject. Windows
//! are small enough that a topic change rarely hides inside one.

use serde::Serialize;

use crate::cue::Cue;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Chapter title when the segment comes from a creator-provided chapter.
    pub title: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}

#[derive(Debug, Clone, Copy)]
pub struct WindowPolicy {
    /// A window closes at the first good pause after this length...
    pub target_ms: u64,
    /// ...and is force-closed at this length if no pause shows up.
    pub max_ms: u64,
    /// Gap between cues that counts as a pause.
    pub pause_ms: u64,
}

impl Default for WindowPolicy {
    fn default() -> Self {
        Self {
            target_ms: 180_000,
            max_ms: 300_000,
            pause_ms: 700,
        }
    }
}

/// Segments by chapters when the creator provided them (they are topic
/// boundaries chosen by a human), otherwise by pause-aligned windows.
pub fn segment(cues: &[Cue], chapters: &[Chapter], policy: WindowPolicy) -> Vec<Segment> {
    if chapters.len() >= 2 {
        by_chapters(cues, chapters)
    } else {
        by_windows(cues, policy)
    }
}

fn by_chapters(cues: &[Cue], chapters: &[Chapter]) -> Vec<Segment> {
    chapters
        .iter()
        .filter_map(|ch| {
            let text = join(
                cues.iter()
                    .filter(|c| c.start_ms >= ch.start_ms && c.start_ms < ch.end_ms),
            );
            // A chapter with no transcribed speech (partial coverage, music)
            // has nothing to score; skip it instead of inventing content.
            (!text.is_empty()).then(|| Segment {
                start_ms: ch.start_ms,
                end_ms: ch.end_ms,
                title: Some(ch.title.clone()),
                text,
            })
        })
        .collect()
}

fn by_windows(cues: &[Cue], policy: WindowPolicy) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..cues.len() {
        let len = cues[i].end_ms.saturating_sub(cues[start].start_ms);
        let gap = cues
            .get(i + 1)
            .map(|n| n.start_ms.saturating_sub(cues[i].end_ms))
            .unwrap_or(u64::MAX);
        let last = i + 1 == cues.len();
        if last || (len >= policy.target_ms && gap >= policy.pause_ms) || len >= policy.max_ms {
            out.push(Segment {
                start_ms: cues[start].start_ms,
                end_ms: cues[i].end_ms,
                title: None,
                text: join(cues[start..=i].iter()),
            });
            start = i + 1;
        }
    }
    out
}

fn join<'a>(cues: impl Iterator<Item = &'a Cue>) -> String {
    cues.map(|c| c.text.as_str()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 4-second cue every 5 seconds, i.e. a 1-second pause between cues.
    fn speech(seconds: u64) -> Vec<Cue> {
        (0..seconds / 5)
            .map(|i| Cue {
                start_ms: i * 5000,
                end_ms: i * 5000 + 4000,
                text: format!("w{i}"),
            })
            .collect()
    }

    #[test]
    fn windows_cover_every_cue_exactly_once() {
        let cues = speech(1000);
        let segs = segment(&cues, &[], WindowPolicy::default());
        let words: usize = segs.iter().map(|s| s.text.split(' ').count()).sum();
        assert_eq!(words, cues.len());
        for pair in segs.windows(2) {
            assert!(pair[0].end_ms <= pair[1].start_ms);
        }
    }

    #[test]
    fn windows_close_near_the_target_at_a_pause() {
        let segs = segment(&speech(1000), &[], WindowPolicy::default());
        let first = &segs[0];
        assert!(first.end_ms - first.start_ms >= 180_000);
        assert!(first.end_ms - first.start_ms < 190_000);
    }

    #[test]
    fn windows_are_force_closed_without_pauses() {
        let cues: Vec<Cue> = (0..200)
            .map(|i| Cue {
                start_ms: i * 5000,
                end_ms: (i + 1) * 5000,
                text: "x".into(),
            })
            .collect();
        let segs = segment(&cues, &[], WindowPolicy::default());
        assert!(segs.iter().all(|s| s.end_ms - s.start_ms <= 300_000));
    }

    #[test]
    fn chapters_win_and_empty_chapters_are_skipped() {
        let cues = speech(100);
        let chapters = vec![
            Chapter {
                start_ms: 0,
                end_ms: 50_000,
                title: "Intro".into(),
            },
            Chapter {
                start_ms: 50_000,
                end_ms: 100_000,
                title: "Rust".into(),
            },
            Chapter {
                start_ms: 100_000,
                end_ms: 200_000,
                title: "Not transcribed".into(),
            },
        ];
        let segs = segment(&cues, &chapters, WindowPolicy::default());
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].title.as_deref(), Some("Rust"));
        assert!(segs[1].text.starts_with("w10"));
    }
}
