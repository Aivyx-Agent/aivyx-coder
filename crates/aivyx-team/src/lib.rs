//! `aivyx-team`: the Nonagon-style team schema and attenuation
//! invariant for `aivyx-coder`. Deliberately dependency-light --
//! see this crate's own `Cargo.toml` and the parent plan's Global
//! Constraints -- no dependency on `aivyx-tools`/`aivyx-core`/
//! `aivyx-sandbox`. Validation and attenuation take caller-supplied
//! data (available tool names, the lead's deny_paths) as plain
//! parameters; wiring this into a real tool registry and a real
//! delegation path is later, separate scope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub lead: String,
    pub members: Vec<TeamMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub name: String,
    pub role: String,
    pub persona: String,
    pub tool_allowlist: Vec<String>,
    #[serde(default)]
    pub extra_deny_paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_config_round_trips_through_toml() {
        let toml_str = r#"
            lead = "coordinator"

            [[members]]
            name = "coordinator"
            role = "Lead"
            persona = "You delegate, verify, and synthesize; you never execute tasks directly."
            tool_allowlist = ["set_tasks"]
            extra_deny_paths = []

            [[members]]
            name = "implementer"
            role = "Implementer"
            persona = "You write and edit code."
            tool_allowlist = ["read_file", "write_file", "edit_file", "grep", "glob", "run_command"]
            extra_deny_paths = ["secrets/"]
        "#;
        let config: TeamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.lead, "coordinator");
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.members[1].name, "implementer");
        assert_eq!(
            config.members[1].extra_deny_paths,
            vec!["secrets/".to_string()]
        );
    }

    #[test]
    fn team_member_defaults_extra_deny_paths_to_empty_when_omitted() {
        let toml_str = r#"
            lead = "solo"

            [[members]]
            name = "solo"
            role = "Lead"
            persona = "You work alone."
            tool_allowlist = []
        "#;
        let config: TeamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.members[0].extra_deny_paths, Vec::<String>::new());
    }
}
