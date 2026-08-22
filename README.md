# servicenow-cli

An unofficial, agent-friendly command-line client for ServiceNow. It combines convenient
incident commands with generic access to every table your ServiceNow account is
allowed to use.

- Auto-JSON when stdout is piped
- Stable error kinds and exit codes
- Basic and bearer-token authentication
- Named config profiles and a read-only safety mode
- Generic Table API CRUD for standard and custom tables
- Offline command schema and shell completions

## Build and install

```sh
cargo install --path .

# Or, from this checkout:
make install
```

The installed binary is `servicenow`.

## Configuration

Run `servicenow config init` to print the resolved config path and an example.
On macOS and Linux the default is `~/.config/servicenow/config.toml`, honoring
`XDG_CONFIG_HOME` when set.

```toml
[default]
instance = "dev12345"
username = "api-user"
password = "your-password"
auth_type = "basic"
read_only = false

[profiles.production]
instance = "company"
token = "oauth-access-token"
auth_type = "bearer"
read_only = true
```

Keep the file private:

```sh
chmod 600 ~/.config/servicenow/config.toml
```

Credentials are intentionally not accepted as command-line flags because
process arguments can be visible to other users. Environment variables take
precedence over the active config profile:

| Variable | Purpose |
|---|---|
| `SERVICENOW_INSTANCE` | Short instance name, host, or full base URL |
| `SERVICENOW_USERNAME` | Basic-auth username |
| `SERVICENOW_PASSWORD` | Basic-auth password |
| `SERVICENOW_TOKEN` | OAuth bearer access token |
| `SERVICENOW_AUTH_TYPE` | `basic` (default) or `bearer` |
| `SERVICENOW_PROFILE` | Named profile |
| `SERVICENOW_READ_ONLY` | Block all mutations when true |

The CLI does not perform an OAuth login flow yet. In bearer mode, provide an
access token issued by your ServiceNow OAuth setup.

## Incidents

```sh
# List and filter with an encoded ServiceNow query
servicenow incidents list --active
servicenow incidents list --query 'priority=1^ORDERBYDESCsys_updated_on'
servicenow incidents mine

# Show by incident number or sys_id
servicenow incidents show INC0010001
servicenow incidents show 0123456789abcdef0123456789abcdef

# Create and update
servicenow incidents create \
  --short-description "VPN unavailable" \
  --description "Unable to connect since 08:30" \
  --impact 2 --urgency 2

servicenow incidents update INC0010001 \
  --state 2 --work-notes "Investigating the gateway"
```

Use repeated `--field name=value` arguments for instance-specific fields. Values
that are valid JSON become their corresponding JSON type; other values remain
strings.

```sh
servicenow incidents create --short-description "Laptop setup" \
  --field u_office=Amsterdam --field notify=true
```

## Generic Table API

```sh
# Read records from any allowed table
servicenow tables list cmdb_ci \
  --query 'operational_status=1' \
  --fields sys_id,name,sys_class_name --limit 100

servicenow tables get cmdb_ci 0123456789abcdef0123456789abcdef

# Create from inline JSON or stdin
servicenow tables create u_example --data '{"name":"Demo","active":true}'
printf '%s' '{"name":"Demo"}' | servicenow tables create u_example --data -

# Update and delete
servicenow tables update u_example 0123456789abcdef0123456789abcdef \
  --field active=false
servicenow tables delete u_example 0123456789abcdef0123456789abcdef --yes
```

ServiceNow reference fields can be returned as raw values, display values, or
both:

```sh
servicenow incidents show INC0010001 --display-value all
servicenow tables list sys_user --display-value true
```

## Automation contract

When stdout is not a terminal, commands produce JSON automatically. Override
that with `--output text` or `--output json`. Data goes to stdout; informational
messages and errors go to stderr.

```sh
servicenow incidents list | jq '.result[].number'
servicenow schema | jq '.commands[].name'
servicenow completions zsh > _servicenow
```

Errors use this envelope in machine-readable mode:

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

## Development

```sh
make check
```

The test suite uses a mock HTTP server and never requires a live ServiceNow
instance.
