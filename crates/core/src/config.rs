//! Daemon configuration.
//!
//! Every field has a safe default, so an absent config file yields a working
//! daemon bound to loopback. Unknown keys are rejected: a typo such as
//! `listn = "0.0.0.0:80"` must fail loudly instead of being silently ignored.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::Profile;

const APP: &str = "signalsift";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub paths: PathsConfig,
    pub runtime: RuntimeConfig,
    pub media: MediaConfig,
    pub asr: AsrConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP API binds to. Non-loopback addresses are refused unless
    /// `allow_non_loopback` is set, because the API has no TLS.
    pub listen: SocketAddr,
    pub allow_non_loopback: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsConfig {
    /// SQLite database and API token. Defaults to `$XDG_STATE_HOME/signalsift`.
    pub state_dir: Option<PathBuf>,
    /// Temporary audio and subtitle files. Defaults to `$XDG_CACHE_HOME/signalsift`.
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    /// Profile applied at startup; it can be changed at runtime.
    pub profile: Profile,
    /// Heavy jobs (transcription, inference) allowed at the same time.
    pub heavy_concurrency: usize,
    /// How long shutdown waits for in-flight work before giving up.
    #[serde(with = "secs")]
    pub shutdown_grace: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Executables are looked up on PATH unless an absolute path is given.
    pub yt_dlp: PathBuf,
    pub ffmpeg: PathBuf,
    /// Preferred subtitle languages, most preferred first.
    pub subtitle_languages: Vec<String>,
    /// Media longer than this is refused before anything is downloaded.
    #[serde(with = "secs")]
    pub max_duration: Duration,
    /// Wall-clock limit for any single subprocess call.
    #[serde(with = "secs")]
    pub subprocess_timeout: Duration,
    /// Upper bound on bytes read from a subprocess's stdout or a subtitle file.
    pub max_output_bytes: u64,
}

/// Local speech recognition, used only when a video has no usable captions.
/// Disabled until a model is configured: models are never downloaded
/// automatically (see docs/benchmarks/ for which one to fetch).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AsrConfig {
    pub whisper_cli: PathBuf,
    /// ggml model file, e.g. ggml-large-v3-turbo-q5_0.bin.
    pub model: Option<PathBuf>,
    /// Silero VAD model; strongly recommended (skips silence, curbs hallucinations).
    pub vad_model: Option<PathBuf>,
    /// Spoken language when the platform does not declare one; `auto` detects.
    pub language: String,
    pub threads: u32,
    pub use_gpu: bool,
    #[serde(with = "secs")]
    pub chunk: Duration,
    #[serde(with = "secs")]
    pub overlap: Duration,
    /// Keep downloaded audio after a complete transcription (default: delete).
    pub keep_audio: bool,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            whisper_cli: "whisper-cli".into(),
            model: None,
            vad_model: None,
            language: "auto".into(),
            threads: 4,
            use_gpu: true,
            chunk: Duration::from_secs(600),
            overlap: Duration::from_secs(30),
            keep_audio: false,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 7878)),
            allow_non_loopback: false,
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            profile: Profile::Balanced,
            heavy_concurrency: 1,
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            yt_dlp: "yt-dlp".into(),
            ffmpeg: "ffmpeg".into(),
            subtitle_languages: vec!["pt".into(), "en".into()],
            max_duration: Duration::from_secs(6 * 3600),
            subprocess_timeout: Duration::from_secs(300),
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error(
        "cannot determine a home directory for XDG paths; set paths.state_dir and paths.cache_dir"
    )]
    NoHome,
}

impl Config {
    /// Loads `path` if given, otherwise the default location if it exists,
    /// otherwise the built-in defaults.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let candidate = match path {
            Some(p) => Some(p.to_path_buf()),
            None => default_config_file().filter(|p| p.exists()),
        };
        let config = match candidate {
            Some(path) => Self::from_file(&path)?,
            None => Self::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.server.listen.ip().is_loopback() && !self.server.allow_non_loopback {
            return Err(ConfigError::Invalid(format!(
                "server.listen = {} is not a loopback address; the API has no TLS, \
                 set server.allow_non_loopback = true only behind a reverse proxy",
                self.server.listen
            )));
        }
        if self.asr.overlap >= self.asr.chunk {
            return Err(ConfigError::Invalid(
                "asr.overlap must be shorter than asr.chunk".into(),
            ));
        }
        if self.runtime.heavy_concurrency == 0 {
            return Err(ConfigError::Invalid(
                "runtime.heavy_concurrency must be at least 1".into(),
            ));
        }
        Ok(())
    }

    pub fn state_dir(&self) -> Result<PathBuf, ConfigError> {
        match &self.paths.state_dir {
            Some(p) => Ok(p.clone()),
            None => xdg_dir("XDG_STATE_HOME", ".local/state").ok_or(ConfigError::NoHome),
        }
    }

    pub fn cache_dir(&self) -> Result<PathBuf, ConfigError> {
        match &self.paths.cache_dir {
            Some(p) => Ok(p.clone()),
            None => xdg_dir("XDG_CACHE_HOME", ".cache").ok_or(ConfigError::NoHome),
        }
    }

    pub fn database_path(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.state_dir()?.join("signalsift.db"))
    }
}

/// `$XDG_CONFIG_HOME/signalsift/config.toml`.
pub fn default_config_file() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join("config.toml"))
}

/// `$VAR/signalsift`, or `$HOME/<fallback>/signalsift` when `$VAR` is unset or
/// relative (the XDG spec says relative values must be ignored). Resolved by
/// hand: the `directories` crate would add an MPL-2.0 transitive dependency
/// for these few lines (see docs/adr/0007-license.md).
fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(fallback))
        })?;
    Some(base.join(APP))
}

/// Durations are written as whole seconds in TOML, which is what a human
/// editing the file expects (`subprocess_timeout = 300`).
mod secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bind_to_loopback_with_one_heavy_job() {
        let c = Config::default();
        assert!(c.server.listen.ip().is_loopback());
        assert_eq!(c.runtime.heavy_concurrency, 1);
        c.validate().unwrap();
    }

    #[test]
    fn partial_files_keep_defaults_for_missing_keys() {
        let c = Config::from_toml("[runtime]\nprofile = \"gaming\"\n").unwrap();
        assert_eq!(c.runtime.profile, Profile::Gaming);
        assert_eq!(c.media.subprocess_timeout, Duration::from_secs(300));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::from_toml("[server]\nlistn = \"0.0.0.0:80\"\n").is_err());
    }

    #[test]
    fn public_bind_requires_explicit_opt_in() {
        let c = Config::from_toml("[server]\nlisten = \"0.0.0.0:7878\"\n").unwrap();
        assert!(c.validate().is_err());
        let c =
            Config::from_toml("[server]\nlisten = \"0.0.0.0:7878\"\nallow_non_loopback = true\n")
                .unwrap();
        c.validate().unwrap();
    }

    #[test]
    fn explicit_paths_override_xdg() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config {
            paths: PathsConfig {
                state_dir: Some(dir.path().into()),
                cache_dir: None,
            },
            ..Config::default()
        };
        assert_eq!(c.database_path().unwrap(), dir.path().join("signalsift.db"));
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../../config/signalsift.example.toml");
        let c = Config::from_toml(text).unwrap();
        c.validate().unwrap();
    }
}
