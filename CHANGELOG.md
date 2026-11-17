# Changelog

## [Unreleased]

### Changed

- `aivyx-coder pack install` now refuses packs that fail the new checks (`AGENTS.md` must name each of the pack's skills). A near-miss tool name only warns at install and the install proceeds; `pack check` still fails on it.

### Added

- `aivyx-coder pack check` (and `pack install`) now flags a backticked name in a pack's skills or AGENTS.md that looks like a misspelt aivyx-coder tool (an error in `pack check`, a warning at install), and checks that AGENTS.md names each of the pack's skills.
