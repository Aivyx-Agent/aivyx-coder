//! Unknown-key reporting for `config.toml`.
//!
//! The config types stay as they are: the TOML is deserialized through
//! `serde_ignored`, which hands back the path of every key the types did not
//! consume. Those become [`UnknownKey`]s, each with a "did you mean" hint
//! drawn from the stable-surface snapshot (`docs/stable/config.txt`), which
//! is checked against the types by a test and so lists every known key.
//! Free-form maps (`HashMap` fields) consume all their entries and are never
//! reported.

use std::collections::BTreeSet;
use std::fmt;

/// The snapshot of every known key path, one per line: `path = default`,
/// `path[].field: type` or `path.*.field: type` (list and table entries),
/// each maybe followed by ` (...)`.
const KNOWN_SNAPSHOT: &str = include_str!("../../../docs/stable/config.txt");

/// The key path of one snapshot line, entry markers dropped:
/// `permissions.allowed_commands[].name: string` gives
/// `permissions.allowed_commands.name`, as a reported path reads once its
/// array index is dropped.
fn snapshot_path(line: &str) -> &str {
    let end = [" = ", ": ", " ("]
        .iter()
        .filter_map(|sep| line.find(sep))
        .min()
        .unwrap_or(line.len());
    &line[..end]
}

/// A key in the config file that this version does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    /// Full dotted path, e.g. `sandbox.allow_leaving_process_group`.
    pub path: String,
    /// The known key this one probably meant.
    pub suggestion: Option<String>,
}

impl fmt::Display for UnknownKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown config key `{}` (ignored)", self.path)?;
        if let Some(s) = &self.suggestion {
            write!(f, ": did you mean `{s}`?")?;
        }
        Ok(())
    }
}

/// Every known key path plus every section prefix of one.
fn known_paths() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in KNOWN_SNAPSHOT.lines() {
        let path = snapshot_path(line).replace("[]", "");
        let mut prefix = String::new();
        // A table entry's name (`.*`) is the user's own; it is skipped.
        for seg in path.split('.').filter(|s| *s != "*") {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(seg);
            out.insert(prefix.clone());
        }
    }
    out
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// The nearest known key: within edit distance 2, else the same last segment
/// in another section. Array indices are ignored when comparing.
fn suggest(path: &str, known: &BTreeSet<String>, by_last_segment: bool) -> Option<String> {
    let norm: Vec<&str> = path.split('.').filter(|s| s.parse::<usize>().is_err()).collect();
    let norm = norm.join(".");
    let near = known
        .iter()
        .filter(|k| *k != &norm)
        .map(|k| (edit_distance(&norm, k), k))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d);
    if let Some((_, k)) = near {
        return Some(k.clone());
    }
    if by_last_segment {
        let last = last_segment(&norm);
        // Leaf keys only: a section named like the key is not a good hint.
        return known
            .iter()
            .find(|k| *k != &norm && last_segment(k) == last && k.contains('.'))
            .cloned();
    }
    None
}

/// Expands an unknown table into its leaf keys so a misplaced section is
/// reported per key (`sandbox.allow_leaving_process_group`), not as one blob.
fn leaves(prefix: &str, v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::Table(t) if !t.is_empty() => {
            for (k, v) in t {
                leaves(&format!("{prefix}.{k}"), v, out);
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

fn lookup<'a>(root: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut segs = path.split('.');
    let mut cur = root.get(segs.next()?)?;
    for s in segs {
        cur = match cur {
            toml::Value::Table(t) => t.get(s)?,
            toml::Value::Array(a) => a.get(s.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Deserializes `raw` into `T`, returning the unknown keys beside it. The
/// value is exactly what `toml::from_str` returns.
pub fn parse_reporting<T: serde::de::DeserializeOwned>(
    raw: &str,
) -> Result<(T, Vec<UnknownKey>), toml::de::Error> {
    let mut ignored = Vec::new();
    let value: T = serde_ignored::deserialize(toml::Deserializer::parse(raw)?, |p| {
        ignored.push(p.to_string());
    })?;
    let table = raw.parse::<toml::Table>().ok();
    let known = known_paths();
    let mut paths = Vec::new();
    for p in ignored {
        match table.as_ref().and_then(|t| lookup(t, &p)) {
            // A whole unknown section: a near-miss on the section name wins;
            // otherwise report each key in it.
            Some(v @ toml::Value::Table(_)) if suggest(&p, &known, false).is_none() => {
                leaves(&p, v, &mut paths);
            }
            _ => paths.push(p),
        }
    }
    let mut seen = BTreeSet::new();
    let keys = paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .map(|path| {
            let suggestion = suggest(&path, &known, true);
            UnknownKey { path, suggestion }
        })
        .collect();
    Ok((value, keys))
}

/// Tells the user, once per key: a log line, and stderr (the log is a file,
/// and this runs before any full-screen UI starts).
pub(crate) fn report(keys: &[UnknownKey]) {
    for k in keys {
        tracing::warn!(key = %k.path, "{k}");
        eprintln!("warning: {k}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;

    fn unknown(raw: &str) -> Vec<UnknownKey> {
        parse_reporting::<Settings>(raw).unwrap().1
    }

    fn paths(raw: &str) -> Vec<String> {
        unknown(raw).into_iter().map(|k| k.path).collect()
    }

    #[test]
    fn unknown_top_level_key_warns() {
        assert_eq!(paths("bogus_key = 1\n"), vec!["bogus_key"]);
    }

    #[test]
    fn unknown_nested_key_warns_with_its_full_path() {
        assert_eq!(paths("[backend]\nmystery = true\n"), vec!["backend.mystery"]);
    }

    #[test]
    fn near_miss_gets_a_suggestion() {
        let keys = unknown("[backend]\nbase_ur = \"http://x\"\n");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].suggestion.as_deref(), Some("backend.base_url"));
        assert_eq!(
            keys[0].to_string(),
            "unknown config key `backend.base_ur` (ignored): did you mean `backend.base_url`?"
        );
    }

    #[test]
    fn same_last_segment_in_another_section_is_suggested() {
        // The key that pa spells under `[confine]` is a typo-by-product here.
        let keys = unknown("[backend]\nextra_read_paths = []\n");
        assert_eq!(keys[0].suggestion.as_deref(), Some("sandbox.extra_read_paths"));
    }

    #[test]
    fn unknown_section_is_reported_per_key() {
        assert_eq!(paths("[nosuch]\na = 1\nb = 2\n"), vec!["nosuch.a", "nosuch.b"]);
    }

    #[test]
    fn misspelled_section_is_reported_once_with_a_suggestion() {
        let keys = unknown("[sandbxo]\nextra_read_paths = []\n");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].path, "sandbxo");
        assert_eq!(keys[0].suggestion.as_deref(), Some("sandbox"));
    }

    #[test]
    fn valid_config_has_no_warnings() {
        let raw = toml::to_string(&Settings::default()).unwrap();
        assert!(unknown(&raw).is_empty());
        let example = include_str!("../../../docs/manual/reference/config-example.toml");
        assert!(unknown(example).is_empty(), "{:?}", unknown(example));
    }

    #[test]
    fn free_form_map_entries_do_not_warn() {
        let raw = "[[mcp.servers]]\nname = \"x\"\ncommand = \"y\"\n\
                   [mcp.servers.env]\nANY_NAME = \"1\"\nOTHER = \"2\"\n";
        assert!(unknown(raw).is_empty(), "{:?}", unknown(raw));
    }

    #[test]
    fn unknown_key_inside_an_array_of_tables_warns() {
        let raw = "[[mcp.servers]]\nname = \"x\"\ncommand = \"y\"\nbogus = 1\n";
        assert_eq!(paths(raw), vec!["mcp.servers.0.bogus"]);
    }

    #[test]
    fn a_misspelled_entry_field_gets_a_suggestion() {
        let raw = "[[permissions.allowed_commands]]\nname = \"t\"\nprogram = \"p\"\n\
                   timeout_sec = 5\n";
        let keys = unknown(raw);
        assert_eq!(keys.len(), 1, "{keys:?}");
        assert_eq!(keys[0].path, "permissions.allowed_commands.0.timeout_sec");
        assert_eq!(
            keys[0].suggestion.as_deref(),
            Some("permissions.allowed_commands.timeout_secs")
        );
    }

    #[test]
    fn every_snapshot_line_shape_gives_its_path() {
        for (line, path) in [
            ("backend.model = \"qwen3.5:9b\"", "backend.model"),
            ("backend.kind = \"generic\" (one of: generic | llama_server)", "backend.kind"),
            ("permissions.allowed_commands[].name: string", "permissions.allowed_commands[].name"),
            ("routing.endpoints.*.kind: enum (one of: ollama)", "routing.endpoints.*.kind"),
            (
                "backend.broker_base_url (deprecated: use broker.base_url)",
                "backend.broker_base_url",
            ),
        ] {
            assert_eq!(snapshot_path(line), path);
        }
        let known = known_paths();
        assert!(known.contains("permissions.allowed_commands.name"));
        assert!(known.contains("routing.endpoints.kind"));
        assert!(known.contains("backend.locality"), "an unset Option is known too");
        assert!(!known.iter().any(|k| k.contains('*') || k.contains("[]") || k.contains(' ')));
    }

    #[test]
    fn repeated_unknown_key_is_reported_once() {
        let raw = "[backend]\nmystery = 1\n";
        assert_eq!(unknown(raw).len(), 1);
    }

    #[test]
    fn removed_tool_calling_keys_warn_and_still_load() {
        // Both were reserved and never read; 1.0 removed them (R13).
        let raw = "[backend]\nmodel = \"m\"\ntool_calling_mode = \"native\"\n\
                   mistralrs_constrain_tool_calls = true\n";
        let (settings, keys) = parse_reporting::<Settings>(raw).unwrap();
        assert_eq!(settings.backend.model, "m");
        let mut got: Vec<&str> = keys.iter().map(|k| k.path.as_str()).collect();
        got.sort();
        assert_eq!(
            got,
            vec!["backend.mistralrs_constrain_tool_calls", "backend.tool_calling_mode"]
        );
    }

    #[test]
    fn removed_final_review_keys_warn_and_still_load() {
        // The experiment measured no effect; 1.0 removed it (R10).
        let raw = "[autonomous]\nmax_iterations = 7\nfinal_review = true\n\
                   final_review_rounds = 2\nfinal_review_min_budget_secs = 60\n\
                   final_review_fresh_context = true\n";
        let (settings, keys) = parse_reporting::<Settings>(raw).unwrap();
        assert_eq!(settings.autonomous.max_iterations, 7);
        let mut got: Vec<&str> = keys.iter().map(|k| k.path.as_str()).collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "autonomous.final_review",
                "autonomous.final_review_fresh_context",
                "autonomous.final_review_min_budget_secs",
                "autonomous.final_review_rounds",
            ]
        );
    }

    #[test]
    fn build_time_feature_lines_parse_to_their_key() {
        // `backend.kind = "mistral_rs" (build-time feature)` and
        // `backend.mistralrs_model_path = null (build-time feature)` (R9).
        let tagged: Vec<&str> = KNOWN_SNAPSHOT
            .lines()
            .filter(|l| l.ends_with(" (build-time feature)"))
            .collect();
        assert_eq!(tagged.len(), crate::BUILD_TIME_FEATURE_CONFIG.len());
        for line in tagged {
            let path = snapshot_path(line);
            assert!(
                crate::BUILD_TIME_FEATURE_CONFIG.iter().any(|(k, _)| *k == path),
                "{line:?} parsed as {path:?}"
            );
        }
        let known = known_paths();
        assert!(known.contains("backend.mistralrs_model_path"));
        assert!(known.contains("backend.kind"));
        assert!(known.iter().all(|k| !k.contains(' ') && !k.contains('(')));
    }

    #[test]
    fn build_time_feature_keys_are_known_and_suggested() {
        let raw = "[backend]\nkind = \"mistral_rs\"\nmistralrs_model_path = \"/m\"\n\
                   mistralrs_model_fil = \"x.gguf\"\n";
        let keys = unknown(raw);
        assert_eq!(keys.len(), 1, "{keys:?}");
        assert_eq!(keys[0].path, "backend.mistralrs_model_fil");
        assert_eq!(keys[0].suggestion.as_deref(), Some("backend.mistralrs_model_file"));
    }

    #[test]
    fn loaded_values_are_unchanged_by_the_warning_path() {
        let raw = "stray = 1\n[backend]\nmodel = \"m\"\nbase_url = \"http://h/v1\"\nstray2 = 2\n";
        let (with, keys) = parse_reporting::<Settings>(raw).unwrap();
        let plain: Settings = toml::from_str(raw).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(
            toml::to_string(&with).unwrap(),
            toml::to_string(&plain).unwrap()
        );
    }
}
