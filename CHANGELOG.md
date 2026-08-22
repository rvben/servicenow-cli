# Changelog

All notable changes are documented here. Versions follow Semantic Versioning.

## 0.2.0 — 2026-08-22

### Added

- Interactive Basic, bearer-token, and OAuth Authorization Code + PKCE login.
- OS-keychain credential storage and active named-profile management.
- Incident `edit`, `note`, `assign`, `open`, and `watch` workflows.
- User/group resolution by human identifier, including `@me`.
- Cached instance dictionary metadata plus `schema` and `choices` discovery.
- Responsive Unicode tables and JSON, JSONL, YAML, and CSV output.
- Read-only-compatible dry runs for focused incident mutations.
- Cross-platform CI and tag-driven releases with checksums, SBOMs, and
  GitHub artifact attestations.

### Changed

- Incident assignee and assignment-group inputs are resolved before writes.
- New profiles no longer store credentials in the configuration file.
- Mutation confirmations and success messages are clearer in interactive use.

### Compatibility

- Existing 0.1 plaintext profile fields remain readable for migration, but are
  never written by 0.2.
- Existing `--json` and `--output text|json` automation remains supported.

## 0.1.0 — 2026-08-22

- Initial incident and generic Table API commands.
- Named configuration profiles, read-only mode, structured errors, shell
  completions, offline command schema, and live PDI lifecycle tests.
