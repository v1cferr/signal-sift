//! YouTube access through yt-dlp.
//!
//! Inputs are reduced to a validated 11-character video id and the URL handed
//! to yt-dlp is rebuilt from it. yt-dlp therefore never receives user-supplied
//! text: no generic-extractor fetches of arbitrary hosts (SSRF), no argument
//! injection through a value starting with `-`, no playlists.
//!
//! Every invocation passes `--ignore-config`, so a user's yt-dlp config file
//! cannot change behavior (or add `--exec`) behind the daemon's back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use signalsift_process::subprocess::{self, Invocation, Limits, SubprocessError};

pub use signalsift_core::youtube::{video_id, watch_url};

/// The subset of yt-dlp's info JSON we use. Unknown fields are ignored; the
/// full JSON is kept on disk next to the media for reuse with `--load-info-json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub uploader_id: Option<String>,
    /// Seconds.
    #[serde(default)]
    pub duration: Option<f64>,
    /// `YYYYMMDD`.
    #[serde(default)]
    pub upload_date: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub live_status: Option<String>,
    #[serde(default)]
    pub chapters: Option<Vec<ChapterInfo>>,
    #[serde(default)]
    pub subtitles: Option<BTreeMap<String, Vec<SubtitleFormat>>>,
    #[serde(default)]
    pub automatic_captions: Option<BTreeMap<String, Vec<SubtitleFormat>>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChapterInfo {
    pub start_time: f64,
    pub end_time: f64,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubtitleFormat {
    pub ext: String,
}

impl VideoInfo {
    pub fn duration_ms(&self) -> Option<u64> {
        self.duration
            .filter(|d| *d > 0.0)
            .map(|d| (d * 1000.0) as u64)
    }

    pub fn is_live_now(&self) -> bool {
        matches!(self.live_status.as_deref(), Some("is_live" | "is_upcoming"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackOrigin {
    ManualSubtitles,
    AutoSubtitles,
}

impl TrackOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackOrigin::ManualSubtitles => "manual_subtitles",
            TrackOrigin::AutoSubtitles => "auto_subtitles",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackChoice {
    /// yt-dlp's language key, e.g. `pt-BR` or `pt-orig`.
    pub key: String,
    /// The base language, e.g. `pt`.
    pub language: String,
    pub origin: TrackOrigin,
}

/// Picks the best caption track available as json3:
///
/// 1. manual subtitles in a preferred language (human-made, best text);
/// 2. automatic captions in the *spoken* language (`*-orig`, or the key equal
///    to the video's declared language).
///
/// Machine-translated automatic captions are never used: they are a
/// translation of an ASR guess, so their errors compound, and the summary
/// should be built from what was actually said.
pub fn choose_track(info: &VideoInfo, preferred: &[String]) -> Option<TrackChoice> {
    let has_json3 = |formats: &Vec<SubtitleFormat>| formats.iter().any(|f| f.ext == "json3");
    let base = |key: &str| key.split('-').next().unwrap_or(key).to_owned();

    if let Some(subs) = &info.subtitles {
        for lang in preferred {
            if let Some((key, _)) = subs
                .iter()
                .filter(|(_, f)| has_json3(f))
                .find(|(k, _)| *k == lang || k.starts_with(&format!("{lang}-")))
            {
                return Some(TrackChoice {
                    key: key.clone(),
                    language: base(key),
                    origin: TrackOrigin::ManualSubtitles,
                });
            }
        }
    }

    let auto = info.automatic_captions.as_ref()?;
    let spoken = auto
        .iter()
        .filter(|(_, f)| has_json3(f))
        .find(|(k, _)| k.ends_with("-orig"))
        .or_else(|| {
            let declared = info.language.as_deref()?;
            auto.iter()
                .filter(|(_, f)| has_json3(f))
                .find(|(k, _)| *k == declared)
        })?;
    Some(TrackChoice {
        key: spoken.0.clone(),
        language: base(spoken.0),
        origin: TrackOrigin::AutoSubtitles,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum YoutubeError {
    #[error(transparent)]
    Subprocess(#[from] SubprocessError),
    #[error("yt-dlp returned unparseable metadata: {0}")]
    Metadata(serde_json::Error),
    #[error("yt-dlp reported success but {0} was not written")]
    Missing(PathBuf),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct YtDlp {
    pub program: PathBuf,
    pub limits: Limits,
}

impl YtDlp {
    fn base_command(&self) -> tokio::process::Command {
        let mut cmd = subprocess::command(&self.program);
        cmd.args([
            "--ignore-config",
            "--no-playlist",
            "--no-progress",
            "--no-warnings",
        ]);
        cmd
    }

    /// Fetches metadata and writes the raw info JSON to `info_path`.
    pub async fn probe(
        &self,
        id: &str,
        info_path: &Path,
    ) -> Result<(VideoInfo, Invocation), YoutubeError> {
        let mut cmd = self.base_command();
        cmd.args(["--skip-download", "--dump-single-json", "--"])
            .arg(watch_url(id));
        let (out, inv) = subprocess::run_timed(cmd, self.limits).await?;
        let info: VideoInfo =
            serde_json::from_slice(&out.stdout).map_err(YoutubeError::Metadata)?;
        if let Some(dir) = info_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(info_path, &out.stdout).await?;
        Ok((info, inv))
    }

    /// Downloads one caption track as json3 into `dir`, reusing the stored
    /// metadata instead of extracting the video page again.
    pub async fn download_subtitles(
        &self,
        info_path: &Path,
        track: &TrackChoice,
        dir: &Path,
    ) -> Result<(PathBuf, Invocation), YoutubeError> {
        tokio::fs::create_dir_all(dir).await?;
        let mut cmd = self.base_command();
        cmd.arg("--load-info-json")
            .arg(info_path)
            .args(["--skip-download", "--sub-format", "json3", "--sub-langs"])
            // --sub-langs takes a regex; anchor it so `pt` does not match `pt-PT`.
            .arg(format!("^{}$", regex_escape(&track.key)))
            .arg(match track.origin {
                TrackOrigin::ManualSubtitles => "--write-subs",
                TrackOrigin::AutoSubtitles => "--write-auto-subs",
            })
            .arg("-o")
            .arg(dir.join("subtitles"));
        let (_, inv) = subprocess::run_timed(cmd, self.limits).await?;
        let path = dir.join(format!("subtitles.{}.json3", track.key));
        if !path.exists() {
            return Err(YoutubeError::Missing(path));
        }
        Ok((path, inv))
    }

    /// Downloads the smallest audio-only stream that is still comfortably
    /// above what 16 kHz speech recognition needs. yt-dlp resumes `.part`
    /// files, so an interrupted download continues instead of restarting.
    pub async fn download_audio(
        &self,
        info_path: &Path,
        dir: &Path,
    ) -> Result<(PathBuf, Invocation), YoutubeError> {
        tokio::fs::create_dir_all(dir).await?;
        if let Some(existing) = find_audio(dir).await? {
            return Ok((
                existing,
                Invocation {
                    program: "cache".into(),
                    wall_ms: 0,
                    usage: Default::default(),
                },
            ));
        }
        let mut cmd = self.base_command();
        cmd.arg("--load-info-json")
            .arg(info_path)
            .args([
                "-f",
                "bestaudio[acodec=opus][abr<=70]/bestaudio[abr<=80]/bestaudio",
            ])
            .arg("-o")
            .arg(dir.join("audio.%(ext)s"));
        let (_, inv) = subprocess::run_timed(cmd, self.limits).await?;
        let path = find_audio(dir)
            .await?
            .ok_or_else(|| YoutubeError::Missing(dir.join("audio.*")))?;
        Ok((path, inv))
    }
}

async fn find_audio(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(e) = entries.next_entry().await? {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("audio.") && !name.ends_with(".part") && !name.ends_with(".ytdl") {
            return Ok(Some(e.path()));
        }
    }
    Ok(None)
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            let special = ".^$*+?()[]{}|\\".contains(c);
            special
                .then_some('\\')
                .into_iter()
                .chain(std::iter::once(c))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(subs: &[&str], auto: &[&str], language: Option<&str>) -> VideoInfo {
        let map = |keys: &[&str]| {
            Some(
                keys.iter()
                    .map(|k| {
                        (
                            (*k).to_owned(),
                            vec![SubtitleFormat {
                                ext: "json3".into(),
                            }],
                        )
                    })
                    .collect(),
            )
        };
        VideoInfo {
            id: "N3eU-2VIByQ".into(),
            title: "t".into(),
            channel: None,
            channel_id: None,
            uploader_id: None,
            duration: Some(60.0),
            upload_date: None,
            description: None,
            language: language.map(Into::into),
            live_status: None,
            chapters: None,
            subtitles: map(subs),
            automatic_captions: map(auto),
        }
    }

    fn prefs() -> Vec<String> {
        vec!["pt".into(), "en".into()]
    }

    #[test]
    fn manual_subtitles_win_in_preference_order() {
        let c = choose_track(&info(&["en", "pt-BR"], &["pt-orig"], None), &prefs()).unwrap();
        assert_eq!(
            (c.key.as_str(), c.origin),
            ("pt-BR", TrackOrigin::ManualSubtitles)
        );
    }

    #[test]
    fn auto_captions_use_the_spoken_language_only() {
        let c = choose_track(&info(&[], &["en", "pt", "pt-orig"], Some("pt")), &prefs()).unwrap();
        assert_eq!((c.key.as_str(), c.language.as_str()), ("pt-orig", "pt"));
    }

    #[test]
    fn translated_auto_captions_are_never_chosen() {
        // English and Portuguese exist only as translations; spoken language unknown.
        assert_eq!(
            choose_track(&info(&[], &["en", "pt"], None), &prefs()),
            None
        );
    }

    #[test]
    fn no_captions_means_no_track() {
        assert_eq!(choose_track(&info(&[], &[], Some("pt")), &prefs()), None);
    }

    #[test]
    fn sub_lang_regex_is_escaped() {
        assert_eq!(regex_escape("pt-BR"), "pt-BR");
        assert_eq!(regex_escape("a.b(c)"), "a\\.b\\(c\\)");
    }
}
