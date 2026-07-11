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
    pub sandbox: SandboxSettings,
    pub git: GitSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitSettings {
    /// Snapshot the worktree to `refs/aivyx/checkpoints/*` before every
    /// mutating tool call, so any agent change (including arbitrary
    /// `run_shell` effects) can be rewound with plain git commands. Never
    /// touches HEAD, the index, or the worktree; silently disabled when the
    /// working directory isn't a git repository.
    pub checkpoints: bool,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self { checkpoints: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxSettings {
    /// Additional filesystem paths process-executing tools may read from,
    /// beyond the built-in default (the working directory plus common
    /// system/toolchain paths). Empty by default — an escape hatch for
    /// ecosystems the built-in list doesn't cover (e.g. a Python venv or
    /// node_modules cache living outside the project directory), not a
    /// fully general reconfigurable policy.
    pub extra_read_paths: Vec<String>,
    /// Whether process-execution tools must refuse to run at all if real
    /// Landlock confinement can't actually be established (kernel support
    /// missing/disabled, or the kernel only partially enforces the
    /// requested ruleset) — fail closed rather than silently running
    /// unconfined. Defaults to `true`: a coding agent whose core value
    /// proposition includes real sandboxing should not silently degrade to
    /// no sandboxing at all without the user explicitly opting into that.
    pub require_enforcement: bool,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            extra_read_paths: Vec::new(),
            require_enforcement: true,
        }
    }
}

impl SandboxSettings {
    /// Expands a leading `~` in each `extra_read_paths` entry — see
    /// `PermissionSettings::resolved_deny_paths` for the same convention.
    pub fn resolved_extra_read_paths(&self) -> Vec<PathBuf> {
        resolve_tilde_paths(&self.extra_read_paths)
    }
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
    /// The model's context window, in tokens — used to show a live budget
    /// indicator and to trigger context compaction before the window
    /// overflows. Conservative default; **set this to your model's actual
    /// window** (qwen3.5/qwen3.6 support far more than the default). Not
    /// auto-detected: the OpenAI-compatible `/v1` surface doesn't expose it
    /// reliably across Ollama/vLLM/llama.cpp.
    pub context_tokens: u32,
}

impl Default for BackendSettings {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
            api_key: None,
            tool_calling_mode: ToolCallingMode::Native,
            context_tokens: 8192,
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
    /// Hard-blocked regardless of prompt/allow-list state, enforced by
    /// `ConfirmationGate` before any prompt or Always-Allow cache lookup.
    /// Entries may use a leading `~` for the home directory — see
    /// `resolved_deny_paths`.
    pub deny_paths: Vec<String>,
    pub max_tool_iterations_per_turn: u32,
    /// Fixed set of commands the `run_command` tool may execute — the model
    /// selects one by `name`, it never supplies a program or arbitrary args.
    /// Empty by default: nothing is runnable until a project explicitly
    /// opts in.
    pub allowed_commands: Vec<AllowedCommand>,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Confirm,
            deny_paths: vec!["~/.ssh".to_string(), "~/.aws".to_string()],
            max_tool_iterations_per_turn: 25,
            allowed_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowedCommand {
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Overrides the tool's default timeout for this command when set.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl PermissionSettings {
    /// Expands a leading `~` (home directory) in each `deny_paths` entry
    /// into an absolute `PathBuf`. Entries that can't be expanded (no home
    /// directory found) are skipped rather than left unresolved and
    /// silently wrong.
    pub fn resolved_deny_paths(&self) -> Vec<PathBuf> {
        resolve_tilde_paths(&self.deny_paths)
    }
}

/// Shared by `PermissionSettings::resolved_deny_paths` and
/// `SandboxSettings::resolved_extra_read_paths`: expands a leading `~` into
/// an absolute path, skipping entries that can't be expanded (no home
/// directory found) rather than leaving them unresolved and silently wrong.
/// Every result is also symlink-canonicalized (see `resolve_symlinks`) —
/// without this, a symlinked entry (e.g. `~/.ssh` symlinked via a dotfile
/// manager) would never match the fully-resolved paths every tool actually
/// checks against, making the entry a silent no-op.
fn resolve_tilde_paths(raw_paths: &[String]) -> Vec<PathBuf> {
    let home_dir = directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());

    raw_paths
        .iter()
        .filter_map(|raw| {
            let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                home_dir.as_ref().map(|home| home.join(rest))
            } else if raw == "~" {
                home_dir.clone()
            } else if raw.starts_with('~') {
                // `~username`-style expansion (another user's home
                // directory) isn't supported. Failing loudly (skip + warn)
                // is safer than silently keeping it as a literal string —
                // no real filesystem path is relative and tilde-prefixed,
                // so it would otherwise be a permanently no-op entry with
                // no indication anything was wrong.
                tracing::warn!(
                    entry = %raw,
                    "unsupported ~username path syntax in config; skipping this entry"
                );
                None
            } else {
                Some(PathBuf::from(raw))
            };
            expanded.map(|path| resolve_symlinks(&path))
        })
        .collect()
}

/// Canonicalizes as much of `path` as exists, then re-appends whatever
/// doesn't. Mirrors `aivyx-tools`'s `path_resolve::resolve_symlinks`
/// exactly — duplicated here rather than having this lower-level config
/// crate depend on the tools crate for one small helper. Keep the two in
/// sync if either changes.
fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut result = canonical;
            for component in tail.into_iter().rev() {
                result.push(component);
            }
            return result;
        }

        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent;
            }
            _ => return path.to_path_buf(),
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
        })?;

        // The config may contain a plaintext `backend.api_key` — restrict
        // it to owner-read-write rather than leaving it at the OS/umask
        // default (commonly world-readable).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }

        Ok(())
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

    #[test]
    fn tilde_prefixed_deny_paths_expand_to_the_home_directory() {
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .canonicalize()
            .expect("$HOME must exist");
        let settings = PermissionSettings {
            deny_paths: vec!["~/.ssh".to_string()],
            ..PermissionSettings::default()
        };

        // `~/.ssh` need not exist for this test — `resolve_symlinks` walks
        // up to the nearest existing ancestor (home itself) and re-appends
        // the rest, same as `path_resolve::resolve` does for tools.
        assert_eq!(settings.resolved_deny_paths(), vec![home.join(".ssh")]);
    }

    #[test]
    fn non_tilde_deny_paths_pass_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("shadow");
        std::fs::write(&target, "").unwrap();
        let settings = PermissionSettings {
            deny_paths: vec![target.to_str().unwrap().to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(
            settings.resolved_deny_paths(),
            vec![target.canonicalize().unwrap()]
        );
    }

    #[test]
    fn tilde_username_syntax_is_skipped_not_treated_as_literal() {
        let settings = PermissionSettings {
            deny_paths: vec!["~root/.ssh".to_string()],
            ..PermissionSettings::default()
        };

        assert!(settings.resolved_deny_paths().is_empty());
    }

    #[test]
    fn require_enforcement_defaults_to_true() {
        assert!(SandboxSettings::default().require_enforcement);
    }

    #[test]
    fn config_file_is_written_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let settings = Settings::default();

        settings.write_to(&path).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
