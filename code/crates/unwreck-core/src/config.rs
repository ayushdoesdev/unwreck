//! User and per-repo config (`~/.config/unwreck/config.toml`, `.unwreck.toml`).
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Defaults for [`GcConfig::keep_turns`].
pub const DEFAULT_KEEP_TURNS: usize = 500;
/// Defaults for [`GcConfig::keep_days`].
pub const DEFAULT_KEEP_DAYS: u32 = 30;
/// Rewrite a pack when unreferenced compressed bytes exceed this fraction of the file.
pub const DEFAULT_REPACK_DEAD_RATIO: f64 = 0.30;

/// On-disk config. Missing files and missing keys use the defaults below.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Garbage-collection / retention.
    pub gc: GcConfig,
}

/// What `unwreck gc` keeps and when it rewrites packs.
#[derive(Clone, Debug, PartialEq)]
pub struct GcConfig {
    /// Keep at least the newest N snapshots (all triggers, not only turn boundaries).
    /// CLI `--keep` overrides this for one run.
    pub keep_turns: usize,
    /// Also keep every snapshot newer than this many days.
    pub keep_days: u32,
    /// Repack a pack file only when dead compressed data exceeds this fraction (0.0–1.0).
    pub repack_dead_ratio: f64,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            keep_turns: DEFAULT_KEEP_TURNS,
            keep_days: DEFAULT_KEEP_DAYS,
            repack_dead_ratio: DEFAULT_REPACK_DEAD_RATIO,
        }
    }
}

#[derive(Default, Deserialize)]
struct Raw {
    #[serde(default)]
    gc: RawGc,
}

#[derive(Default, Deserialize)]
struct RawGc {
    keep_turns: Option<usize>,
    keep_days: Option<u32>,
    repack_dead_ratio: Option<f64>,
}

impl Config {
    /// Built-in defaults (no files).
    ///
    /// ```
    /// let c = unwreck_core::config::Config::builtin();
    /// assert_eq!(c.gc.keep_turns, 500);
    /// assert_eq!(c.gc.keep_days, 30);
    /// ```
    pub fn builtin() -> Self {
        Self::default()
    }

    /// Load user config then overlay `.unwreck.toml` under `project` if present.
    /// Unknown keys are ignored. Invalid TOML is an error (never silently skip).
    pub fn load(project: &Path, user_config: Option<&Path>) -> Result<Self, ConfigError> {
        let mut cfg = Self::builtin();
        if let Some(p) = user_config
            && p.is_file()
        {
            cfg.apply(parse_file(p)?);
        }
        let repo = project.join(".unwreck.toml");
        if repo.is_file() {
            cfg.apply(parse_file(&repo)?);
        }
        Ok(cfg)
    }

    fn apply(&mut self, raw: Raw) {
        if let Some(v) = raw.gc.keep_turns {
            self.gc.keep_turns = v;
        }
        if let Some(v) = raw.gc.keep_days {
            self.gc.keep_days = v;
        }
        if let Some(v) = raw.gc.repack_dead_ratio {
            self.gc.repack_dead_ratio = v;
        }
    }
}

fn parse_file(path: &Path) -> Result<Raw, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConfigError::Toml {
        path: path.display().to_string(),
        source,
    })
}

/// Platform user config path (`$XDG_CONFIG_HOME/unwreck/config.toml` or equivalent).
/// Override with `UNWRECK_CONFIG`.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("UNWRECK_CONFIG") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".config"));
    Some(cfg.join("unwreck/config.toml"))
}

/// Failures reading config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Could not read the file.
    #[error("{path}: {source} (fix: check permissions or remove the file to use defaults)")]
    Io {
        /// Config path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// TOML parse error.
    #[error("{path}: {source} (fix: valid TOML, or delete the file to use defaults)")]
    Toml {
        /// Config path.
        path: String,
        /// Parse error.
        source: toml::de::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_repo_file_keeps_unmentioned_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".unwreck.toml"), "[gc]\nkeep_turns = 12\n").unwrap();
        let c = Config::load(dir.path(), None).unwrap();
        assert_eq!(c.gc.keep_turns, 12);
        assert_eq!(c.gc.keep_days, DEFAULT_KEEP_DAYS);
        assert_eq!(c.gc.repack_dead_ratio, DEFAULT_REPACK_DEAD_RATIO);
    }
}
