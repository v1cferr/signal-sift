//! Timed text cues and their normalization.
//!
//! Every source of timed text (manual subtitles, YouTube's rolling automatic
//! captions, whisper.cpp output) is reduced to the same shape: non-overlapping,
//! start-ordered cues with absolute millisecond positions and clean text.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Annotations that describe sound rather than speech. They are dropped from
/// the text; the gap they leave is what segmentation uses to find pauses.
const NON_SPEECH: &[&str] = &[
    "[música]",
    "[music]",
    "[aplausos]",
    "[applause]",
    "[risos]",
    "[laughter]",
    "[silêncio]",
    "[silence]",
    "[blank_audio]",
    "[inaudível]",
];

/// Known whisper hallucinations on silence or music in Portuguese and English
/// models: credits from subtitle communities seen in the training data. They
/// are never real speech in our sources, so they are dropped outright.
const HALLUCINATIONS: &[&str] = &[
    "legendas pela comunidade amara.org",
    "legendas pela comunidade do amara.org",
    "subtitles by the amara.org community",
];

/// Cleans and orders cues:
///
/// * collapses whitespace and drops non-speech annotations and known
///   hallucinations;
/// * sorts by start and clamps each end to the next start, which turns rolling
///   captions (whose display windows overlap) into a partition of the timeline;
/// * merges consecutive cues with identical text, which rolling captions and
///   chunk overlaps both produce.
pub fn normalize(mut cues: Vec<Cue>) -> Vec<Cue> {
    for c in &mut cues {
        c.text = clean_text(&c.text);
    }
    cues.retain(|c| !c.text.is_empty());
    cues.sort_by_key(|c| (c.start_ms, c.end_ms));

    let mut out: Vec<Cue> = Vec::with_capacity(cues.len());
    for i in 0..cues.len() {
        let mut cue = cues[i].clone();
        if let Some(next) = cues.get(i + 1)
            && next.start_ms < cue.end_ms
        {
            cue.end_ms = next.start_ms.max(cue.start_ms);
        }
        cue.end_ms = cue.end_ms.max(cue.start_ms);
        match out.last_mut() {
            Some(prev) if prev.text == cue.text => prev.end_ms = prev.end_ms.max(cue.end_ms),
            _ => out.push(cue),
        }
    }
    out
}

fn clean_text(raw: &str) -> String {
    // Token-level filtering: offsets found in a lowercased copy are not
    // guaranteed to be char boundaries in the original, so no byte surgery.
    let text = raw
        .split_whitespace()
        .filter(|w| !NON_SPEECH.contains(&w.to_lowercase().as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let lower = text.to_lowercase();
    if HALLUCINATIONS
        .iter()
        .any(|h| lower.trim_end_matches(['.', '!']) == *h)
    {
        return String::new();
    }
    text
}

/// Joins chunk outputs at a boundary. `incoming` cues that start before the
/// last kept cue ends and repeat most of its words are duplicates produced by
/// the overlap between audio chunks; they are dropped. Survivors that still
/// overlap in time are trimmed to start where the previous cue ended.
pub fn join_at_boundary(previous_last: Option<&Cue>, incoming: Vec<Cue>) -> Vec<Cue> {
    let Some(prev) = previous_last else {
        return incoming;
    };
    let mut out = Vec::with_capacity(incoming.len());
    for mut cue in incoming {
        if cue.start_ms < prev.end_ms {
            if word_overlap(&prev.text, &cue.text) >= 0.5 {
                continue;
            }
            cue.start_ms = prev.end_ms.min(cue.end_ms);
        }
        out.push(cue);
    }
    out
}

/// Share of the shorter text's words that also appear in the longer one.
fn word_overlap(a: &str, b: &str) -> f64 {
    let words = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (a, b) = (words(a), words(b));
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        return 0.0;
    }
    let hits = short.iter().filter(|w| long.contains(w)).count();
    hits as f64 / short.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_ms: u64, end_ms: u64, text: &str) -> Cue {
        Cue {
            start_ms,
            end_ms,
            text: text.into(),
        }
    }

    #[test]
    fn rolling_captions_become_a_partition() {
        let out = normalize(vec![
            cue(80, 2720, "Deve não ter horário,"),
            cue(1280, 4880, "madrugada, reunião"),
            cue(2720, 6560, "dia inteiro"),
        ]);
        assert_eq!(out[0], cue(80, 1280, "Deve não ter horário,"));
        assert_eq!(out[1], cue(1280, 2720, "madrugada, reunião"));
        assert_eq!(out[2], cue(2720, 6560, "dia inteiro"));
    }

    #[test]
    fn drops_annotations_hallucinations_and_blank_cues() {
        let out = normalize(vec![
            cue(0, 1000, "[Música]"),
            cue(1000, 2000, "  olá   [música] pessoal "),
            cue(2000, 3000, "Legendas pela comunidade Amara.org"),
            cue(3000, 4000, "\n"),
        ]);
        assert_eq!(out, vec![cue(1000, 2000, "olá pessoal")]);
    }

    #[test]
    fn merges_consecutive_repeats() {
        let out = normalize(vec![cue(0, 1000, "sim"), cue(1000, 2000, "sim")]);
        assert_eq!(out, vec![cue(0, 2000, "sim")]);
    }

    #[test]
    fn boundary_duplicates_from_overlapping_chunks_are_dropped() {
        let prev = cue(590_000, 597_000, "e aí a gente usa o Rust pra isso");
        let incoming = vec![
            cue(589_500, 596_800, "a gente usa o Rust pra isso"),
            cue(597_000, 600_000, "porque é mais rápido"),
        ];
        let out = join_at_boundary(Some(&prev), incoming);
        assert_eq!(out, vec![cue(597_000, 600_000, "porque é mais rápido")]);
    }

    #[test]
    fn overlapping_but_different_text_is_trimmed_not_dropped() {
        let prev = cue(0, 5000, "primeira frase completa");
        let out = join_at_boundary(Some(&prev), vec![cue(4000, 8000, "outra coisa")]);
        assert_eq!(out, vec![cue(5000, 8000, "outra coisa")]);
    }
}
