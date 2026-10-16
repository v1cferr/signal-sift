//! Benchmarks a GGUF model on stored segments through the daemon's code path.
//!
//! ```text
//! signalsift media show VIDEO_ID > segments.json
//! nix develop .#inference -c cargo run --release -p signalsift-analysis --example bench_llm -- \
//!     MODEL.gguf INTERESTS.toml OUT.json SEGMENTS.json [SEGMENTS.json ...] [--every N]
//! ```
//!
//! Writes every segment's analysis plus aggregate timing and resource figures
//! to OUT.json, and prints the aggregate.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use signalsift_analysis::llama::{LlamaConfig, LlamaServer};
use signalsift_analysis::prompt::ContentContext;
use signalsift_analysis::{Analyzer, InterestProfile};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let every = match args.iter().position(|a| a == "--every") {
        Some(i) => {
            let n = args[i + 1].parse::<usize>().expect("--every N");
            args.drain(i..=i + 1);
            n
        }
        None => 1,
    };
    assert!(
        args.len() >= 4,
        "usage: bench_llm MODEL INTERESTS OUT SEGMENTS.json... [--every N]"
    );
    let model = PathBuf::from(&args[0]);
    let profile = InterestProfile::load(args[1].as_ref()).expect("interest profile");
    let out_path = PathBuf::from(&args[2]);

    let cfg = LlamaConfig {
        server: "llama-server".into(),
        model,
        context: 8192,
        gpu_layers: 999,
        threads: 4,
        startup_timeout: Duration::from_secs(180),
        request_timeout: Duration::from_secs(300),
    };
    let server = LlamaServer::start(&cfg).await.expect("llama-server");

    let mut results = Vec::new();
    let (mut ok, mut failed) = (0usize, 0usize);
    let (mut prompt_tokens, mut completion_tokens, mut prompt_ms, mut completion_ms) =
        (0u64, 0u64, 0f64, 0f64);
    let started = Instant::now();
    for file in &args[3..] {
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).expect("segments file")).expect("json");
        let item = &doc["item"];
        let video = item["external_id"].as_str().unwrap_or("").to_owned();
        let title = item["title"].as_str().unwrap_or("").to_owned();
        let author = item["author"].as_str().map(str::to_owned);
        // Same rule as the daemon: name the transcript's language.
        let language = item["transcript"]["language"]
            .as_str()
            .map(signalsift_analysis::prompt::language_name);
        let analyzer = Analyzer {
            provider: &server,
            profile: &profile,
            language: language.as_deref(),
        };
        for seg in doc["segments"]
            .as_array()
            .expect("segments")
            .iter()
            .step_by(every)
        {
            let ctx = ContentContext {
                title: &title,
                source: author.as_deref(),
                kind: "video",
                position: seg["start"].as_str().map(str::to_owned),
            };
            let text = seg["text"].as_str().unwrap_or("");
            match analyzer.segment(&ctx, text).await {
                Ok((a, stats)) => {
                    ok += 1;
                    prompt_tokens += stats.prompt_tokens;
                    completion_tokens += stats.completion_tokens;
                    prompt_ms += stats.prompt_ms;
                    completion_ms += stats.completion_ms;
                    results.push(serde_json::json!({
                        "video_id": video, "start_ms": seg["start_ms"], "start": seg["start"],
                        "analysis": a, "stats": stats,
                    }));
                }
                Err(e) => {
                    failed += 1;
                    results.push(serde_json::json!({ "video_id": video, "start_ms": seg["start_ms"], "error": e.to_string() }));
                }
            }
        }
    }
    let wall = started.elapsed();
    let usage = server.usage();
    let load_ms = server.load_ms;
    let model_id = {
        use signalsift_analysis::provider::InferenceProvider;
        server.model_id().to_owned()
    };
    server.stop().await;

    let summary = serde_json::json!({
        "model": model_id,
        "load_ms": load_ms,
        "segments_ok": ok,
        "segments_failed": failed,
        "wall_s": wall.as_secs_f64(),
        "mean_segment_s": wall.as_secs_f64() / (ok + failed).max(1) as f64,
        "prompt_tokens_per_s": prompt_tokens as f64 / (prompt_ms / 1000.0).max(1e-9),
        "gen_tokens_per_s": completion_tokens as f64 / (completion_ms / 1000.0).max(1e-9),
        "mean_prompt_tokens": prompt_tokens / ok.max(1) as u64,
        "mean_completion_tokens": completion_tokens / ok.max(1) as u64,
        "server_peak_rss_mib": usage.peak_rss_kib / 1024,
        "server_peak_vram_mib": usage.peak_vram_kib / 1024,
        "server_cpu_s": usage.cpu_time_ms as f64 / 1000.0,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    std::fs::write(
        &out_path,
        serde_json::to_vec_pretty(&serde_json::json!({ "summary": summary, "results": results }))
            .unwrap(),
    )
    .expect("write output");
}
