# Changelog

All notable changes are documented here. Versions follow Semantic Versioning.

## Unreleased

### Added

- `servicenow setup` and `servicenow auth login` now inspect the public instance
  login route and automatically select local Basic authentication or
  browser-based OAuth for Microsoft Entra and other external SSO providers.
- Inconclusive interactive discovery presents an authentication chooser, while
  non-interactive setup asks for an explicit `--method` instead of guessing.
- SSO onboarding explains the required ServiceNow OAuth registration and prints
  a copy-ready administrator request containing the loopback redirect URI.
- Interactive discovery has a terminal progress indicator, optional OAuth
  secrets are labeled explicitly, and successful setup ends with a compact
  connection summary and next steps.
- When an OAuth client ID is not ready, setup can save a credential-free draft;
  `servicenow auth login PROFILE` resumes the saved instance and OAuth settings.

### Safety

- Login discovery sends no credentials and follows redirects only while they
  remain on the ServiceNow instance; external identity-provider URLs are
  classified without being requested.

## 0.3.2 — 2026-08-24

### Fixed

- `servicenow setup` no longer exposes a raw D-Bus/zbus failure when WSL2 or a
  minimal Linux environment has no Secret Service provider.

### Added

- Interactive setup now explains unavailable credential storage and offers a
  permission-locked config-file fallback before reading or writing a secret.
- Non-interactive setup can select the fallback explicitly with
  `--insecure-storage`.
- `servicenow doctor` reports whether credentials came from the OS keychain,
  protected config file, environment, or a legacy profile field.

### Safety

- The OS keychain remains the default. Plaintext fallback storage requires
  confirmation, uses an atomic mode-`0600` config write on Unix, remains
  overridable by environment variables, and is cleared by logout or profile
  removal.

## 0.3.1 — 2026-08-22

### Added

- A guided `servicenow setup` entry point with secure prompts and clear next
  steps.
- Compact, typed command discovery through `servicenow schema --command`,
  including defaults, enums, side effects, confirmation, dry-run, output, and
  exit-code metadata for agents.
- Actionable empty states and configuration error remediation.

### Changed

- Incident lists now request readable display values for text output while
  preserving raw values and the existing envelope for machine output.
- Default human incident tables use curated columns, readable headers, terminal
  width bounds, subtle semantic color, and no routine `sys_id` column.
- Profile discovery now gives first-time users a direct setup command.

### Compatibility

- Piped and explicitly machine-readable incident output remains raw by default
  and retains the stable `{count, result}` contract.
- `--display-value false|true|all` continues to override the adaptive default.

## 0.3.0 — 2026-08-22

### Added

- Attachment `list`, `upload`, `download`, and `delete` workflows for every
  ServiceNow table.
- Record resolution by number, `sys_id`, or a form URL from the active instance.
- Streamed attachment transfers, inferred or explicit MIME types, and binary
  download-to-stdout support.
- Attachment upload and deletion dry runs plus a live PDI attachment lifecycle.
- Post-publication Cargo and PyPI installation smoke tests for tagged releases.

### Safety

- Downloads sanitize server-provided names, write through a temporary file, and
  refuse to replace an existing path unless `--force` is supplied.
- Attachment mutations honor read-only profiles, and permanent deletion requires
  an interactive confirmation or `--yes`.
- Record and attachment URLs from a different ServiceNow instance are rejected.

### Changed

- GitHub Actions now use Node.js 24-compatible artifact actions.
- RustSec auditing runs the pinned official `cargo-audit` tool directly.
- Python wheels use the latest pinned maturin action and maturin release.

## 0.2.1 — 2026-08-22

### Fixed

- Corrected the CycloneDX output name so the release workflow publishes the
  SBOM, checksums, attestations, GitHub assets, and registry packages.

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
