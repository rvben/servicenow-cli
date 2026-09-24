# Changelog

All notable changes are documented here. Versions follow Semantic Versioning.

## [0.5.2](https://github.com/rvben/servicenow-cli/compare/v0.5.1...v0.5.2) - 2026-09-24

### Added

- **tui**: make network requests cancellable with Esc or Ctrl-C ([5e2eb12](https://github.com/rvben/servicenow-cli/commit/5e2eb12150d3a126b1cdfa9eadc09e4deae8dd2c))

### Fixed

- **tui**: show the loading panel while a new table, query, or page loads ([2406681](https://github.com/rvben/servicenow-cli/commit/2406681f33a61ca67f94b9cf6e47b31fe93439d3))
- **attachments**: distinguish omitted metadata fields from empty ones ([f581b40](https://github.com/rvben/servicenow-cli/commit/f581b400eba78f73fd01b6cfd6b2b6122bbdc163))
- **tui**: clear stale selection and keep the overview error visible ([c40e835](https://github.com/rvben/servicenow-cli/commit/c40e835b5bc2665ee8eae34f75ac7b81654c4070))
- **cli**: prompt interactively for tables delete like other delete commands ([e63d167](https://github.com/rvben/servicenow-cli/commit/e63d167f6ca2ec2b76ed3aae3078a4b7e2923265))
- **output**: print nothing for an empty CSV result set ([fc41749](https://github.com/rvben/servicenow-cli/commit/fc417494bd3150ccecfeec6917ed6cc64076d779))
- **cli**: route argument parsing errors through the JSON error envelope ([ac4df03](https://github.com/rvben/servicenow-cli/commit/ac4df033e9574a4b27a2c3ac4a5687a050628419))
- **cli**: end the process quietly on a closed stdout pipe ([bd08d9c](https://github.com/rvben/servicenow-cli/commit/bd08d9c0ec0408e4ae800323a3e281f2b5164918))
- **auth**: describe Firefox sign-in as a profile, not a private browser ([1d330ad](https://github.com/rvben/servicenow-cli/commit/1d330adf0564c24290b70084235394615b816573))
- **auth**: harden throwaway browser profiles and the CDP sign-in channel ([1e6775b](https://github.com/rvben/servicenow-cli/commit/1e6775b5146f3eadf04758c0c5557325018bdc8d))

## [0.5.1](https://github.com/rvben/servicenow-cli/compare/v0.5.0...v0.5.1) - 2026-09-24

### Added

- **auth**: support Firefox and default-browser selection for browser sign-in ([99bc722](https://github.com/rvben/servicenow-cli/commit/99bc72218cbc4c7017def0303dea8140ed1e0d8b))
- **tui**: add guarded incident actions ([d1d3680](https://github.com/rvben/servicenow-cli/commit/d1d36807adedf87d97234fa6e990c722b2918899))

### Fixed

- **deps**: update rustls to 0.23.45 for RUSTSEC-2026-0285 ([2ddd82f](https://github.com/rvben/servicenow-cli/commit/2ddd82feae83d622322f03a780d5a946dd4452ef))

## [0.5.0](https://github.com/rvben/servicenow-cli/compare/v0.4.5...v0.5.0) - 2026-09-03

### Added

- **auth**: improve profile selection and offline checks ([1f05b18](https://github.com/rvben/servicenow-cli/commit/1f05b181840988eb5f64a831e2ca0effd495e41a))
- **incidents**: add guarded resolve workflow ([1ae636d](https://github.com/rvben/servicenow-cli/commit/1ae636d240b1b92df069a4edc6857a84d79b7589))

## [0.4.5](https://github.com/rvben/servicenow-cli/compare/v0.4.4...v0.4.5) - 2026-08-31

### Added

- **tui**: add local page search ([41b3ca4](https://github.com/rvben/servicenow-cli/commit/41b3ca42ea7f8bc6651b45863941913b2626288b))

## [0.4.4](https://github.com/rvben/servicenow-cli/compare/v0.4.3...v0.4.4) - 2026-08-27

### Fixed

- **auth**: persist rotated browser session cookies ([84d7b82](https://github.com/rvben/servicenow-cli/commit/84d7b8201eb5a13a101a738ce24c436b34f869db))

## [0.4.3](https://github.com/rvben/servicenow-cli/compare/v0.4.2...v0.4.3) - 2026-08-27

### Added

- **tui**: focus incidents on relevant assignments ([cdf3cfb](https://github.com/rvben/servicenow-cli/commit/cdf3cfb45f38d7d7e399a96f1579089ee3634fb0))

## [0.4.2](https://github.com/rvben/servicenow-cli/compare/v0.4.1...v0.4.2) - 2026-08-26

### Added

- **tui**: make authentication recoverable in place ([9f69501](https://github.com/rvben/servicenow-cli/commit/9f695017c2fcb85540f61468581ed43994c8c48a))

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
