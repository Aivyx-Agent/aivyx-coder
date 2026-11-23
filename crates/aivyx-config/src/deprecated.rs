//! Deprecated spellings of config keys.
//!
//! An old spelling is still read: its field (named `legacy_*`, never
//! written back) is moved into the key that replaced it after parsing, and
//! the user is told once. When the old and the new key are both set, the
//! new one wins and the warning says so.

use std::fmt;

use crate::Settings;

/// Every deprecated key and the key that replaced it.
pub const DEPRECATED_KEYS: &[(&str, &str)] = &[
    ("backend.broker_base_url", "broker.base_url"),
    ("backend.kvcache_max_bytes", "kvcache.max_bytes"),
    ("backend.kvcache_store_path", "kvcache.store_path"),
    ("vision.broker_url", "broker.base_url"),
];

/// A deprecated key the config file sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deprecation {
    pub old: &'static str,
    pub new: &'static str,
    /// The key whose value was used instead of this one's: `new` itself,
    /// or another deprecated spelling of it read first. `None` when this
    /// one's value was used.
    pub overridden_by: Option<&'static str>,
    /// A fixed text shown instead of the generic "`old` is deprecated"
    /// line, for a deprecated *value* rather than a key.
    pub message: Option<&'static str>,
}

/// The note for the `"cloud"` spelling of `locality`, now `"shared"`.
pub const CLOUD_LOCALITY_NOTE: &str =
    "locality = \"cloud\" is now \"shared\"; the old name works until 3.0";

/// Whether the file spells a `locality` as `"cloud"` at `backend.locality`,
/// `routing.endpoints.*.locality` or `routing.models[].locality`.
fn sets_cloud_locality(table: &toml::Table) -> bool {
    let is_cloud = |t: &toml::Value| {
        t.as_table().and_then(|t| t.get("locality")).and_then(|v| v.as_str()) == Some("cloud")
    };
    if table.get("backend").is_some_and(is_cloud) {
        return true;
    }
    let Some(routing) = table.get("routing").and_then(|r| r.as_table()) else {
        return false;
    };
    routing.get("endpoints").and_then(|e| e.as_table()).is_some_and(|e| e.values().any(is_cloud))
        || routing.get("models").and_then(|m| m.as_array()).is_some_and(|m| m.iter().any(is_cloud))
}

impl fmt::Display for Deprecation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(message) = self.message {
            return f.write_str(message);
        }
        write!(f, "`{}` is deprecated; use `{}`", self.old, self.new)?;
        if let Some(winner) = self.overridden_by {
            write!(f, " (`{winner}` is set too and wins; this value is ignored)")?;
        }
        Ok(())
    }
}

/// Whether `path` (dotted) is set in the parsed file.
fn present(table: &toml::Table, path: &str) -> bool {
    let mut segs = path.split('.');
    let Some(mut cur) = segs.next().and_then(|s| table.get(s)) else {
        return false;
    };
    for s in segs {
        match cur.as_table().and_then(|t| t.get(s)) {
            Some(v) => cur = v,
            None => return false,
        }
    }
    true
}

/// Moves every deprecated value in `settings` into the key that replaced
/// it, unless that key is set too. `table` is the same file, parsed as a
/// table, to tell a set key from a default.
pub(crate) fn resolve(settings: &mut Settings, table: &toml::Table) -> Vec<Deprecation> {
    let mut out = Vec::new();
    // `broker.base_url` has two old spellings; the first one read wins
    // over the second, and the new key over both.
    let mut broker_source: Option<&'static str> =
        present(table, "broker.base_url").then_some("broker.base_url");
    for (old, value) in [
        ("backend.broker_base_url", settings.backend.legacy_broker_base_url.take()),
        ("vision.broker_url", settings.vision.legacy_broker_url.take()),
    ] {
        let Some(value) = value else { continue };
        let overridden_by = broker_source;
        if overridden_by.is_none() {
            settings.broker.base_url = Some(value);
            broker_source = Some(old);
        }
        out.push(Deprecation { old, new: "broker.base_url", overridden_by, message: None });
    }
    if let Some(value) = settings.backend.legacy_kvcache_store_path.take() {
        let overridden_by = present(table, "kvcache.store_path").then_some("kvcache.store_path");
        if overridden_by.is_none() {
            settings.kvcache.store_path = Some(value);
        }
        out.push(Deprecation {
            old: "backend.kvcache_store_path",
            new: "kvcache.store_path",
            overridden_by,
            message: None,
        });
    }
    if let Some(value) = settings.backend.legacy_kvcache_max_bytes.take() {
        let overridden_by = present(table, "kvcache.max_bytes").then_some("kvcache.max_bytes");
        if overridden_by.is_none() {
            settings.kvcache.max_bytes = value;
        }
        out.push(Deprecation {
            old: "backend.kvcache_max_bytes",
            new: "kvcache.max_bytes",
            overridden_by,
            message: None,
        });
    }
    if sets_cloud_locality(table) {
        out.push(Deprecation {
            old: "locality = \"cloud\"",
            new: "locality = \"shared\"",
            overridden_by: None,
            message: Some(CLOUD_LOCALITY_NOTE),
        });
    }
    out
}

/// The `[routing.endpoints.*] kind`s aivyx-coder takes, for messages.
pub const LOCAL_ENDPOINT_KINDS: &str = "ollama, llama_router, openai_compat or lemonade";

/// The hosted-API endpoint kinds aivyx-route 0.2 removed.
const REMOVED_ENDPOINT_KINDS: &[&str] = &["anthropic", "openai"];

/// The error for a `[routing.endpoints.*]` table naming a removed hosted-API
/// kind, checked before deserializing so the user gets the fix instead of
/// serde's "unknown variant". `None` when no endpoint does.
pub(crate) fn removed_endpoint_kind(table: &toml::Table) -> Option<String> {
    let endpoints = table.get("routing")?.as_table()?.get("endpoints")?.as_table()?;
    endpoints.iter().find_map(|(name, endpoint)| {
        let kind = endpoint.as_table()?.get("kind")?.as_str()?;
        REMOVED_ENDPOINT_KINDS.contains(&kind).then(|| {
            format!(
                "[routing.endpoints.{name}]: provider \"{kind}\" was removed in 2.0 — aivyx is \
                 local-only; use {LOCAL_ENDPOINT_KINDS} with a local base_url"
            )
        })
    })
}

/// Logs each, once per key. The text reaches the user through
/// `Settings::load_notes`.
pub(crate) fn report(found: &[Deprecation]) {
    for d in found {
        tracing::warn!(key = d.old, "{d}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> (Settings, Vec<Deprecation>, Vec<crate::UnknownKey>) {
        let (settings, unknown, deprecated) = Settings::parse_with_warnings(raw).unwrap();
        (settings, deprecated, unknown)
    }

    #[test]
    fn locality_cloud_loads_as_shared_and_warns_once() {
        let (s, d, unknown) = parse(
            "[backend]\nlocality = \"cloud\"\n\
             [routing.endpoints.a]\nkind = \"ollama\"\nlocality = \"cloud\"\n\
             [[routing.models]]\nid = \"m\"\nendpoint = \"a\"\nlocality = \"cloud\"\n",
        );
        assert_eq!(s.backend.locality, Some(aivyx_route::Locality::Shared));
        assert!(unknown.is_empty(), "{unknown:?}");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(
            d[0].to_string(),
            "locality = \"cloud\" is now \"shared\"; the old name works until 3.0"
        );
    }

    #[test]
    fn locality_shared_does_not_warn() {
        let (s, d, _) = parse("[backend]\nlocality = \"shared\"\n");
        assert_eq!(s.backend.locality, Some(aivyx_route::Locality::Shared));
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn backend_broker_base_url_loads_into_broker_base_url_and_warns() {
        let (s, d, unknown) = parse("[backend]\nbroker_base_url = \"http://b:1\"\n");
        assert_eq!(s.broker.base_url.as_deref(), Some("http://b:1"));
        assert!(unknown.is_empty(), "an alias is not an unknown key: {unknown:?}");
        assert_eq!(d.len(), 1);
        assert_eq!(
            d[0].to_string(),
            "`backend.broker_base_url` is deprecated; use `broker.base_url`"
        );
    }

    #[test]
    fn vision_broker_url_loads_into_broker_base_url_and_warns() {
        let (s, d, unknown) = parse("[vision]\nenabled = true\nbroker_url = \"http://v:2\"\n");
        assert_eq!(s.broker.base_url.as_deref(), Some("http://v:2"));
        assert_eq!(s.broker.image_tools_url(), "http://v:2");
        assert!(unknown.is_empty(), "{unknown:?}");
        assert_eq!(d[0].to_string(), "`vision.broker_url` is deprecated; use `broker.base_url`");
    }

    #[test]
    fn backend_kvcache_keys_load_into_kvcache_and_warn() {
        let (s, d, unknown) = parse(
            "[backend]\nkvcache_store_path = \"~/shared\"\nkvcache_max_bytes = 5000000000\n",
        );
        assert_eq!(s.kvcache.store_path.as_deref(), Some("~/shared"));
        assert_eq!(s.kvcache.max_bytes, 5_000_000_000);
        assert!(unknown.is_empty(), "{unknown:?}");
        let olds: Vec<&str> = d.iter().map(|d| d.old).collect();
        assert_eq!(olds, ["backend.kvcache_store_path", "backend.kvcache_max_bytes"]);
        assert!(d.iter().all(|d| d.overridden_by.is_none()));
    }

    #[test]
    fn the_new_key_beats_its_alias_and_the_warning_says_so() {
        let (s, d, _) = parse(
            "[broker]\nbase_url = \"http://new:1\"\n\
             [kvcache]\nstore_path = \"/new\"\nmax_bytes = 10737418240\n\
             [backend]\nbroker_base_url = \"http://old:1\"\n\
             kvcache_store_path = \"/old\"\nkvcache_max_bytes = 1\n",
        );
        assert_eq!(s.broker.base_url.as_deref(), Some("http://new:1"));
        assert_eq!(s.kvcache.store_path.as_deref(), Some("/new"));
        // Set to the default explicitly still counts as set.
        assert_eq!(s.kvcache.max_bytes, 10 * 1024 * 1024 * 1024);
        assert_eq!(d.len(), 3);
        assert_eq!(
            d[0].to_string(),
            "`backend.broker_base_url` is deprecated; use `broker.base_url` \
             (`broker.base_url` is set too and wins; this value is ignored)"
        );
        assert!(d.iter().all(|d| d.overridden_by == Some(d.new)), "{d:?}");
    }

    #[test]
    fn of_two_old_broker_spellings_the_backend_one_wins() {
        let (s, d, _) = parse(
            "[backend]\nbroker_base_url = \"http://chat:1\"\n\
             [vision]\nbroker_url = \"http://img:2\"\n",
        );
        assert_eq!(s.broker.base_url.as_deref(), Some("http://chat:1"));
        assert_eq!(d[1].old, "vision.broker_url");
        assert_eq!(d[1].overridden_by, Some("backend.broker_base_url"));
    }

    #[test]
    fn a_removed_endpoint_kind_names_the_local_kinds() {
        for kind in ["anthropic", "openai"] {
            let raw = format!("[routing.endpoints.hosted]\nkind = \"{kind}\"\n");
            let err = Settings::parse_with_warnings(&raw).unwrap_err();
            assert_eq!(
                err.message(),
                format!(
                    "[routing.endpoints.hosted]: provider \"{kind}\" was removed in 2.0 — \
                     aivyx is local-only; use ollama, llama_router, openai_compat or lemonade \
                     with a local base_url"
                )
            );
        }
        // A local kind still loads.
        let (s, _, _) = parse("[routing.endpoints.gpu]\nkind = \"ollama\"\n");
        assert!(s.routing.endpoints.contains_key("gpu"));
    }

    #[test]
    fn a_config_without_old_keys_has_no_deprecations() {
        let (s, d, _) = parse("[broker]\nbase_url = \"http://b:1\"\n");
        assert!(d.is_empty());
        assert_eq!(s.broker.base_url.as_deref(), Some("http://b:1"));
    }

    #[test]
    fn every_listed_alias_is_resolved() {
        for (old, new) in DEPRECATED_KEYS {
            let (section, key) = old.split_once('.').unwrap();
            let value = if key.ends_with("max_bytes") { "7" } else { "\"x\"" };
            let (_, d, unknown) = parse(&format!("[{section}]\n{key} = {value}\n"));
            assert!(unknown.is_empty(), "{old}: {unknown:?}");
            assert_eq!(d.len(), 1, "{old}");
            assert_eq!((d[0].old, d[0].new), (*old, *new));
        }
    }

    #[test]
    fn nothing_deprecated_is_ever_written() {
        let (s, _, _) = parse(
            "[backend]\nbroker_base_url = \"http://b:1\"\nkvcache_store_path = \"/k\"\n\
             kvcache_max_bytes = 3\n[vision]\nbroker_url = \"http://v:2\"\n",
        );
        let text = toml::to_string(&s).unwrap();
        for (old, _) in DEPRECATED_KEYS {
            let key = old.split_once('.').unwrap().1;
            assert!(!text.contains(&format!("{key} =")), "{old} written:\n{text}");
        }
        assert!(text.contains("[broker]") && text.contains("base_url = \"http://b:1\""), "{text}");
        assert!(text.contains("[kvcache]") && text.contains("max_bytes = 3"), "{text}");
    }

    #[test]
    fn setup_writes_no_deprecated_key() {
        // `--setup` writes `minimal_config_text`; the full writer
        // serializes `Settings`. Neither may produce an old spelling.
        let backend = crate::BackendSettings {
            kind: crate::BackendKind::LlamaServerBroker,
            ..Default::default()
        };
        let minimal = crate::minimal_config_text(Some(&backend)).unwrap();
        let full = toml::to_string_pretty(&Settings::default()).unwrap();
        for text in [&minimal, &full] {
            for (old, _) in DEPRECATED_KEYS {
                let key = old.split_once('.').unwrap().1;
                assert!(!text.contains(key), "{old} written:\n{text}");
            }
            let (_, d, unknown) = parse(text);
            assert!(d.is_empty() && unknown.is_empty(), "{d:?} {unknown:?}");
        }
        assert!(full.contains("[broker]") && full.contains("[kvcache]"), "{full}");
    }
}
