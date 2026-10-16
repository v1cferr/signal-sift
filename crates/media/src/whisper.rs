//! Local speech recognition with whisper.cpp, run as a child process.
//!
//! Long audio is processed in fixed chunks:
//!
//! * memory stays bounded: whisper.cpp loads its whole input as 32-bit float
//!   samples, which for a four-hour podcast at 16 kHz would be ~0.9 GiB;
//! * progress is checkpointed per chunk, so an interruption loses at most one
//!   chunk and the transcript up to the last checkpoint is usable (as partial);
//! * the model is loaded only while a chunk runs; between jobs nothing of
//!   whisper.cpp stays in RAM or VRAM.
//!
//! Chunks overlap so that words cut at a chunk edge are heard whole in the
//! neighbouring chunk. Each chunk owns a *keep range*; cues are kept only if
//! they start inside it, and ranges tile the timeline without gaps or overlap.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cue::{Cue, normalize};
use signalsift_process::subprocess::{self, Invocation, Limits, SubprocessError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Chunk {
    pub index: u32,
    /// Audio actually decoded for this chunk.
    pub audio_start_ms: u64,
    pub audio_end_ms: u64,
    /// Cues starting in `[keep_start_ms, keep_end_ms)` belong to this chunk.
    pub keep_start_ms: u64,
    pub keep_end_ms: u64,
}

/// Plans chunks of `chunk_ms` with `overlap_ms` of extra audio before each
/// boundary. The keep boundary sits in the middle of the overlap, so each
/// side has `overlap_ms / 2` of context around it.
pub fn plan_chunks(duration_ms: u64, chunk_ms: u64, overlap_ms: u64) -> Vec<Chunk> {
    assert!(
        chunk_ms > overlap_ms,
        "chunk must be longer than the overlap"
    );
    let half = overlap_ms / 2;
    let count = duration_ms.div_ceil(chunk_ms);
    (0..count)
        .map(|k| {
            let boundary = k * chunk_ms;
            let next = (k + 1) * chunk_ms;
            let last = k + 1 == count;
            Chunk {
                index: k as u32,
                audio_start_ms: boundary.saturating_sub(overlap_ms),
                audio_end_ms: next.min(duration_ms),
                keep_start_ms: if k == 0 { 0 } else { boundary - half },
                keep_end_ms: if last { duration_ms } else { next - half },
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct WhisperConfig {
    pub whisper_cli: PathBuf,
    pub ffmpeg: PathBuf,
    pub model: PathBuf,
    /// Silero VAD model. With it, silent and music-only stretches are skipped
    /// before decoding, which is both faster and the main defence against
    /// hallucinated text on silence.
    pub vad_model: Option<PathBuf>,
    /// Spoken language (`pt`, `en`) or `auto`.
    pub language: String,
    pub threads: u32,
    pub use_gpu: bool,
    /// Fail rather than let whisper.cpp fall back to the CPU (ADR 0011).
    pub require_gpu: bool,
    pub limits: Limits,
}

impl WhisperConfig {
    /// Human-readable engine id stored with the transcript.
    pub fn engine_id(&self) -> String {
        let model = self
            .model
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            "whisper.cpp {model}{}{}",
            if self.vad_model.is_some() {
                " +vad"
            } else {
                ""
            },
            if self.use_gpu { "" } else { " cpu" }
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WhisperError {
    #[error(transparent)]
    Subprocess(#[from] SubprocessError),
    #[error("whisper.cpp output is not valid JSON: {0}")]
    Output(serde_json::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkResult {
    pub chunk: Chunk,
    pub cues: Vec<Cue>,
    pub ffmpeg: Invocation,
    pub whisper: Invocation,
}

/// Decodes one chunk to 16 kHz mono WAV with ffmpeg, transcribes it, and
/// returns the cues inside the chunk's keep range with absolute positions.
/// Temporary files are removed whether or not transcription succeeds.
pub async fn transcribe_chunk(
    cfg: &WhisperConfig,
    audio: &Path,
    chunk: Chunk,
    workdir: &Path,
) -> Result<ChunkResult, WhisperError> {
    tokio::fs::create_dir_all(workdir).await?;
    let stem = workdir.join(format!("chunk-{:05}", chunk.index));
    let wav = stem.with_extension("wav");
    let json = stem.with_extension("json");
    let result = transcribe_inner(cfg, audio, chunk, &stem, &wav, &json).await;
    for f in [&wav, &json] {
        let _ = tokio::fs::remove_file(f).await;
    }
    result
}

async fn transcribe_inner(
    cfg: &WhisperConfig,
    audio: &Path,
    chunk: Chunk,
    stem: &Path,
    wav: &Path,
    json: &Path,
) -> Result<ChunkResult, WhisperError> {
    let mut ff = subprocess::command(&cfg.ffmpeg);
    ff.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-ss"])
        .arg(secs(chunk.audio_start_ms))
        .arg("-t")
        .arg(secs(chunk.audio_end_ms - chunk.audio_start_ms))
        .arg("-i")
        .arg(audio)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", "-y"])
        .arg(wav);
    // Audio decoding is CPU work: never subject to the GPU requirement.
    let cpu_limits = Limits {
        gpu_deadline: None,
        ..cfg.limits
    };
    let (_, ffmpeg) = subprocess::run_timed(ff, cpu_limits).await?;

    let mut w = subprocess::command(&cfg.whisper_cli);
    w.arg("-m")
        .arg(&cfg.model)
        .arg("-f")
        .arg(wav)
        .args([
            "-l",
            &cfg.language,
            "-t",
            &cfg.threads.to_string(),
            "-oj",
            "-np",
            "-of",
        ])
        .arg(stem);
    if let Some(vad) = &cfg.vad_model {
        w.arg("--vad").arg("-vm").arg(vad);
    }
    if !cfg.use_gpu {
        w.arg("-ng");
    }
    let mut whisper_limits = cfg.limits;
    if cfg.require_gpu {
        whisper_limits.gpu_deadline = Some(GPU_DEADLINE);
    }
    let (_, whisper) = subprocess::run_timed(w, whisper_limits).await?;

    let bytes = tokio::fs::read(json).await?;
    let raw = parse_output(&bytes, chunk.audio_start_ms).map_err(WhisperError::Output)?;
    let cues = normalize(raw)
        .into_iter()
        .filter(|c| c.start_ms >= chunk.keep_start_ms && c.start_ms < chunk.keep_end_ms)
        .collect();
    Ok(ChunkResult {
        chunk,
        cues,
        ffmpeg,
        whisper,
    })
}

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    transcription: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    offsets: Offsets,
    text: String,
}

#[derive(Deserialize)]
struct Offsets {
    from: u64,
    to: u64,
}

/// Parses `whisper-cli -oj` output, shifting chunk-relative offsets to
/// absolute media positions.
pub fn parse_output(bytes: &[u8], base_ms: u64) -> Result<Vec<Cue>, serde_json::Error> {
    // whisper.cpp writes the text as produced by the tokenizer; invalid UTF-8
    // can appear when a multi-byte character is split across tokens.
    let text = String::from_utf8_lossy(bytes);
    let out: Output = serde_json::from_str(&text)?;
    Ok(out
        .transcription
        .into_iter()
        .map(|e| Cue {
            start_ms: base_ms + e.offsets.from,
            end_ms: base_ms + e.offsets.to,
            text: e.text,
        })
        .collect())
}

fn secs(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Default subprocess limits for one chunk: generous, because a CPU-only
/// fallback on a slow machine can take several times real time.
pub fn default_chunk_limits(chunk: Duration) -> Limits {
    Limits::new(chunk * 10, 16 * 1024 * 1024)
}

/// How long whisper-cli gets to put the model in GPU memory before a run is
/// considered a silent CPU fallback (ADR 0011). Loading takes 1-2 s on the
/// reference machine.
pub const GPU_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_ranges_tile_the_timeline() {
        let chunks = plan_chunks(15_447_000, 600_000, 30_000);
        assert_eq!(chunks.len(), 26);
        assert_eq!(chunks[0].keep_start_ms, 0);
        assert_eq!(chunks.last().unwrap().keep_end_ms, 15_447_000);
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].keep_end_ms, pair[1].keep_start_ms);
        }
    }

    #[test]
    fn audio_covers_keep_range_with_context_on_both_sides() {
        for c in plan_chunks(3_000_000, 600_000, 30_000) {
            assert!(c.audio_start_ms <= c.keep_start_ms);
            assert!(c.audio_end_ms >= c.keep_end_ms);
            if c.index > 0 {
                assert_eq!(c.keep_start_ms - c.audio_start_ms, 15_000);
            }
        }
    }

    #[test]
    fn short_media_is_one_chunk() {
        let chunks = plan_chunks(90_000, 600_000, 30_000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            (chunks[0].audio_start_ms, chunks[0].audio_end_ms),
            (0, 90_000)
        );
    }

    #[test]
    fn parses_whisper_json_with_absolute_offsets() {
        let out = r#"{"result":{"language":"pt"},"transcription":[
            {"timestamps":{"from":"00:00:00,000","to":"00:00:02,500"},"offsets":{"from":0,"to":2500},"text":" Olá pessoal"},
            {"timestamps":{"from":"00:00:02,500","to":"00:00:04,000"},"offsets":{"from":2500,"to":4000},"text":" tudo bem?"}
        ]}"#
        .as_bytes();
        let cues = parse_output(out, 570_000).unwrap();
        assert_eq!(cues[1].start_ms, 572_500);
        assert_eq!(cues[0].text, " Olá pessoal");
    }

    #[test]
    fn formats_seconds_for_ffmpeg() {
        assert_eq!(secs(570_000), "570.000");
        assert_eq!(secs(1_234), "1.234");
    }
}
