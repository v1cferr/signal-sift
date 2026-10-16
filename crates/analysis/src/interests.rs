//! The interest profile: what the user wants surfaced.
//!
//! Kept in a TOML file the user edits (never hard-coded), and rendered into
//! the prompt verbatim. Topics carry a short description because a bare
//! label ("security") is too vague for a model to score consistently.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterestProfile {
    /// Free text about who the reader is and what "useful" means to them.
    #[serde(default)]
    pub about: String,
    #[serde(rename = "topic", default)]
    pub topics: Vec<Topic>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topic {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, thiserror::Error)]
pub enum InterestError {
    #[error("cannot read interest profile {0}: {1}")]
    Read(String, std::io::Error),
    #[error("invalid interest profile {0}: {1}")]
    Parse(String, toml::de::Error),
    #[error("interest profile {0} lists no topics")]
    Empty(String),
}

impl InterestProfile {
    pub fn load(path: &Path) -> Result<Self, InterestError> {
        let shown = path.display().to_string();
        let text =
            std::fs::read_to_string(path).map_err(|e| InterestError::Read(shown.clone(), e))?;
        let profile: Self =
            toml::from_str(&text).map_err(|e| InterestError::Parse(shown.clone(), e))?;
        if profile.topics.is_empty() {
            return Err(InterestError::Empty(shown));
        }
        Ok(profile)
    }

    /// The profile as prompt text: one line per topic.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if !self.about.trim().is_empty() {
            out.push_str(self.about.trim());
            out.push_str("\n\n");
        }
        for t in &self.topics {
            out.push_str("- ");
            out.push_str(&t.name);
            if !t.description.trim().is_empty() {
                out.push_str(": ");
                out.push_str(t.description.trim());
            }
            out.push('\n');
        }
        out
    }

    /// A short fingerprint stored with each analysis, so scores produced
    /// under a different profile can be told apart (and recomputed).
    pub fn fingerprint(&self) -> String {
        // FNV-1a over the rendered text: stable across builds and platforms,
        // which `DefaultHasher` does not promise.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.render().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_profile_parses_and_renders() {
        let p: InterestProfile =
            toml::from_str(include_str!("../../../config/interests.example.toml")).unwrap();
        assert!(p.topics.len() >= 5);
        let text = p.render();
        assert!(text.contains("Rust"));
        assert_eq!(p.fingerprint(), p.clone().fingerprint());
    }

    #[test]
    fn fingerprint_changes_with_content() {
        let a: InterestProfile = toml::from_str("[[topic]]\nname = \"Rust\"\n").unwrap();
        let b: InterestProfile = toml::from_str("[[topic]]\nname = \"Go\"\n").unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
    }
}
