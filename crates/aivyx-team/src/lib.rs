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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamConfigError {
    #[error("team's `lead` ({0:?}) does not match any member's name")]
    UnknownLead(String),
    #[error("duplicate member name: {0:?}")]
    DuplicateMember(String),
    #[error("member {member:?}'s tool_allowlist names an unavailable tool: {tool:?}")]
    UnknownTool { member: String, tool: String },
}

impl TeamConfig {
    /// Validates this config against the caller-supplied list of tool
    /// names actually available to the lead. Does not check
    /// `deny_paths`/`extra_deny_paths` shape (any string is accepted as
    /// a path pattern here) -- only structural validity: the lead
    /// exists, member names are unique, and every tool_allowlist entry
    /// is one the lead itself could grant.
    pub fn validate(&self, available_tools: &[&str]) -> Result<(), TeamConfigError> {
        if !self.members.iter().any(|m| m.name == self.lead) {
            return Err(TeamConfigError::UnknownLead(self.lead.clone()));
        }

        let mut seen = std::collections::HashSet::new();
        for member in &self.members {
            if !seen.insert(member.name.clone()) {
                return Err(TeamConfigError::DuplicateMember(member.name.clone()));
            }
        }

        for member in &self.members {
            for tool in &member.tool_allowlist {
                if !available_tools.contains(&tool.as_str()) {
                    return Err(TeamConfigError::UnknownTool {
                        member: member.name.clone(),
                        tool: tool.clone(),
                    });
                }
            }
        }

        Ok(())
    }
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

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn valid_config() -> TeamConfig {
        TeamConfig {
            lead: "coordinator".to_string(),
            members: vec![
                TeamMember {
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec!["set_tasks".to_string()],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You write code.".to_string(),
                    tool_allowlist: vec!["read_file".to_string(), "write_file".to_string()],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    #[test]
    fn valid_config_passes() {
        let config = valid_config();
        let available = ["set_tasks", "read_file", "write_file", "grep"];
        assert!(config.validate(&available).is_ok());
    }

    #[test]
    fn rejects_lead_not_present_among_members() {
        let mut config = valid_config();
        config.lead = "nobody".to_string();
        let available = ["set_tasks", "read_file", "write_file"];
        let err = config.validate(&available).unwrap_err();
        assert_eq!(err, TeamConfigError::UnknownLead("nobody".to_string()));
    }

    #[test]
    fn rejects_duplicate_member_names() {
        let mut config = valid_config();
        config.members.push(config.members[0].clone());
        let available = ["set_tasks", "read_file", "write_file"];
        let err = config.validate(&available).unwrap_err();
        assert_eq!(
            err,
            TeamConfigError::DuplicateMember("coordinator".to_string())
        );
    }

    #[test]
    fn rejects_tool_allowlist_entry_not_in_available_tools() {
        let mut config = valid_config();
        config.members[1]
            .tool_allowlist
            .push("run_command".to_string());
        let available = ["set_tasks", "read_file", "write_file"]; // no run_command
        let err = config.validate(&available).unwrap_err();
        assert_eq!(
            err,
            TeamConfigError::UnknownTool {
                member: "implementer".to_string(),
                tool: "run_command".to_string(),
            }
        );
    }
}
