//! YouTube's `json3` timed-text format.
//!
//! Chosen over VTT because it is structured: VTT auto-captions repeat each line
//! across rolling cues and embed word timings in inline tags, while json3 gives
//! one event per caption line with explicit start and duration.

use serde::Deserialize;

use crate::cue::Cue;

#[derive(Debug, thiserror::Error)]
#[error("invalid json3 subtitles: {0}")]
pub struct Json3Error(#[from] serde_json::Error);

#[derive(Deserialize)]
struct Doc {
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    #[serde(default)]
    t_start_ms: u64,
    #[serde(default)]
    d_duration_ms: u64,
    #[serde(default)]
    segs: Vec<Seg>,
}

#[derive(Deserialize)]
struct Seg {
    #[serde(default)]
    utf8: String,
}

/// Parses json3 into raw cues. Window-setup events (no `segs`) and the
/// newline-only events that rolling captions append are skipped; [`crate::cue::normalize`]
/// takes care of overlaps.
pub fn parse(bytes: &[u8]) -> Result<Vec<Cue>, Json3Error> {
    let doc: Doc = serde_json::from_slice(bytes)?;
    Ok(doc
        .events
        .into_iter()
        .filter_map(|e| {
            let text: String = e.segs.iter().map(|s| s.utf8.as_str()).collect();
            if text.trim().is_empty() {
                return None;
            }
            Some(Cue {
                start_ms: e.t_start_ms,
                end_ms: e.t_start_ms + e.d_duration_ms,
                text,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auto_captions_and_skips_window_events() {
        let doc = r#"{"wireMagic":"pb3","events":[
            {"tStartMs":0,"dDurationMs":822199,"id":1,"wpWinPosId":1},
            {"tStartMs":80,"dDurationMs":2640,"wWinId":1,"segs":[{"utf8":"Deve","acAsrConf":0},{"utf8":" não","tOffsetMs":200}]},
            {"tStartMs":1270,"dDurationMs":1450,"wWinId":1,"aAppend":1,"segs":[{"utf8":"\n"}]},
            {"tStartMs":1280,"dDurationMs":3600,"wWinId":1,"segs":[{"utf8":"madrugada,"}]}
        ]}"#
        .as_bytes();
        let cues = parse(doc).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Deve não");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (80, 2720));
        assert_eq!(cues[1].start_ms, 1280);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"<tt>not json</tt>").is_err());
    }
}
