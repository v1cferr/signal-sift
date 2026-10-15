//! Benchmarks one whisper.cpp chunk with the same code path the daemon uses.
//!
//! ```text
//! nix develop .#inference -c cargo run --release -p signalsift-media --example bench_chunk -- \
//!     AUDIO MODEL START_S LEN_S [--vad VAD_MODEL] [--cpu] [--threads N] [--lang pt]
//! ```
//!
//! Prints one JSON object: wall time, real-time factor, peak RSS / VRAM / CPU
//! time of ffmpeg and whisper-cli, and the first cues for a sanity check.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use signalsift_media::whisper::{Chunk, WhisperConfig, default_chunk_limits, transcribe_chunk};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: bench_chunk AUDIO MODEL START_S LEN_S [--vad PATH] [--cpu] [--threads N] [--lang L]";
    if args.len() < 4 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let audio = PathBuf::from(&args[0]);
    let start_ms: u64 = args[2].parse::<u64>().expect(usage) * 1000;
    let len_ms: u64 = args[3].parse::<u64>().expect(usage) * 1000;
    let mut cfg = WhisperConfig {
        whisper_cli: "whisper-cli".into(),
        ffmpeg: "ffmpeg".into(),
        model: PathBuf::from(&args[1]),
        vad_model: None,
        language: "pt".into(),
        threads: 4,
        use_gpu: true,
        limits: default_chunk_limits(Duration::from_millis(len_ms)),
    };
    let mut rest = args[4..].iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--vad" => cfg.vad_model = rest.next().map(PathBuf::from),
            "--cpu" => cfg.use_gpu = false,
            "--threads" => cfg.threads = rest.next().and_then(|v| v.parse().ok()).expect(usage),
            "--lang" => cfg.language = rest.next().cloned().expect(usage),
            other => panic!("unknown flag {other}; {usage}"),
        }
    }

    let chunk = Chunk {
        index: 0,
        audio_start_ms: start_ms,
        audio_end_ms: start_ms + len_ms,
        keep_start_ms: start_ms,
        keep_end_ms: start_ms + len_ms,
    };
    let work = tempfile::tempdir().expect("tempdir");
    let t = Instant::now();
    let r = transcribe_chunk(&cfg, &audio, chunk, work.path())
        .await
        .expect("transcription failed");
    let wall_ms = t.elapsed().as_millis() as u64;
    let out = serde_json::json!({
        "engine": cfg.engine_id(),
        "threads": cfg.threads,
        "audio_ms": len_ms,
        "wall_ms": wall_ms,
        "rtf": wall_ms as f64 / len_ms as f64,
        "cues": r.cues.len(),
        "words": r.cues.iter().map(|c| c.text.split_whitespace().count()).sum::<usize>(),
        "ffmpeg": r.ffmpeg,
        "whisper": r.whisper,
        "first_cues": r.cues.iter().take(3).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
