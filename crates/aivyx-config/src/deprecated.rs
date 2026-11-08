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
}

impl fmt::Display for Deprecation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
        out.push(Deprecation { old, new: "broker.base_url", overridden_by });
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
        });
    }
    out
}

/// Tells the user, once per key, the same way as an unknown key: a log
/// line, and stderr.
pub(crate) fn report(found: &[Deprecation]) {
    for d in found {
        tracing::warn!(key = d.old, "{d}");
        eprintln!("warning: {d}");
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
