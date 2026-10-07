//! Config packs: a signed, configuration-only bundle (the shared
//! `aivyx-pack` format, `format = 2`) whose aivyx-coder part — an
//! `AGENTS.md`, skills, an optional roster and MCP servers — is installed
//! once and then switched on per project as a layer over the user's own
//! setup. Nothing here ever edits `config.toml`, an `AGENTS.md`, or any file
//! in a project.
//!
//! Layout, beside `config.toml`: `packs/<name>/<version>/` (one version per
//! pack) and `packs/active.toml` (which project uses which pack, and the MCP
//! servers the user approved).

use std::path::{Path, PathBuf};

use aivyx_config::{McpServerConfig, Settings};
use aivyx_pack::{ConfigPackManifest, Manifest};
use serde::{Deserialize, Serialize};

/// The folder installed packs live in.
pub struct PacksDir(PathBuf);

/// One installed pack.
#[derive(Debug, Clone)]
pub struct Installed {
    pub name: String,
    pub version: String,
    /// `packs/<name>/<version>`.
    pub dir: PathBuf,
    pub manifest: ConfigPackManifest,
}

impl PacksDir {
    /// `packs/` beside the user's `config.toml`.
    pub fn user() -> anyhow::Result<Self> {
        let config = Settings::config_path()?;
        let dir = config
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config.toml has no parent folder"))?
            .join("packs");
        Ok(PacksDir(dir))
    }

    #[cfg(test)]
    pub fn at(root: PathBuf) -> Self {
        PacksDir(root)
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.0
    }

    /// Every installed pack, by name.
    pub fn installed(&self) -> Vec<Installed> {
        let Ok(names) = std::fs::read_dir(&self.0) else {
            return Vec::new();
        };
        let mut out: Vec<Installed> = names
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| self.find(&name))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The installed version of `name`, if any.
    pub fn find(&self, name: &str) -> Option<Installed> {
        let versions = std::fs::read_dir(self.0.join(name)).ok()?;
        versions
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .find_map(|e| {
                let text = std::fs::read_to_string(e.path().join("manifest.toml")).ok()?;
                let Manifest::Config(manifest) = Manifest::parse(&text).ok()? else {
                    return None;
                };
                (manifest.name == name).then(|| Installed {
                    name: manifest.name.clone(),
                    version: manifest.version.clone(),
                    dir: e.path(),
                    manifest,
                })
            })
    }
}

/// What a good aivyx-coder part holds.
#[derive(Debug)]
pub struct CoderPartSummary {
    pub manifest: ConfigPackManifest,
    pub skills: Vec<String>,
    /// Members of the roster; 0 when the pack has none.
    pub roster_members: usize,
    pub mcp_servers: Vec<McpServerConfig>,
}

/// `coder.mcp`'s shape: `[[servers]]` blocks, as in `config.toml`'s `[mcp]`.
#[derive(Deserialize)]
struct McpFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

/// The MCP servers a pack folder declares (empty if it declares none or
/// the file doesn't parse — `check_coder_part` reports that).
pub fn pack_mcp_servers(dir: &Path, manifest: &ConfigPackManifest) -> Vec<McpServerConfig> {
    manifest
        .coder
        .as_ref()
        .and_then(|c| c.mcp.as_ref())
        .and_then(|rel| std::fs::read_to_string(dir.join(rel)).ok())
        .and_then(|text| toml::from_str::<McpFile>(&text).ok())
        .map(|f| f.servers)
        .unwrap_or_default()
}

/// Check the aivyx-coder part of a pack folder. `Err` lists every problem.
pub fn check_coder_part(dir: &Path) -> Result<CoderPartSummary, Vec<String>> {
    let text = std::fs::read_to_string(dir.join("manifest.toml"))
        .map_err(|e| vec![format!("can't read manifest.toml: {e}")])?;
    let manifest = match Manifest::parse(&text).map_err(|e| vec![e.to_string()])? {
        Manifest::Config(m) => m,
        Manifest::Binary(_) => {
            return Err(vec!["this is a tool pack for aivyx-pa, not a config pack".into()]);
        }
    };
    let Some(coder) = manifest.coder.clone() else {
        return Err(vec!["this pack has no aivyx-coder part — it's for aivyx-pa".into()]);
    };

    let mut problems = Vec::new();

    match std::fs::read_to_string(dir.join(&coder.agents_file)) {
        Ok(text) if !text.trim().is_empty() => {}
        Ok(_) => problems.push(format!("{} is empty", coder.agents_file)),
        Err(e) => problems.push(format!("can't read {}: {e}", coder.agents_file)),
    }

    let mut skills = Vec::new();
    if let Some(rel) = &coder.skills {
        let skills_dir = dir.join(rel);
        let loader = aivyx_skills::SkillLoader::new().with_project_dir(skills_dir.clone());
        let mut names: Vec<String> = std::fs::read_dir(&skills_dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().join("SKILL.md").is_file())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        if names.is_empty() {
            problems.push(format!("the skills folder {rel} has no skills"));
        }
        for name in names {
            let ours = loader.get(&name).is_some_and(|s| {
                s.source == aivyx_skills::SkillSource::Project && !s.body.trim().is_empty()
            });
            if ours {
                skills.push(name);
            } else {
                problems.push(format!(
                    "skill `{name}` doesn't parse (it needs `name` and `description` \
                     frontmatter, and `name` must match its folder)"
                ));
            }
        }
    }

    let mut roster_members = 0;
    if let Some(rel) = &coder.roster {
        match std::fs::read_to_string(dir.join(rel))
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str::<aivyx_team::TeamConfig>(&t).map_err(|e| e.to_string()))
        {
            Err(e) => problems.push(format!("the roster doesn't load: {e}")),
            Ok(team) => {
                // The tools a roster may name depend on the user's setup, so
                // only its shape is checked here; the full check runs when
                // the pack is in use, as for any roster.
                let named: Vec<&str> = team
                    .members
                    .iter()
                    .flat_map(|m| m.tool_allowlist.iter().map(String::as_str))
                    .collect();
                match team.validate(&named) {
                    Ok(()) => roster_members = team.members.len(),
                    Err(e) => problems.push(format!("the roster is invalid: {e}")),
                }
            }
        }
    }

    let mut mcp_servers = Vec::new();
    if let Some(rel) = &coder.mcp {
        match std::fs::read_to_string(dir.join(rel))
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str::<McpFile>(&t).map_err(|e| e.to_string()))
        {
            Err(e) => problems.push(format!("the MCP file doesn't load: {e}")),
            Ok(file) => {
                for (i, s) in file.servers.iter().enumerate() {
                    if s.name.trim().is_empty() || s.command.trim().is_empty() {
                        problems.push(format!("MCP server #{} needs a name and a command", i + 1));
                    } else if file.servers[..i].iter().any(|o| o.name == s.name) {
                        problems.push(format!("MCP server `{}` is listed twice", s.name));
                    }
                }
                mcp_servers = file.servers;
            }
        }
    }

    if problems.is_empty() {
        Ok(CoderPartSummary { manifest, skills, roster_members, mcp_servers })
    } else {
        Err(problems)
    }
}

/// `[pack] trusted_publishers`, each checked to be a real key.
pub fn trusted_publishers(settings: &Settings) -> Result<Vec<String>, String> {
    for key in &settings.pack.trusted_publishers {
        if aivyx_pack::decode_verifying_key(key).is_none() {
            return Err(format!(
                "[pack] trusted_publishers entry {key:?} isn't a base64 Ed25519 key \
                 (as printed by `aivyx-pack keygen`)"
            ));
        }
    }
    Ok(settings.pack.trusted_publishers.clone())
}

fn scratch(parent: &Path) -> PathBuf {
    parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// Verify, check and install a pack's aivyx-coder part, replacing any
/// older version. Leaves nothing behind on failure.
pub fn install(packs: &PacksDir, file: &Path, trusted: &[String]) -> Result<Installed, String> {
    let bundle = aivyx_pack::read_bundle(file).map_err(|e| e.to_string())?;
    aivyx_pack::verify_bundle(&bundle, trusted).map_err(|e| e.to_string())?;
    let manifest = match aivyx_pack::read_any_manifest(&bundle.payload).map_err(|e| e.to_string())? {
        Manifest::Config(m) => m,
        Manifest::Binary(_) => return Err("this is a tool pack for aivyx-pa, not a config pack".into()),
    };
    let coder = manifest
        .coder
        .as_ref()
        .ok_or("this pack has no aivyx-coder part — it's for aivyx-pa")?;
    aivyx_pack::daemon_version_ok(&coder.min_version, env!("CARGO_PKG_VERSION"))
        .map_err(|_| {
            format!(
                "this pack needs aivyx-coder {} or later (this is {}) — upgrade first",
                coder.min_version,
                env!("CARGO_PKG_VERSION")
            )
        })?;

    let home = packs.0.join(&manifest.name);
    let tmp = scratch(&home);
    let staged = (|| -> Result<(), String> {
        std::fs::create_dir_all(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        aivyx_pack::unpack_payload(&bundle.payload, &tmp).map_err(|e| e.to_string())?;
        check_coder_part(&tmp).map_err(|problems| {
            format!("the pack doesn't pass its checks:\n  - {}", problems.join("\n  - "))
        })?;
        Ok(())
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir(&home);
        return Err(e);
    }
    // One version per pack: drop the others, then move the new one in.
    for entry in std::fs::read_dir(&home).map_err(|e| e.to_string())?.flatten() {
        if entry.path() != tmp {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let dir = home.join(&manifest.version);
    std::fs::rename(&tmp, &dir).map_err(|e| format!("move into {}: {e}", dir.display()))?;
    Ok(Installed { name: manifest.name.clone(), version: manifest.version.clone(), dir, manifest })
}

/// Delete an installed pack (every version) and switch it off everywhere.
pub fn remove(packs: &PacksDir, name: &str) -> Result<(), String> {
    let home = packs.0.join(name);
    if packs.find(name).is_none() || !home.is_dir() {
        return Err(format!("pack `{name}` isn't installed — see `aivyx-coder pack list`"));
    }
    std::fs::remove_dir_all(&home).map_err(|e| format!("remove {}: {e}", home.display()))?;
    let mut state = ActiveState::load(packs);
    let before = state.uses.len();
    state.uses.retain(|u| u.pack != name);
    if state.uses.len() != before {
        state.save(packs).map_err(|e| format!("save packs/active.toml: {e}"))?;
    }
    Ok(())
}

/// `pack inspect`: the shared description plus what the checks found.
pub fn render_inspect(payload: &[u8]) -> Result<String, String> {
    let manifest = aivyx_pack::read_any_manifest(payload).map_err(|e| e.to_string())?;
    let mut out = aivyx_pack::describe::describe(&manifest);
    if let Manifest::Config(m) = &manifest
        && m.coder.is_some()
    {
        let dir = scratch(&std::env::temp_dir());
        let result = aivyx_pack::unpack_payload(payload, &dir)
            .map_err(|e| vec![e.to_string()])
            .and_then(|()| check_coder_part(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Ok(s) => {
                out.push_str(&format!(
                    "  skills: {} · roster members: {} · MCP servers: {}\n  checks: OK\n",
                    s.skills.len(),
                    s.roster_members,
                    s.mcp_servers.len()
                ));
                for server in &s.mcp_servers {
                    out.push_str(&format!("    MCP {}: {}\n", server.name, command_line(server)));
                }
            }
            Err(problems) => {
                out.push_str(&format!(
                    "  checks: {} problem{} — this pack won't install\n",
                    problems.len(),
                    if problems.len() == 1 { "" } else { "s" }
                ));
                for p in problems {
                    out.push_str(&format!("    - {p}\n"));
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Switching a pack on: packs/active.toml
// ---------------------------------------------------------------------------

/// The scope that means "every project".
pub const GLOBAL: &str = "*";

/// `packs/active.toml`: which pack each project uses.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ActiveState {
    #[serde(default, rename = "use")]
    pub uses: Vec<Use>,
}

/// One project's (or every project's) pack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Use {
    /// A canonical project path, or [`GLOBAL`].
    pub scope: String,
    pub pack: String,
    /// MCP servers the user approved, with the exact command approved.
    #[serde(default)]
    pub mcp: Vec<ApprovedServer>,
}

/// An MCP server the user agreed to run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovedServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl ApprovedServer {
    fn of(server: &McpServerConfig) -> Self {
        ApprovedServer {
            name: server.name.clone(),
            command: server.command.clone(),
            args: server.args.clone(),
        }
    }

    /// Still exactly what the user approved?
    pub fn matches(&self, server: &McpServerConfig) -> bool {
        *self == Self::of(server)
    }
}

impl ActiveState {
    fn path(packs: &PacksDir) -> PathBuf {
        packs.0.join("active.toml")
    }

    /// Missing → empty; unreadable → empty, with a warning in the log.
    pub fn load(packs: &PacksDir) -> Self {
        match std::fs::read_to_string(Self::path(packs)) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable packs/active.toml: {e}");
                ActiveState::default()
            }),
            Err(_) => ActiveState::default(),
        }
    }

    /// Owner-only, like `config.toml`.
    pub fn save(&self, packs: &PacksDir) -> std::io::Result<()> {
        std::fs::create_dir_all(&packs.0)?;
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let path = Self::path(packs);
        std::fs::write(&path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// The pack for `cwd`: this project's own wins over the global one.
    pub fn for_project(&self, cwd: &Path) -> Option<&Use> {
        let scope = project_scope(cwd);
        self.uses
            .iter()
            .find(|u| u.scope == scope)
            .or_else(|| self.uses.iter().find(|u| u.scope == GLOBAL))
    }
}

/// The scope string for a project: its canonical path (as sessions key it).
pub fn project_scope(cwd: &Path) -> String {
    std::fs::canonicalize(cwd)
        .unwrap_or_else(|_| cwd.to_path_buf())
        .display()
        .to_string()
}

/// Switch `name` on for `scope`, asking about each of its MCP servers.
/// Replaces whatever that scope used before.
pub fn use_pack(
    packs: &PacksDir,
    name: &str,
    scope: String,
    ask: &mut dyn FnMut(&McpServerConfig) -> bool,
) -> Result<Use, String> {
    let installed = packs
        .find(name)
        .ok_or_else(|| format!("pack `{name}` isn't installed — see `aivyx-coder pack list`"))?;
    let mcp = pack_mcp_servers(&installed.dir, &installed.manifest)
        .iter()
        .filter(|server| ask(server))
        .map(ApprovedServer::of)
        .collect();
    let new_use = Use { scope: scope.clone(), pack: name.to_string(), mcp };
    let mut state = ActiveState::load(packs);
    state.uses.retain(|u| u.scope != scope);
    state.uses.push(new_use.clone());
    state.save(packs).map_err(|e| format!("save packs/active.toml: {e}"))?;
    Ok(new_use)
}

/// Switch off whatever `scope` uses. `false` = nothing was on there.
pub fn off(packs: &PacksDir, scope: &str) -> Result<bool, String> {
    let mut state = ActiveState::load(packs);
    let before = state.uses.len();
    state.uses.retain(|u| u.scope != scope);
    if state.uses.len() == before {
        return Ok(false);
    }
    state.save(packs).map_err(|e| format!("save packs/active.toml: {e}"))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// The pack layer at startup
// ---------------------------------------------------------------------------

/// What a pack in use here adds to this session.
#[derive(Debug)]
pub struct PackLayer {
    pub name: String,
    pub version: String,
    pub agents_file: PathBuf,
    pub skills_dir: Option<PathBuf>,
    pub roster: Option<PathBuf>,
    /// Approved and unchanged since approval.
    pub mcp_servers: Vec<McpServerConfig>,
    /// Shown at startup.
    pub notices: Vec<String>,
}

/// The pack in use for `cwd`, if any, as a layer (or a notice-only layer
/// when the pack in use isn't installed any more).
pub fn resolve_layer(packs: &PacksDir, cwd: &Path) -> Option<PackLayer> {
    let state = ActiveState::load(packs);
    let used = state.for_project(cwd)?;
    let Some(installed) = packs.find(&used.pack) else {
        return Some(PackLayer {
            name: used.pack.clone(),
            version: String::new(),
            agents_file: PathBuf::new(),
            skills_dir: None,
            roster: None,
            mcp_servers: Vec::new(),
            notices: vec![format!(
                "Pack {} is in use here but isn't installed any more — run \
                 'aivyx-coder pack off' (or install it again).",
                used.pack
            )],
        });
    };
    let coder = installed.manifest.coder.as_ref()?;
    let mut notices = vec![format!("Using pack {} v{} here.", installed.name, installed.version)];
    let mut mcp_servers = Vec::new();
    for server in pack_mcp_servers(&installed.dir, &installed.manifest) {
        match used.mcp.iter().find(|a| a.name == server.name) {
            Some(approved) if approved.matches(&server) => mcp_servers.push(server),
            Some(_) => notices.push(format!(
                "Pack {}'s MCP server {} changed since you approved it, so it isn't running — \
                 run 'aivyx-coder pack use {}' again.",
                installed.name, server.name, installed.name
            )),
            None => {}
        }
    }
    Some(PackLayer {
        name: installed.name.clone(),
        version: installed.version.clone(),
        agents_file: installed.dir.join(&coder.agents_file),
        skills_dir: coder.skills.as_ref().map(|r| installed.dir.join(r)),
        roster: coder.roster.as_ref().map(|r| installed.dir.join(r)),
        mcp_servers,
        notices,
    })
}

/// Apply the layer to this session's settings (in memory only): the roster
/// only if `[team] roster_path` is unset; the skills into the unset skills
/// slot (project, then user); approved MCP servers unless the user already
/// has one of that name. Anything not applied gets a notice.
pub fn apply_to_settings(layer: &mut PackLayer, settings: &mut Settings) {
    if let Some(roster) = &layer.roster {
        if settings.team.roster_path.is_none() {
            settings.team.roster_path = Some(roster.display().to_string());
        } else {
            layer.notices.push(format!(
                "Pack {}'s roster isn't used: [team] roster_path is set.",
                layer.name
            ));
        }
    }
    if let Some(dir) = &layer.skills_dir {
        let dir = dir.display().to_string();
        if settings.skills.project_dir.is_none() {
            settings.skills.project_dir = Some(dir);
        } else if settings.skills.user_dir.is_none() {
            settings.skills.user_dir = Some(dir);
        } else {
            layer.notices.push(format!(
                "Pack {}'s skills aren't loaded: [skills] project_dir and user_dir are both set.",
                layer.name
            ));
        }
    }
    for server in &layer.mcp_servers {
        if settings.mcp.servers.iter().any(|s| s.name == server.name) {
            layer.notices.push(format!(
                "Pack {}'s MCP server {} isn't started: you already have one with that name.",
                layer.name, server.name
            ));
        } else {
            settings.mcp.servers.push(server.clone());
        }
    }
}

/// `pack list`: each installed pack, its version and where it's in use.
pub fn render_list(packs: &PacksDir, cwd: &Path) -> String {
    let installed = packs.installed();
    if installed.is_empty() {
        return "No packs installed. Install one with `aivyx-coder pack install <file>`.\n".into();
    }
    let state = ActiveState::load(packs);
    let here = project_scope(cwd);
    let mut out = String::new();
    for p in installed {
        let wheres: Vec<String> = state
            .uses
            .iter()
            .filter(|u| u.pack == p.name)
            .map(|u| match u.scope.as_str() {
                GLOBAL => "all projects".to_string(),
                s if s == here => "this project".to_string(),
                s => s.to_string(),
            })
            .collect();
        let used = if wheres.is_empty() { "not in use".to_string() } else { format!("in use: {}", wheres.join(", ")) };
        out.push_str(&format!("{} v{} — {used}\n", p.name, p.version));
    }
    out
}

/// `command arg arg` for showing an MCP server to the user.
pub fn command_line(server: &McpServerConfig) -> String {
    std::iter::once(server.command.as_str())
        .chain(server.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `pack check`: what it prints.
pub fn render_check(result: &Result<CoderPartSummary, Vec<String>>) -> String {
    match result {
        Ok(s) => format!(
            "✓ {} v{} — aivyx-coder part looks good\n  skills: {} · roster members: {} · MCP servers: {}\n",
            s.manifest.name,
            s.manifest.version,
            s.skills.len(),
            s.roster_members,
            s.mcp_servers.len()
        ),
        Err(problems) => {
            let mut out = format!(
                "✗ {} problem{} in the aivyx-coder part:\n",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            );
            for p in problems {
                out.push_str(&format!("  - {p}\n"));
            }
            out
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aivyx-coder-packs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) const MANIFEST: &str = r#"format = 2
name = "business-manager"
version = "0.1.0"
publisher = "Aivyx"
products = ["coder"]

[coder]
min_version = "0.4.0"
agents_file = "coder/AGENTS.md"
skills = "coder/skills"
mcp = "coder/mcp.toml"
"#;

    pub(crate) fn good_pack(tag: &str) -> PathBuf {
        let dir = tmp(tag);
        for (rel, body) in [
            ("manifest.toml", MANIFEST),
            ("coder/AGENTS.md", "Explain changes in plain language.\n"),
            (
                "coder/skills/example/SKILL.md",
                "---\nname: example\ndescription: An example skill.\n---\n\nDo the example thing.\n",
            ),
            (
                "coder/mcp.toml",
                "[[servers]]\nname = \"files\"\ncommand = \"mcp-files\"\nargs = [\"--root\", \".\"]\n",
            ),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    pub(crate) fn rewrite(dir: &Path, rel: &str, f: impl Fn(String) -> String) {
        let path = dir.join(rel);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(path, f(text)).unwrap();
    }

    /// A signed bundle of `dir`, and the key that signed it.
    pub(crate) fn signed(dir: &Path) -> (PathBuf, String) {
        let out = tmp("bundle");
        let key = out.join("key.bin");
        let pubkey = aivyx_pack::keygen_to_file(&key).unwrap();
        let signing = aivyx_pack::load_signing_key(&key).unwrap();
        let bundle = out.join("pack.aivyxpack");
        aivyx_pack::write_bundle(&aivyx_pack::build_payload(dir).unwrap(), &signing, &bundle).unwrap();
        (bundle, pubkey)
    }

    #[test]
    fn a_good_coder_part_passes() {
        let s = check_coder_part(&good_pack("good")).unwrap();
        assert_eq!(s.skills, vec!["example"]);
        assert_eq!(s.mcp_servers.len(), 1);
        assert_eq!(command_line(&s.mcp_servers[0]), "mcp-files --root .");
        assert!(render_check(&Ok(s)).contains("skills: 1 · roster members: 0 · MCP servers: 1"));
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let d = good_pack("many");
        rewrite(&d, "coder/AGENTS.md", |_| "  \n".into());
        rewrite(&d, "coder/skills/example/SKILL.md", |t| t.replace("name: example", "name: other"));
        rewrite(&d, "coder/mcp.toml", |t| format!("{t}\n{t}"));
        let problems = check_coder_part(&d).unwrap_err();
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    #[test]
    fn a_pa_only_pack_is_explained() {
        let d = good_pack("paonly");
        rewrite(&d, "manifest.toml", |_| {
            "format = 2\nname = \"p\"\nversion = \"0.1.0\"\npublisher = \"A\"\nproducts = [\"pa\"]\n\n\
             [pa]\nmin_version = \"0.18.0\"\ntemplate = \"pa/aivyx-pa.toml\"\n"
                .into()
        });
        assert!(check_coder_part(&d).unwrap_err()[0].contains("for aivyx-pa"));
    }

    #[test]
    fn install_unpacks_and_replaces_older_versions() {
        let packs = PacksDir::at(tmp("install-root"));
        let d = good_pack("install");
        let (bundle, key) = signed(&d);
        let first = install(&packs, &bundle, std::slice::from_ref(&key)).unwrap();
        assert!(first.dir.join("coder/AGENTS.md").is_file());
        rewrite(&d, "manifest.toml", |t| t.replace(r#"version = "0.1.0""#, r#"version = "0.2.0""#));
        let (bundle, key) = signed(&d);
        let second = install(&packs, &bundle, &[key]).unwrap();
        assert_eq!(second.version, "0.2.0");
        let versions: Vec<_> = std::fs::read_dir(packs.root().join("business-manager"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(versions, vec!["0.2.0"]);
        assert_eq!(packs.find("business-manager").unwrap().version, "0.2.0");
        assert_eq!(packs.installed().len(), 1);
    }

    #[test]
    fn an_untrusted_pack_is_refused_and_nothing_is_left() {
        let packs = PacksDir::at(tmp("untrusted-root"));
        let (bundle, _key) = signed(&good_pack("untrusted"));
        let err = install(&packs, &bundle, &[]).unwrap_err();
        assert!(err.contains("not trusted"), "{err}");
        assert!(packs.installed().is_empty());
    }

    #[test]
    fn a_failing_pack_leaves_nothing() {
        let packs = PacksDir::at(tmp("failing-root"));
        let d = good_pack("failing");
        rewrite(&d, "coder/AGENTS.md", |_| " ".into());
        let (bundle, key) = signed(&d);
        let err = install(&packs, &bundle, &[key]).unwrap_err();
        assert!(err.contains("is empty"), "{err}");
        assert!(!packs.root().join("business-manager").exists());
    }

    #[test]
    fn a_pack_needing_a_newer_aivyx_coder_is_refused() {
        let packs = PacksDir::at(tmp("newer-root"));
        let d = good_pack("newer");
        rewrite(&d, "manifest.toml", |t| t.replace(r#"min_version = "0.4.0""#, r#"min_version = "99.0.0""#));
        let (bundle, key) = signed(&d);
        assert!(install(&packs, &bundle, &[key]).unwrap_err().contains("upgrade"));
    }

    #[test]
    fn remove_deletes_the_pack() {
        let packs = PacksDir::at(tmp("remove-root"));
        let (bundle, key) = signed(&good_pack("remove"));
        install(&packs, &bundle, &[key]).unwrap();
        remove(&packs, "business-manager").unwrap();
        assert!(packs.find("business-manager").is_none());
        assert!(remove(&packs, "business-manager").is_err());
    }

    fn installed_packs(tag: &str) -> PacksDir {
        let packs = PacksDir::at(tmp(&format!("{tag}-root")));
        let (bundle, key) = signed(&good_pack(tag));
        install(&packs, &bundle, &[key]).unwrap();
        packs
    }

    #[test]
    fn use_records_the_project_and_approved_servers() {
        let packs = installed_packs("use");
        let project = tmp("use-project");
        let scope = project_scope(&project);
        let used = use_pack(&packs, "business-manager", scope, &mut |_| true).unwrap();
        assert_eq!(used.mcp.len(), 1);
        assert_eq!(used.mcp[0].command, "mcp-files");
        let state = ActiveState::load(&packs);
        assert_eq!(state.for_project(&project), Some(&used));
        assert!(render_list(&packs, &project).contains("business-manager v0.1.0 — in use: this project"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(packs.root().join("active.toml")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_declined_server_is_not_recorded() {
        let packs = installed_packs("decline");
        let used = use_pack(&packs, "business-manager", GLOBAL.into(), &mut |_| false).unwrap();
        assert!(used.mcp.is_empty());
    }

    #[test]
    fn a_project_use_beats_the_global_one() {
        let packs = installed_packs("precedence");
        let project = tmp("precedence-project");
        let other = tmp("precedence-other");
        use_pack(&packs, "business-manager", GLOBAL.into(), &mut |_| false).unwrap();
        use_pack(&packs, "business-manager", project_scope(&project), &mut |_| true).unwrap();
        let state = ActiveState::load(&packs);
        assert_eq!(state.for_project(&project).unwrap().mcp.len(), 1);
        assert_eq!(state.for_project(&other).unwrap().scope, GLOBAL);
    }

    #[test]
    fn off_removes_only_that_scope() {
        let packs = installed_packs("off");
        let project = tmp("off-project");
        use_pack(&packs, "business-manager", GLOBAL.into(), &mut |_| false).unwrap();
        use_pack(&packs, "business-manager", project_scope(&project), &mut |_| false).unwrap();
        assert!(off(&packs, &project_scope(&project)).unwrap());
        assert!(!off(&packs, &project_scope(&project)).unwrap(), "already off");
        assert_eq!(ActiveState::load(&packs).for_project(&project).unwrap().scope, GLOBAL);
    }

    #[test]
    fn using_a_pack_that_isnt_installed_is_an_error() {
        let packs = PacksDir::at(tmp("missing-root"));
        assert!(use_pack(&packs, "nope", GLOBAL.into(), &mut |_| true).unwrap_err().contains("isn't installed"));
    }

    #[test]
    fn removing_a_pack_switches_it_off() {
        let packs = installed_packs("remove-off");
        use_pack(&packs, "business-manager", GLOBAL.into(), &mut |_| false).unwrap();
        remove(&packs, "business-manager").unwrap();
        assert!(ActiveState::load(&packs).uses.is_empty());
    }

    #[test]
    fn resolve_layer_returns_the_projects_pack() {
        let packs = installed_packs("layer");
        let project = tmp("layer-project");
        use_pack(&packs, "business-manager", project_scope(&project), &mut |_| true).unwrap();
        let layer = resolve_layer(&packs, &project).unwrap();
        assert_eq!(layer.name, "business-manager");
        assert!(layer.agents_file.is_file());
        assert!(layer.skills_dir.as_ref().unwrap().is_dir());
        assert_eq!(layer.mcp_servers.len(), 1);
        assert_eq!(layer.notices, vec!["Using pack business-manager v0.1.0 here."]);
        assert!(resolve_layer(&packs, &tmp("layer-elsewhere")).is_none(), "not in use there");
    }

    #[test]
    fn a_changed_mcp_command_is_not_started() {
        let packs = installed_packs("changed");
        let project = tmp("changed-project");
        use_pack(&packs, "business-manager", project_scope(&project), &mut |_| true).unwrap();
        let mut state = ActiveState::load(&packs);
        state.uses[0].mcp[0].args = vec!["--root".into(), "/".into()];
        state.save(&packs).unwrap();
        let layer = resolve_layer(&packs, &project).unwrap();
        assert!(layer.mcp_servers.is_empty());
        assert!(layer.notices.iter().any(|n| n.contains("changed since you approved it")));
    }

    #[test]
    fn a_missing_installed_pack_gives_a_notice() {
        let packs = installed_packs("gone");
        use_pack(&packs, "business-manager", GLOBAL.into(), &mut |_| false).unwrap();
        std::fs::remove_dir_all(packs.root().join("business-manager")).unwrap();
        let layer = resolve_layer(&packs, &tmp("gone-project")).unwrap();
        assert!(layer.notices[0].contains("isn't installed any more"));
    }

    #[test]
    fn apply_fills_only_unset_settings() {
        let packs = installed_packs("apply");
        use_pack(&packs, "business-manager", GLOBAL.into(), &mut |_| true).unwrap();
        let mut layer = resolve_layer(&packs, &tmp("apply-project")).unwrap();
        layer.roster = Some(PathBuf::from("/pack/team.toml"));

        let mut settings = Settings::default();
        settings.team.roster_path = Some("/mine.toml".into());
        apply_to_settings(&mut layer, &mut settings);
        assert_eq!(settings.team.roster_path.as_deref(), Some("/mine.toml"), "the user's roster wins");
        assert!(settings.skills.project_dir.as_deref().unwrap().ends_with("coder/skills"));
        assert_eq!(settings.mcp.servers.len(), 1);
        assert!(layer.notices.iter().any(|n| n.contains("roster isn't used")));

        let mut full = Settings::default();
        full.skills.project_dir = Some("/a".into());
        full.skills.user_dir = Some("/b".into());
        full.mcp.servers.push(McpServerConfig { name: "files".into(), command: "mine".into(), ..Default::default() });
        let mut layer = resolve_layer(&packs, &tmp("apply-project2")).unwrap();
        apply_to_settings(&mut layer, &mut full);
        assert_eq!(full.skills.project_dir.as_deref(), Some("/a"));
        assert_eq!(full.mcp.servers.len(), 1, "the user's server of that name stays");
        assert!(layer.notices.iter().any(|n| n.contains("skills aren't loaded")));
        assert!(layer.notices.iter().any(|n| n.contains("already have one with that name")));
    }

    #[test]
    fn trusted_publishers_must_be_real_keys() {
        let mut settings = Settings::default();
        settings.pack.trusted_publishers = vec!["not-a-key".into()];
        assert!(trusted_publishers(&settings).unwrap_err().contains("isn't a base64"));
        let (_bundle, key) = signed(&good_pack("keys"));
        settings.pack.trusted_publishers = vec![key];
        assert_eq!(trusted_publishers(&settings).unwrap().len(), 1);
    }

    #[test]
    fn inspect_describes_and_checks() {
        let payload = aivyx_pack::build_payload(&good_pack("inspect")).unwrap();
        let text = render_inspect(&payload).unwrap();
        assert!(text.contains("aivyx-coder part"), "{text}");
        assert!(text.contains("MCP files: mcp-files --root ."), "{text}");
        assert!(text.contains("checks: OK"), "{text}");
    }
}
