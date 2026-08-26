# Changelog

All notable changes are documented here. Versions follow Semantic Versioning.

## [0.4.1](https://github.com/rvben/servicenow-cli/compare/v0.4.0...v0.4.1) - 2026-08-26

### Added

- **packaging**: add package-named launcher ([91ac1c4](https://github.com/rvben/servicenow-cli/commit/91ac1c4457dcb488f1a3503f78e6983ff302bd1f))

## [0.4.0](https://github.com/rvben/servicenow-cli/compare/v0.3.14...v0.4.0) - 2026-08-26

### Added

- **tui**: add a read-only Ratatui browser for incidents and arbitrary ServiceNow
  tables, with keyboard navigation, encoded queries, pagination, responsive
  layouts, and complete record inspection ([ebcf1e3](https://github.com/rvben/servicenow-cli/commit/ebcf1e3e019c970fcbca72bac9cfa80c494518c1))
- **tui**: add on-demand Overview, Activity, Attachments, and SLA views for
  incidents, including independent ACL failure reporting and truncation notices
- **cli**: standardize onboarding command ([1d39cf5](https://github.com/rvben/servicenow-cli/commit/1d39cf54362acd4aea72773cbd839a993320e0d5))

### Fixed

- **onboarding**: remove stale setup references ([6c8591c](https://github.com/rvben/servicenow-cli/commit/6c8591cb213b7e8a93adbbe60d348ad515a663fa))
- **package**: include README in PyPI metadata ([aa49e1b](https://github.com/rvben/servicenow-cli/commit/aa49e1bf1cd19d027dfd154c38cdc38b45bdb3a9))
- **ci**: install pinned Rust components ([ec4d639](https://github.com/rvben/servicenow-cli/commit/ec4d639549401e5921b078584d9008b921237ca7))

### Safety

- The TUI requires an interactive terminal, performs no ServiceNow writes,
  redacts secret-bearing field names, sanitizes server-provided control
  characters, and preserves structural focus cues when color is disabled.

## 0.3.14 — 2026-08-24

### Changed

- WSL browser sign-in now prefers an installed Linux Chrome, Edge, or Chromium
  browser and uses the Windows PowerShell bridge only as a fallback.
- `SERVICENOW_BROWSER` accepts friendly `chrome`, `edge`, `chromium`, and
  `auto` names. `windows-edge`, `windows-chrome`, and `windows-chromium`
  explicitly select the WSL Windows bridge.

### Fixed

- The Windows browser bridge checks the managed `RemoteDebuggingAllowed`
  policy before launching Edge, Chrome, or Chromium. When an administrator has
  disabled remote debugging, sign-in now fails immediately with a specific
  remediation instead of waiting for the browser-channel timeout.

## 0.3.13 — 2026-08-24

### Fixed

- WSL browser sign-in now streams its PowerShell bridge over standard input
  instead of embedding the entire script in a process argument. This avoids
  Windows interop's command-line size boundary, which could make PowerShell
  exit immediately with `Invalid argument` before opening the browser.

## 0.3.12 — 2026-08-24

### Fixed

- WSL browser sign-in now reserves an explicit Windows loopback port and gives
  that port directly to Edge or Chrome instead of waiting for Chromium's
  intermittently missing `DevToolsActivePort` file. The browser-channel startup
  has its own 20-second deadline and reports an actionable enterprise-policy
  diagnostic, while users still receive the full five minutes to complete SSO.
- The isolated browser is asked to close through its private DevTools session
  when sign-in finishes, preventing successful handoffs from leaving the
  temporary window open.

## 0.3.11 — 2026-08-24

### Added

- `--verbose` and `SERVICENOW_VERBOSE=true` now stream timestamped browser
  sign-in milestones while the CLI waits for an SSO handoff. The WSL
  PowerShell bridge emits progress as it happens instead of buffering until the
  browser closes or times out.

### Safety

- Verbose browser diagnostics accept only allowlisted stage identifiers and
  HTTP status codes. URLs, cookies, tokens, usernames, executable paths, raw
  PowerShell output, and page content are never logged.

## 0.3.10 — 2026-08-24

### Fixed

- WSL browser sign-in no longer reports Windows PowerShell's harmless
  first-run progress records as raw CLIXML. The bridge suppresses progress
  output, ignores progress-only diagnostics, and returns real failures through
  a dedicated UTF-8 protocol so errors stay concise and actionable.

## 0.3.9 — 2026-08-24

### Fixed

- Browser-session sign-in now discovers ServiceNow's user token in both the
  top-level page and same-origin UI frames, using either `g_ck` or the classic
  `sysparm_ck` field. This completes the WSL handoff for framed ServiceNow UI
  layouts that previously remained open and waited until timeout.
- The terminal now shows that the secure browser handoff is still in progress,
  and timeout errors identify whether the CLI was waiting for the authenticated
  page, session cookies, user token, or REST validation.

## 0.3.8 — 2026-08-24

### Fixed

- Browser-session sign-in now supplies ServiceNow's anti-CSRF user token while
  validating the captured session. Instances that require `X-UserToken` for
  session-bound REST requests now complete the CLI handoff after SSO instead of
  waiting until timeout.

## 0.3.7 — 2026-08-24

### Fixed

- Browser-session authentication now opens a visibly InPrivate or Incognito
  window in addition to using its disposable profile. Setup displays the
  resolved ServiceNow identity and asks for confirmation before storing the
  session, protecting against unintended Entra device-SSO account selection.

## 0.3.6 — 2026-08-24

### Fixed

- WSL2 browser sign-in now passes the complete bridge to Windows PowerShell as
  one encoded command and accepts both UTF-8 and UTF-16 output. This prevents
  PowerShell from exiting successfully without launching the browser or
  returning a session.

## 0.3.5 — 2026-08-24

### Added

- SSO discovery now selects zero-admin browser-session authentication instead
  of requiring a ServiceNow OAuth Application Registry entry. Chrome, Edge, or
  Chromium opens with an isolated temporary profile and the session is
  validated before anything is stored.
- WSL2 can use Windows Edge or Chrome through a loopback-only PowerShell bridge,
  without depending on WSL mirrored networking or exposing browser debugging to
  the LAN.
- `--method browser` and `SERVICENOW_COOKIE` provide explicit browser-session
  selection and ephemeral automation support.

### Safety

- Browser sign-in retains only cookies scoped to the requested ServiceNow host
  and API root; identity-provider cookies are discarded. It also supplies the
  session's anti-CSRF user token for writes and never follows an API redirect to
  an identity provider. The isolated browser profile is removed after login,
  session values remain masked, and expired sessions require an explicit
  re-login instead of automatically retrying a possibly mutating command.

## 0.3.4 — 2026-08-24

### Fixed

- SAML/Entra-federated instances are detected through an authenticated UI route
  instead of incorrectly treating the always-available `/login.do` form as
  evidence that the current user can authenticate with a ServiceNow password.
- Ambiguous discovery now asks interactive users to choose a login method, and
  a Basic 401 on a detected SSO instance points to OAuth and its ServiceNow
  Application Registry prerequisite instead of suggesting another Basic login.

## 0.3.3 — 2026-08-24

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
