use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Config {
    #[serde(default)]
    pub repos: Vec<PathBuf>,
    #[serde(default)]
    pub jira: Option<JiraConfig>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct JiraConfig {
    pub base_url: String,
    pub email: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        // One-shot migration from the previous (XDG / Application Support)
        // location. Only runs when the new file doesn't exist yet.
        if !path.exists() {
            if let Ok(old) = Self::legacy_path() {
                if old.exists() {
                    if let Some(parent) = path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::rename(&old, &path);
                }
            }
        }
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config at {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(&path, text)
            .with_context(|| format!("writing config to {}", path.display()))?;
        Ok(())
    }

    pub fn path() -> Result<PathBuf> {
        let base =
            directories::BaseDirs::new().context("resolving home directory")?;
        Ok(base.home_dir().join(".dlog").join("config.toml"))
    }

    fn legacy_path() -> Result<PathBuf> {
        let dirs =
            ProjectDirs::from("", "", "dlog").context("resolving legacy config directory")?;
        Ok(dirs.config_dir().join("config.toml"))
    }
}
