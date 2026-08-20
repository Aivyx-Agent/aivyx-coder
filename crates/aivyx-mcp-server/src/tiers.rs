//! `AccessLevel` and the three tier→tool-name mappings. Each tier's set is
//! additive over the previous (`plan` ⊂ `edit` ⊂ `execute`), verified
//! against real `Tool::mutates_outside_session()` classifications and
//! `delegate_task`'s own exclusion precedent — see the plan's Global
//! Constraints for the full accounting of why each tool landed where it
//! did.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessLevel {
    Plan,
    Edit,
    Execute,
}

impl AccessLevel {
    /// Parses the MCP tool parameter / config value. Case-sensitive,
    /// exactly "plan" | "edit" | "execute" — no aliases, so a typo fails
    /// loudly rather than silently mapping to something unintended.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "plan" => Ok(Self::Plan),
            "edit" => Ok(Self::Edit),
            "execute" => Ok(Self::Execute),
            other => Err(format!(
                "invalid access_level {other:?} -- must be \"plan\", \"edit\", or \"execute\""
            )),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Plan => 0,
            Self::Edit => 1,
            Self::Execute => 2,
        }
    }

    /// `true` if `self` does not exceed `ceiling` -- used to reject a
    /// `code` call's requested level against the operator-configured max.
    pub fn at_most(&self, ceiling: &AccessLevel) -> bool {
        self.rank() <= ceiling.rank()
    }

    /// Names of every tool excluded from `mcp_registry` (Task 2) to reach
    /// this tier -- i.e. everything ranked strictly above it, plus (at
    /// every tier) the always-excluded set from the plan's Global
    /// Constraints.
    pub fn excluded_tool_names(&self) -> Vec<&'static str> {
        const EDIT_ONLY: &[&str] = &["write_file", "edit_file", "delete_file", "move_file", "patch_file"];
        const EXECUTE_ONLY: &[&str] = &[
            "run_command", "run_shell", "git_commit", "git_branch", "git_push", "git_pr",
            "memory_write", "memory_forget", "remember_preference",
        ];
        match self {
            Self::Plan => [EDIT_ONLY, EXECUTE_ONLY].concat(),
            Self::Edit => EXECUTE_ONLY.to_vec(),
            Self::Execute => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_exactly_the_three_valid_strings() {
        assert_eq!(AccessLevel::parse("plan"), Ok(AccessLevel::Plan));
        assert_eq!(AccessLevel::parse("edit"), Ok(AccessLevel::Edit));
        assert_eq!(AccessLevel::parse("execute"), Ok(AccessLevel::Execute));
    }

    #[test]
    fn parse_rejects_anything_else() {
        assert!(AccessLevel::parse("Plan").is_err(), "case-sensitive");
        assert!(AccessLevel::parse("danger-full-access").is_err());
        assert!(AccessLevel::parse("").is_err());
    }

    #[test]
    fn at_most_orders_plan_below_edit_below_execute() {
        assert!(AccessLevel::Plan.at_most(&AccessLevel::Plan));
        assert!(AccessLevel::Plan.at_most(&AccessLevel::Execute));
        assert!(!AccessLevel::Execute.at_most(&AccessLevel::Plan));
        assert!(!AccessLevel::Edit.at_most(&AccessLevel::Plan));
        assert!(AccessLevel::Execute.at_most(&AccessLevel::Execute));
    }

    #[test]
    fn plan_excludes_both_edit_only_and_execute_only_tools() {
        let excluded = AccessLevel::Plan.excluded_tool_names();
        assert!(excluded.contains(&"write_file"));
        assert!(excluded.contains(&"run_command"));
        assert_eq!(excluded.len(), 5 + 9);
    }

    #[test]
    fn edit_excludes_only_execute_only_tools() {
        let excluded = AccessLevel::Edit.excluded_tool_names();
        assert!(!excluded.contains(&"write_file"), "edit tier must include write_file");
        assert!(excluded.contains(&"run_command"));
        assert_eq!(excluded.len(), 9);
    }

    #[test]
    fn execute_excludes_nothing_tier_specific() {
        assert!(AccessLevel::Execute.excluded_tool_names().is_empty());
    }
}
