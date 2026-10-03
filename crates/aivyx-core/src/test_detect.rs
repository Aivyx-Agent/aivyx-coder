//! Finding a project's test command for `/test` and `--auto`: a few
//! marker files directly in the project folder, first match wins. Never
//! runs anything; reads at most a few small files, each capped.

use std::io::Read;
use std::path::Path;

/// Cap on each file detection reads (`package.json`, `pyproject.toml`,
/// `Makefile`).
const MAX_READ_BYTES: u64 = 256 * 1024;

pub const NO_TESTS_FOUND: &str =
    "No test command found — set [verification] command in config.toml";

/// How long `/test` may run, in seconds, before it's killed — the single
/// source of truth for both `agent/test_command.rs`'s own `TEST_TIMEOUT`
/// and the `timeout_secs` on the synthetic `detected-tests`
/// `allowed_commands` entry `--auto` adds when it falls back to a
/// detected command (`agent_builder.rs`'s `auto_verification`).
pub const TEST_TIMEOUT_SECS: u64 = 600;

/// What detection found: a command (direct exec, no shell) and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedTests {
    pub program: String,
    pub args: Vec<String>,
    /// e.g. `detected from Cargo.toml`.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestSource {
    /// `[verification] command` names a real `allowed_commands` entry.
    Config,
    /// Found by [`detect`]; carries its reason.
    Detected(String),
}

/// The test command this session uses for `/test` (and, under `--auto`,
/// for verification when nothing is configured).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveTests {
    pub program: String,
    pub args: Vec<String>,
    pub source: TestSource,
}

impl EffectiveTests {
    /// `configured` is the `(program, args)` of the `allowed_commands`
    /// entry `[verification] command` names, when it names a real one.
    /// It always wins; detection only fills in when it's `None`.
    pub fn resolve(configured: Option<(String, Vec<String>)>, dir: &Path) -> Option<Self> {
        if let Some((program, args)) = configured {
            return Some(Self { program, args, source: TestSource::Config });
        }
        detect(dir).map(|d| Self {
            program: d.program,
            args: d.args,
            source: TestSource::Detected(d.reason),
        })
    }

    /// The command as a person would type it (arguments shell-quoted
    /// where needed).
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `from config.toml`, or the detection reason.
    pub fn origin(&self) -> String {
        match &self.source {
            TestSource::Config => "from config.toml".to_string(),
            TestSource::Detected(reason) => reason.clone(),
        }
    }

    /// The welcome / `/help` line.
    pub fn status_line(&self) -> String {
        format!("Tests: `{}` ({}) — run them with /test", self.display(), self.origin())
    }

    /// Shown when `--auto` starts.
    pub fn auto_line(&self) -> String {
        format!("Verifying with `{}` ({})", self.display(), self.origin())
    }
}

/// The welcome / `/help` line for an optional command.
pub fn status_line(tests: Option<&EffectiveTests>) -> String {
    tests.map_or_else(|| NO_TESTS_FOUND.to_string(), EffectiveTests::status_line)
}

fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn found(program: &str, args: &[&str], reason: &str) -> Option<DetectedTests> {
    Some(DetectedTests {
        program: program.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        reason: reason.to_string(),
    })
}

/// Reads at most [`MAX_READ_BYTES`] of a file; `None` if it can't be read.
/// Refuses anything that isn't a regular file first — `std::fs::metadata`
/// follows symlinks, so a symlink to a regular file is still read, but a
/// symlink (or direct path) to a FIFO, a device like `/dev/tty`, or a
/// directory is not: opening a FIFO with no writer connected blocks the
/// `File::open` call below forever, and a crafted project directory is
/// untrusted input to this scan.
fn read_capped(path: &Path) -> Option<String> {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_READ_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The project's test command, from marker files directly in `dir`. The
/// first rule that matches wins (see the design doc's table).
pub fn detect(dir: &Path) -> Option<DetectedTests> {
    let has = |name: &str| dir.join(name).is_file();

    if has("Cargo.toml") {
        return found("cargo", &["test"], "detected from Cargo.toml");
    }
    if has("go.mod") {
        return found("go", &["test", "./..."], "detected from go.mod");
    }
    if let Some(text) = read_capped(&dir.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(script) = json.pointer("/scripts/test").and_then(|s| s.as_str())
        && !script.contains("no test specified")
    {
        let program = if has("pnpm-lock.yaml") {
            "pnpm"
        } else if has("yarn.lock") {
            "yarn"
        } else {
            "npm"
        };
        return found(program, &["test"], "detected from package.json");
    }
    if has("pytest.ini") {
        return found("python3", &["-m", "pytest"], "detected from pytest.ini");
    }
    if read_capped(&dir.join("pyproject.toml")).is_some_and(|t| t.contains("[tool.pytest")) {
        return found("python3", &["-m", "pytest"], "detected from pyproject.toml");
    }
    if has("conftest.py") {
        return found("python3", &["-m", "pytest"], "detected from conftest.py");
    }
    if has_python_tests(dir) {
        return found("python3", &["-m", "unittest"], "detected from Python test files");
    }
    if read_capped(&dir.join("Makefile")).is_some_and(|t| t.lines().any(|l| l.starts_with("test:"))) {
        return found("make", &["test"], "detected from Makefile");
    }
    None
}

/// `test_*.py` / `*_test.py` directly in `dir`, or a `tests/` folder
/// holding any `.py` file (not recursive).
fn has_python_tests(dir: &Path) -> bool {
    let names = |d: &Path| -> Vec<String> {
        std::fs::read_dir(d)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    names(dir).iter().any(|n| {
        n.ends_with(".py") && (n.starts_with("test_") || n.ends_with("_test.py"))
    }) || names(&dir.join("tests")).iter().any(|n| n.ends_with(".py"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    fn detected_pair(files: &[(&str, &str)]) -> Option<(String, String)> {
        let dir = dir_with(files);
        detect(dir.path()).map(|d| {
            let mut cmd = d.program.clone();
            for a in &d.args {
                cmd.push(' ');
                cmd.push_str(a);
            }
            (cmd, d.reason)
        })
    }

    fn pair(cmd: &str, reason: &str) -> Option<(String, String)> {
        Some((cmd.to_string(), reason.to_string()))
    }

    const NPM_PLACEHOLDER: &str =
        r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#;
    const NPM_REAL: &str = r#"{"scripts":{"test":"vitest run"}}"#;

    #[test]
    fn every_rule_and_its_reason() {
        type TestCase = (Vec<(&'static str, &'static str)>, Option<(String, String)>);
        let cases: Vec<TestCase> = vec![
            (vec![("Cargo.toml", "[package]\n")], pair("cargo test", "detected from Cargo.toml")),
            (vec![("go.mod", "module x\n")], pair("go test ./...", "detected from go.mod")),
            (vec![("package.json", NPM_REAL)], pair("npm test", "detected from package.json")),
            (vec![("package.json", NPM_REAL), ("pnpm-lock.yaml", "")], pair("pnpm test", "detected from package.json")),
            (vec![("package.json", NPM_REAL), ("yarn.lock", "")], pair("yarn test", "detected from package.json")),
            (vec![("package.json", NPM_PLACEHOLDER)], None),
            (vec![("package.json", r#"{"name":"x"}"#)], None),
            (vec![("package.json", "not json")], None),
            (vec![("pytest.ini", "[pytest]\n")], pair("python3 -m pytest", "detected from pytest.ini")),
            (vec![("pyproject.toml", "[tool.pytest.ini_options]\n")], pair("python3 -m pytest", "detected from pyproject.toml")),
            (vec![("pyproject.toml", "[project]\nname = \"x\"\n")], None),
            (vec![("conftest.py", "")], pair("python3 -m pytest", "detected from conftest.py")),
            (vec![("test_calc.py", "")], pair("python3 -m unittest", "detected from Python test files")),
            (vec![("calc_test.py", "")], pair("python3 -m unittest", "detected from Python test files")),
            (vec![("tests/check.py", "")], pair("python3 -m unittest", "detected from Python test files")),
            (vec![("tests/readme.md", "")], None),
            (vec![("Makefile", "build:\n\tcc x.c\ntest: build\n\t./run\n")], pair("make test", "detected from Makefile")),
            (vec![("Makefile", "build:\n\tcc x.c\n")], None),
            (vec![("README.md", "hi")], None),
        ];
        for (files, expected) in cases {
            assert_eq!(detected_pair(&files), expected, "files: {files:?}");
        }
    }

    #[test]
    fn the_first_matching_rule_wins() {
        assert_eq!(
            detected_pair(&[("Makefile", "test:\n"), ("go.mod", ""), ("Cargo.toml", "")]),
            pair("cargo test", "detected from Cargo.toml")
        );
        assert_eq!(
            detected_pair(&[("Makefile", "test:\n"), ("test_x.py", ""), ("pytest.ini", "")]),
            pair("python3 -m pytest", "detected from pytest.ini")
        );
        assert_eq!(
            detected_pair(&[("Makefile", "test:\n"), ("package.json", NPM_PLACEHOLDER)]),
            pair("make test", "detected from Makefile")
        );
    }

    #[test]
    fn only_the_top_level_folder_counts() {
        assert_eq!(detected_pair(&[("sub/Cargo.toml", "")]), None);
    }

    #[test]
    fn config_wins_over_detection() {
        let dir = dir_with(&[("Cargo.toml", "")]);
        let tests = EffectiveTests::resolve(
            Some(("pytest".into(), vec!["-q".into()])),
            dir.path(),
        )
        .unwrap();
        assert_eq!(tests.source, TestSource::Config);
        assert_eq!(tests.display(), "pytest -q");
        assert_eq!(tests.status_line(), "Tests: `pytest -q` (from config.toml) — run them with /test");
    }

    #[test]
    fn detection_fills_in_when_config_is_unset() {
        let dir = dir_with(&[("Cargo.toml", "")]);
        let tests = EffectiveTests::resolve(None, dir.path()).unwrap();
        assert_eq!(tests.source, TestSource::Detected("detected from Cargo.toml".into()));
        assert_eq!(
            tests.status_line(),
            "Tests: `cargo test` (detected from Cargo.toml) — run them with /test"
        );
        assert_eq!(tests.auto_line(), "Verifying with `cargo test` (detected from Cargo.toml)");
    }

    #[test]
    fn nothing_configured_or_found() {
        let dir = dir_with(&[]);
        assert_eq!(EffectiveTests::resolve(None, dir.path()), None);
        assert_eq!(
            status_line(None),
            "No test command found — set [verification] command in config.toml"
        );
    }

    #[test]
    fn display_quotes_arguments_that_need_it() {
        let tests = EffectiveTests {
            program: "sh".into(),
            args: vec!["-c".into(), "pytest -k 'a b'".into(), String::new()],
            source: TestSource::Config,
        };
        assert_eq!(tests.display(), r#"sh -c 'pytest -k '\''a b'\''' ''"#);
    }

    // A symlinked `package.json` pointing at a FIFO must not make
    // detection block forever: `std::fs::File::open` on a FIFO blocks
    // until a writer connects, and nothing here ever writes to it.
    // Run off the test thread with a bounded `recv_timeout` so a
    // regression fails (and reports a clear panic) instead of hanging
    // the whole test binary.
    #[cfg(unix)]
    #[test]
    fn package_json_symlinked_to_a_fifo_does_not_hang_detection() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join(".fifo");
        assert!(
            std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success(),
            "mkfifo failed"
        );
        std::os::unix::fs::symlink(&fifo, dir.path().join("package.json")).unwrap();

        let dir_path = dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(detect(&dir_path));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(result) => assert_eq!(
                result, None,
                "a FIFO-backed package.json must not be treated as a real test marker"
            ),
            Err(_) => panic!("detect() hung reading a FIFO-backed package.json"),
        }
    }
}
