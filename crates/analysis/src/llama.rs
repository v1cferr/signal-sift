//! llama.cpp's `llama-server`, started for a batch of work and stopped after.
//!
//! The server is a child process ([`Service`]): the daemon never links
//! llama.cpp, and stopping the service releases all of the model's RAM and
//! VRAM. It listens on a random loopback port and talks the
//! OpenAI-compatible chat API, with the reply constrained by a JSON schema.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use signalsift_process::service::Service;
use signalsift_process::subprocess;
use signalsift_process::usage::Usage;

use crate::provider::{ChatRequest, ChatResponse, ChatStats, InferenceError, InferenceProvider};

#[derive(Debug, Clone)]
pub struct LlamaConfig {
    pub server: PathBuf,
    pub model: PathBuf,
    pub context: u32,
    /// Layers offloaded to the GPU; a large number means "all of them".
    pub gpu_layers: u32,
    pub threads: u32,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

pub struct LlamaServer {
    service: Service,
    base: String,
    client: reqwest::Client,
    model_id: String,
    pub load_ms: u64,
    request_timeout: Duration,
}

impl LlamaServer {
    /// Starts the server and waits until the model is loaded.
    pub async fn start(cfg: &LlamaConfig) -> Result<Self, InferenceError> {
        if !cfg.model.is_file() {
            return Err(InferenceError::Unavailable(format!(
                "model file {} not found",
                cfg.model.display()
            )));
        }
        let port =
            free_port().map_err(|e| InferenceError::Unavailable(format!("no free port: {e}")))?;
        let mut cmd = subprocess::command(&cfg.server);
        cmd.arg("-m")
            .arg(&cfg.model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", &cfg.context.to_string()])
            .args(["-ngl", &cfg.gpu_layers.to_string()])
            .args(["-t", &cfg.threads.to_string()])
            // One request at a time: the daemon runs one heavy job anyway,
            // and a single slot keeps the KV cache at one context's worth.
            .args(["-np", "1"])
            // No hidden reasoning: replies are short JSON, and thinking
            // tokens would cost time without being stored.
            .args(["--reasoning-budget", "0"]);
        let started = Instant::now();
        let mut service = Service::spawn("llama-server", cmd).map_err(|e| {
            InferenceError::Unavailable(format!("cannot start {}: {e}", cfg.server.display()))
        })?;
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| InferenceError::Unavailable(e.to_string()))?;

        let deadline = started + cfg.startup_timeout;
        loop {
            if let Some(status) = service.exited() {
                return Err(InferenceError::Unavailable(format!(
                    "llama-server exited during startup ({status}); check the model file and GPU"
                )));
            }
            service.sample();
            let ok = client
                .get(format!("{base}/health"))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if ok {
                break;
            }
            if Instant::now() > deadline {
                drop(service);
                return Err(InferenceError::Unavailable(
                    "llama-server did not become ready in time".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let model_id = format!(
            "llama.cpp {}",
            cfg.model
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        tracing::info!(model = %model_id, load_ms = started.elapsed().as_millis() as u64, "llama-server ready");
        Ok(Self {
            service,
            base,
            client,
            model_id,
            load_ms: started.elapsed().as_millis() as u64,
            request_timeout: cfg.request_timeout,
        })
    }

    /// Peak RSS / VRAM of the server so far (sampled).
    pub fn usage(&self) -> Usage {
        self.service.sample();
        self.service.usage()
    }

    pub async fn stop(self) -> Usage {
        let usage = self.service.stop(Duration::from_secs(5)).await;
        tracing::info!(model = %self.model_id, "llama-server stopped");
        usage
    }
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageBlock>,
    #[serde(default)]
    timings: Option<Timings>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct UsageBlock {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Deserialize)]
struct Timings {
    #[serde(default)]
    prompt_ms: f64,
    #[serde(default)]
    predicted_ms: f64,
}

impl InferenceProvider for LlamaServer {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let body = json!({
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user", "content": req.user },
            ],
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": req.schema_name, "schema": req.schema, "strict": true }
            },
            "chat_template_kwargs": { "enable_thinking": false },
            "cache_prompt": true,
        });
        let started = Instant::now();
        let res = self
            .client
            .post(format!("{}/v1/chat/completions", self.base))
            .timeout(self.request_timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| InferenceError::Request(e.to_string()))?;
        self.service.sample();
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(InferenceError::Request(format!(
                "HTTP {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        let c: Completion = res
            .json()
            .await
            .map_err(|e| InferenceError::Request(e.to_string()))?;
        let content = c
            .choices
            .into_iter()
            .next()
            .and_then(|ch| ch.message.content)
            .ok_or_else(|| InferenceError::Request("empty completion".into()))?;
        let usage = c.usage.unwrap_or(UsageBlock {
            prompt_tokens: 0,
            completion_tokens: 0,
        });
        let timings = c.timings.unwrap_or(Timings {
            prompt_ms: 0.0,
            predicted_ms: 0.0,
        });
        Ok(ChatResponse {
            content,
            stats: ChatStats {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                prompt_ms: timings.prompt_ms,
                completion_ms: timings.predicted_ms,
                wall_ms: started.elapsed().as_millis() as u64,
            },
        })
    }
}

fn free_port() -> std::io::Result<u16> {
    // Bind to port 0 and release it. Another process could take it in the
    // gap; startup then fails and the job is retried, which is acceptable.
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
