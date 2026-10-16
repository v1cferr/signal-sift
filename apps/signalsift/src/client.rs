//! Talks to the running daemon for commands that change its live state.
//! Reads the API token from the state directory, which only this user can
//! read, so the CLI needs no separate credential.

use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use signalsift_core::{Config, Profile};

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// Switch the running daemon's profile (until restart).
    Set { profile: ProfileArg },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ProfileArg {
    Balanced,
    Gaming,
    Eco,
}

impl From<ProfileArg> for Profile {
    fn from(p: ProfileArg) -> Self {
        match p {
            ProfileArg::Balanced => Profile::Balanced,
            ProfileArg::Gaming => Profile::Gaming,
            ProfileArg::Eco => Profile::Eco,
        }
    }
}

pub async fn profile(config: &Config, cmd: ProfileCommand) -> anyhow::Result<()> {
    let ProfileCommand::Set { profile } = cmd;
    let body = serde_json::json!({ "profile": Profile::from(profile) });
    let res = send(config, reqwest::Method::PUT, "/v1/profile", body).await?;
    println!("{}", serde_json::to_string_pretty(&res)?);
    Ok(())
}

/// Queues a video in the daemon (`POST /v1/media`).
pub async fn queue_media(config: &Config, url: &str, force: bool) -> anyhow::Result<()> {
    let body =
        serde_json::json!({ "url": url, "transcribe": if force { "force" } else { "auto" } });
    let res = send(config, reqwest::Method::POST, "/v1/media", body).await?;
    println!("{}", serde_json::to_string_pretty(&res)?);
    Ok(())
}

async fn send(
    config: &Config,
    method: reqwest::Method,
    path: &str,
    body: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let token = signalsift_api::auth::read_token(&config.state_dir()?)
        .context("no API token; has the daemon been started (signalsift serve)?")?;
    // Loopback only, so the SSRF-guarded fetcher is not used here: this
    // client talks exclusively to the configured local daemon.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()?;
    let url = format!("http://{}{path}", config.server.listen);
    let res = client
        .request(method, &url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .with_context(|| {
            format!("cannot reach the daemon at {url}; is `signalsift serve` running?")
        })?;
    let status = res.status();
    let json: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        bail!("daemon answered {status}: {json}");
    }
    Ok(json)
}
