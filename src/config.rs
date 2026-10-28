use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

fn default_provider() -> String {
    "herne".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_provider")]
    pub provider: String,

    // Herne (tevis) fields — required for provider=herne, default empty otherwise
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub department_id: String,
    #[serde(default)]
    pub department_name: String,
    #[serde(default)]
    pub service_id: String,
    pub service_name: String,
    #[serde(default)]
    pub location_id: String,
    pub location_name: String,
    #[serde(default)]
    pub gps_lat: String,
    #[serde(default)]
    pub gps_long: String,

    // Shared
    pub latest_date: Option<String>,
    #[serde(default)]
    pub earliest_date: Option<String>,
    pub ntfy_topic: String,
    pub deadline: String,

    // Bochum (termine.bochum.de) fields
    #[serde(default)]
    pub calendar_path: Option<String>,
    #[serde(default)]
    pub calendar_uid: Option<String>,
    #[serde(default)]
    pub service_uid: Option<String>,
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".config").join("termin-checker"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Could not read config at {}", path.display()))?;
        toml::from_str(&content).context("Could not parse config")
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        fs::create_dir_all(&dir)?;
        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }

    pub fn latest_date_parsed(&self) -> Option<NaiveDate> {
        self.latest_date
            .as_ref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%d.%m.%Y").ok())
    }

    pub fn earliest_date_parsed(&self) -> Option<NaiveDate> {
        self.earliest_date
            .as_ref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%d.%m.%Y").ok())
    }

    pub fn deadline_parsed(&self) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(&self.deadline, "%d.%m.%Y").ok()
    }
}
