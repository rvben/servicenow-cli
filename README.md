# servicenow-cli

[![CI](https://github.com/rvben/servicenow-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/servicenow-cli/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/servicenow-cli.svg)](https://crates.io/crates/servicenow-cli)
[![PyPI](https://img.shields.io/pypi/v/servicenow-cli.svg)](https://pypi.org/project/servicenow-cli/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fast, safe, human-friendly ServiceNow operations from the terminal.

`servicenow` is an unofficial open-source CLI for people, scripts, and agents.
It pairs focused daily workflows with generic Table API access, works through
supported instance APIs, and requires no instance-side CLI plugin.

```text
$ servicenow incidents mine
┌────────────┬──────────────┬─────────────────┬─────────────┬──────────┬─────────────────────┐
│ NUMBER     │ PRIORITY     │ DESCRIPTION     │ STATE       │ ASSIGNEE │ UPDATED             │
╞════════════╪══════════════╪═════════════════╪═════════════╪══════════╪═════════════════════╡
│ INC0010042 │ 2 - High     │ VPN unavailable │ In Progress │ Ada      │ 2026-08-22 10:30:00 │
└────────────┴──────────────┴─────────────────┴─────────────┴──────────┴─────────────────────┘
```

## Why it feels different

- Workflow-first commands for incidents and attachments, with every table still available.
- Human inputs such as incident numbers, user names, emails, group names, and
  `@me`; no routine `sys_id` hunting.
- Zero-admin browser sign-in for SSO instances, with managed OAuth available when administrators provide it.
- Credentials in the operating-system keychain, with a permission-locked file fallback for
  environments such as WSL2 that do not provide a credential service.
- Beautiful responsive tables for humans; deterministic JSON, JSONL, YAML, and
  CSV for automation.
- Read-only profiles, semantic editor patches, dry runs, and explicit dangerous
  operation confirmation.
- Stable error kinds and exit codes for scripts and agents.

## Install

Both packages install the `servicenow` binary:

```sh
cargo install servicenow-cli --locked

# or
pipx install servicenow-cli
```

From a checkout:

```sh
cargo install --path .
```

## Two-minute start

```sh
# Detects SSO and opens a private browser window; no OAuth app is required.
servicenow setup work --instance company

# Managed OAuth remains available when your organization provides a client ID:
servicenow auth login work --instance company --method oauth \
  --client-id YOUR_CLIENT_ID

# On headless Linux/WSL2, choose the permission-locked file fallback directly:
servicenow setup work --instance company --insecure-storage

servicenow doctor
servicenow incidents mine
servicenow incidents show INC0010042
```

For browser SSO, OAuth, bearer-token, CI, migration, and production-profile guidance, see
[Authentication and profiles](docs/authentication.md).

## Incident workflows

```sh
# Find work
servicenow incidents list --active
servicenow incidents list --query 'priority=1^ORDERBYDESCsys_updated_on'
servicenow incidents mine
servicenow incidents show INC0010042

# Create and update
servicenow incidents create \
  --short-description "VPN unavailable" \
  --description "Unable to connect since 08:30" \
  --impact 2 --urgency 2

servicenow incidents update INC0010042 --state 2

# Focused daily actions
servicenow incidents note INC0010042 "Investigating the gateway"
servicenow incidents assign INC0010042 --to ada@example.com --group "Network"
servicenow incidents open INC0010042
servicenow incidents watch INC0010042
```

Edit safely in `$EDITOR`. The document contains only curated editable fields;
the CLI shows a diff, confirms, and PATCHes only values that changed:

```sh
servicenow incidents edit INC0010042

# Review a non-interactive plan without writing, even on a read-only profile.
servicenow incidents edit INC0010042 --file incident.yaml --dry-run
servicenow incidents note INC0010042 --file note.md --dry-run
servicenow incidents assign INC0010042 --to @me --dry-run
```

Use repeated `--field name=value` arguments for instance-specific fields.
Values that parse as JSON retain their JSON type.

## Attachment workflows

Attachment commands work with any table. Records can be identified by number,
`sys_id`, or a form URL copied from the configured ServiceNow instance:

```sh
# Discover files without looking up the incident sys_id
servicenow attachments list incident INC0010042

# Stream the upload, infer text/plain, and keep status output off stdout
servicenow attachments upload incident INC0010042 ./diagnostic.txt

# Preview writes even when the active profile is read-only
servicenow attachments upload incident INC0010042 ./diagnostic.txt --dry-run

# Download atomically; existing files are never replaced accidentally
servicenow attachments download 0123456789abcdef0123456789abcdef ./downloads/
servicenow attachments download 0123456789abcdef0123456789abcdef - > diagnostic.txt

# Inspect the exact deletion before permanently removing the attachment
servicenow attachments delete 0123456789abcdef0123456789abcdef --dry-run
servicenow attachments delete 0123456789abcdef0123456789abcdef --yes
```

Uploads and downloads are streamed rather than loaded entirely into memory.
Server-provided file names are reduced to a safe local basename, downloads use
a temporary file plus atomic persistence, and replacing a local file requires
`--force`. Upload and delete operations honor profile-level read-only mode.

## Discover your instance

ServiceNow tables, choices, and custom fields vary by instance. The CLI can
cache its dictionary locally and resolve human references before writes:

```sh
servicenow schema incident --refresh
servicenow choices incident state
servicenow resolve user ada@example.com
servicenow resolve group "Network"

# Inspect the full offline contract, or one token-efficient command
servicenow schema
servicenow schema --command 'incidents list'
```

Cached metadata contains no credentials or record data.

## Every ServiceNow table

Focused commands never take away generic access:

```sh
servicenow tables list cmdb_ci \
  --query 'operational_status=1' \
  --fields sys_id,name,sys_class_name --limit 100

servicenow tables get cmdb_ci 0123456789abcdef0123456789abcdef
servicenow tables create u_example --data '{"name":"Demo","active":true}'
servicenow tables update u_example 0123456789abcdef0123456789abcdef \
  --field active=false
servicenow tables delete u_example 0123456789abcdef0123456789abcdef --yes
```

Only records and fields allowed by the authenticated user's ServiceNow ACLs are
available.

## Output and automation contract

Interactive stdout uses a table. Piped stdout automatically becomes JSON. An
explicit format always wins:

```sh
servicenow incidents list --output json
servicenow incidents list --output jsonl
servicenow incidents list --output yaml
servicenow incidents list --output csv
servicenow incidents list --output table
```

Incident tables prefer ServiceNow display values and curated columns. Machine
output intentionally keeps raw values by default for stable automation. Use
`--display-value false|true|all` to override either behavior explicitly.

Data goes to stdout; status messages and errors go to stderr. `--quiet`
suppresses status messages. `--no-color` and the `NO_COLOR` environment
variable disable ANSI color.

Machine-readable errors use a stable envelope:

```json
{"error":{"kind":"not_found","message":"not found: ..."}}
```

| Exit | Meaning |
|---:|---|
| 0 | Success |
| 1 | Unexpected or transport error |
| 2 | Invalid input or configuration |
| 3 | Authentication or authorization failure |
| 4 | Record not found |
| 5 | Other ServiceNow API error |
| 6 | Rate limited |
| 7 | Conflict or ambiguous match |

The versioned offline command schema describes argument types, defaults, enums,
side effects, confirmation and dry-run behavior, output envelopes, and exit
codes. Query a single command to keep agent context small:

```sh
servicenow schema | jq '.commands[].name'
servicenow schema --command 'attachments delete'
servicenow completions zsh > _servicenow
```

## Configuration precedence

Command options override environment variables, which override the active
profile. Environment variables are useful for ephemeral automation:

| Variable | Purpose |
|---|---|
| `SERVICENOW_INSTANCE` | Instance name, hostname, or full base URL |
| `SERVICENOW_USERNAME` | Basic-auth username |
| `SERVICENOW_PASSWORD` | Basic-auth password |
| `SERVICENOW_COOKIE` | Ephemeral ServiceNow browser-session cookie |
| `SERVICENOW_USER_TOKEN` | Matching browser anti-CSRF token for write requests |
| `SERVICENOW_TOKEN` | Bearer/OAuth access token |
| `SERVICENOW_AUTH_TYPE` | `browser`, `basic`, `bearer`, or `oauth` |
| `SERVICENOW_PROFILE` | Named profile |
| `SERVICENOW_READ_ONLY` | Block all actual mutations when true |
| `SERVICENOW_CACHE_DIR` | Override the metadata cache root |

```sh
servicenow profile list
servicenow profile use production
servicenow auth status
servicenow auth logout
```

## Development and release trust

```sh
make check
make test-e2e # requires an ignored .env.e2e file and a PDI
```

The default suite uses mock servers. The ignored PDI lifecycle suite creates
isolated records, verifies incident and attachment lifecycles, and cleans up
every record and file it creates.

CI runs formatting, linting, tests on Linux/macOS/Windows, and a RustSec audit.
Tagged releases produce native archives, Cargo/PyPI packages, SHA-256 checksums,
a CycloneDX SBOM, and GitHub artifact attestations, then install and execute both
public packages as a final smoke test. See [SECURITY.md](SECURITY.md),
[SUPPORT.md](SUPPORT.md), and the [release runbook](docs/releasing.md).

## Status

This project is unofficial and is not affiliated with or supported by
ServiceNow. ServiceNow is a trademark of ServiceNow, Inc.

Licensed under the [MIT License](LICENSE).
