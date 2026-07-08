use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine a config directory for this platform")]
    NoConfigDir,
    #[error("failed to read config file at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write default config file at {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file at {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to serialize default config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: BackendSettings,
    pub permissions: PermissionSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Parsed and validated now; only exercised once the text-fallback
    /// parser lands (see project plan, milestone M4).
    pub tool_calling_mode: ToolCallingMode,
}

impl Default for BackendSettings {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
            api_key: None,
            tool_calling_mode: ToolCallingMode::Native,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallingMode {
    Native,
    TextFallback,
    NativeWithFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionSettings {
    pub mode: PermissionMode,
    /// Hard-blocked regardless of prompt/allow-list state. Not yet
    /// enforced this pass — no tools exist to gate — but validated on
    /// load so the shape is settled.
    pub deny_paths: Vec<String>,
    pub max_tool_iterations_per_turn: u32,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Confirm,
            deny_paths: vec!["~/.ssh".to_string(), "~/.aws".to_string()],
            max_tool_iterations_per_turn: 25,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Confirm,
}

impl Settings {
    pub fn config_path() -> Result<PathBuf, ConfigError> {
        let dirs = ProjectDirs::from("", "", "aivyx-coder").ok_or(ConfigError::NoConfigDir)?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Loads settings from the XDG config file, writing it out with
    /// defaults on first run so the user has a real file to edit rather
    /// than an invisible set of built-in defaults.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;

        if !path.exists() {
            let defaults = Self::default();
            defaults.write_to(&path)?;
            return Ok(defaults);
        }

        let raw = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| ConfigError::Parse { path, source })
    }

    fn write_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let toml_string = toml::to_string_pretty(self)?;
        fs::write(path, toml_string).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// CLI flags win over the config file when present.
    pub fn apply_overrides(&mut self, base_url: Option<String>, model: Option<String>) {
        if let Some(base_url) = base_url {
            self.backend.base_url = base_url;
        }
        if let Some(model) = model {
            self.backend.model = model;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_round_trip_through_toml() {
        let settings = Settings::default();
        let toml_string = toml::to_string_pretty(&settings).unwrap();
        let parsed: Settings = toml::from_str(&toml_string).unwrap();
        assert_eq!(parsed.backend.base_url, settings.backend.base_url);
        assert_eq!(parsed.backend.model, settings.backend.model);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults_for_missing_fields() {
        let partial = r#"
            [backend]
            model = "custom-model"
        "#;
        let parsed: Settings = toml::from_str(partial).unwrap();
        assert_eq!(parsed.backend.model, "custom-model");
        assert_eq!(parsed.backend.base_url, BackendSettings::default().base_url);
        assert_eq!(
            parsed.permissions.deny_paths,
            PermissionSettings::default().deny_paths
        );
    }

    #[test]
    fn cli_overrides_win_over_config_file_values() {
        let mut settings = Settings::default();
        settings.apply_overrides(Some("http://localhost:8080/v1".to_string()), None);
        assert_eq!(settings.backend.base_url, "http://localhost:8080/v1");
        assert_eq!(settings.backend.model, BackendSettings::default().model);
    }
}
