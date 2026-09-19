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

/// A specialist's effective deny-list: the union of the lead's own
/// `deny_paths` and the member's `extra_deny_paths`, de-duplicated. A
/// specialist can only ever be handed *more* restriction than the lead
/// already has -- there is deliberately no way for a member's config to
/// remove one of the lead's own entries.
pub fn effective_deny_paths(lead_deny_paths: &[String], member: &TeamMember) -> Vec<String> {
    let mut effective: Vec<String> = lead_deny_paths.to_vec();
    for path in &member.extra_deny_paths {
        if !effective.contains(path) {
            effective.push(path.clone());
        }
    }
    effective
}

/// True iff every tool in `member`'s `tool_allowlist` is present in
/// `lead_tools` -- the direct analog of NT-02 ("a specialist can never
/// exceed its lead") for tool access, checked against the lead's own
/// actual current tool set (a stronger, more specific check than
/// `TeamConfig::validate`'s crate-wide available-tools check).
pub fn tool_allowlist_is_subset(member: &TeamMember, lead_tools: &[&str]) -> bool {
    member
        .tool_allowlist
        .iter()
        .all(|t| lead_tools.contains(&t.as_str()))
}

/// The default, coding-shaped roster this crate ships: `coordinator`
/// (lead, delegates/verifies/synthesizes -- never executes directly),
/// `implementer`, `reviewer`, `tester`. Ships as ready-to-use,
/// schema-valid config data even though no delegation tooling exists
/// yet to invoke it (Phase 2's job) -- matching aivyx-pa's own
/// Foundation-phase precedent of shipping role definitions ahead of the
/// tooling that uses them.
pub fn default_coding_roster() -> TeamConfig {
    TeamConfig {
        lead: "coordinator".to_string(),
        members: vec![
            TeamMember {
                name: "coordinator".to_string(),
                role: "Lead".to_string(),
                persona: "You coordinate a small coding team. You decompose \
                    the task, delegate pieces to implementer/reviewer/tester, \
                    verify their output, and synthesize the final result. You \
                    never write, edit, or run anything directly."
                    .to_string(),
                tool_allowlist: vec!["set_tasks".to_string()],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "implementer".to_string(),
                role: "Implementer".to_string(),
                persona: "You write and edit code to satisfy the task you were \
                    delegated. Read what you need, make the change, keep it \
                    minimal and focused."
                    .to_string(),
                tool_allowlist: vec![
                    "read_file".to_string(),
                    "write_file".to_string(),
                    "edit_file".to_string(),
                    "grep".to_string(),
                    "glob".to_string(),
                ],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "reviewer".to_string(),
                role: "Reviewer".to_string(),
                persona: "You review code changes for correctness, clarity, and \
                    whether they actually satisfy the delegated task. You never \
                    modify files yourself -- you report findings."
                    .to_string(),
                tool_allowlist: vec![
                    "read_file".to_string(),
                    "grep".to_string(),
                    "glob".to_string(),
                    "git_read".to_string(),
                ],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "tester".to_string(),
                role: "Tester".to_string(),
                persona: "You verify a change actually works -- run the \
                    relevant tests or commands and report the real output, \
                    pass or fail."
                    .to_string(),
                tool_allowlist: vec![
                    "read_file".to_string(),
                    "run_command".to_string(),
                    "run_shell".to_string(),
                    "grep".to_string(),
                ],
                extra_deny_paths: vec![],
            },
        ],
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;

    #[test]
    fn default_coding_roster_is_schema_valid() {
        let roster = default_coding_roster();
        // The full set of tool names this roster's members reference --
        // matches Task 4's Step 1 verification against the real
        // aivyx-tools registry at plan-writing time.
        let available = [
            "set_tasks",
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "glob",
            "run_command",
            "run_shell",
            "git_read",
            "git_commit",
        ];
        assert!(roster.validate(&available).is_ok());
    }

    #[test]
    fn default_coding_roster_has_the_four_expected_members() {
        let roster = default_coding_roster();
        let names: Vec<&str> = roster.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["coordinator", "implementer", "reviewer", "tester"]
        );
        assert_eq!(roster.lead, "coordinator");
    }

    #[test]
    fn default_coding_roster_coordinator_has_no_direct_execution_tools() {
        let roster = default_coding_roster();
        let coordinator = &roster.members[0];
        // Matches aivyx-pa's own Nonagon convention: the coordinator's
        // persona forbids direct execution -- its tool_allowlist should
        // not include any file-mutating or command-running tool.
        for forbidden in ["write_file", "edit_file", "run_command", "run_shell"] {
            assert!(
                !coordinator.tool_allowlist.contains(&forbidden.to_string()),
                "coordinator should not directly hold {forbidden}"
            );
        }
    }
}

#[cfg(test)]
mod attenuation_tests {
    use super::*;

    fn member(tool_allowlist: &[&str], extra_deny_paths: &[&str]) -> TeamMember {
        TeamMember {
            name: "implementer".to_string(),
            role: "Implementer".to_string(),
            persona: "You write code.".to_string(),
            tool_allowlist: tool_allowlist.iter().map(|s| s.to_string()).collect(),
            extra_deny_paths: extra_deny_paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn effective_deny_paths_unions_lead_and_member_paths() {
        let lead_deny_paths = vec![".env".to_string(), "*.pem".to_string()];
        let m = member(&["read_file"], &["secrets/"]);
        let mut effective = effective_deny_paths(&lead_deny_paths, &m);
        effective.sort();
        let mut expected = vec![
            ".env".to_string(),
            "*.pem".to_string(),
            "secrets/".to_string(),
        ];
        expected.sort();
        assert_eq!(effective, expected);
    }

    #[test]
    fn effective_deny_paths_deduplicates_overlapping_entries() {
        let lead_deny_paths = vec![".env".to_string()];
        let m = member(&["read_file"], &[".env"]);
        let effective = effective_deny_paths(&lead_deny_paths, &m);
        assert_eq!(effective, vec![".env".to_string()]);
    }

    #[test]
    fn effective_deny_paths_with_no_extra_paths_equals_lead_paths() {
        let lead_deny_paths = vec![".env".to_string()];
        let m = member(&["read_file"], &[]);
        assert_eq!(effective_deny_paths(&lead_deny_paths, &m), lead_deny_paths);
    }

    #[test]
    fn tool_allowlist_is_subset_true_when_every_tool_is_available() {
        let m = member(&["read_file", "grep"], &[]);
        assert!(tool_allowlist_is_subset(
            &m,
            &["read_file", "grep", "write_file"]
        ));
    }

    #[test]
    fn tool_allowlist_is_subset_false_when_a_tool_is_missing() {
        let m = member(&["read_file", "run_command"], &[]);
        assert!(!tool_allowlist_is_subset(&m, &["read_file", "grep"]));
    }

    #[test]
    fn tool_allowlist_is_subset_true_for_empty_allowlist() {
        let m = member(&[], &[]);
        assert!(tool_allowlist_is_subset(&m, &["read_file"]));
    }
}
