# Authentication and profiles

The recommended setup is an OS-keychain-backed named profile:

```sh
servicenow auth login work --instance company --method basic --username api-user
servicenow auth status
servicenow profile list
```

The password prompt is hidden and the secret is stored in Keychain Access on
macOS, Credential Manager on Windows, or the platform credential service on
Linux. The profile's instance, username, authentication type, and safety mode
are stored in the configuration file.

## OAuth

Register an OAuth application in ServiceNow with a loopback redirect URI, then
run:

```sh
servicenow auth login work \
  --instance company \
  --method oauth \
  --client-id YOUR_CLIENT_ID \
  --redirect-uri http://127.0.0.1:8484/callback
```

The CLI uses Authorization Code with PKCE, validates the callback state, and
refreshes expiring access tokens when the instance returns a refresh token.
Only loopback HTTP redirect URIs are accepted.

## Bearer tokens and CI

For a manually issued token, use `--method bearer`. For ephemeral automation,
prefer environment variables supplied by the CI secret store:

```sh
export SERVICENOW_INSTANCE=company
export SERVICENOW_TOKEN=...
export SERVICENOW_AUTH_TYPE=bearer
servicenow incidents mine --output json
```

Do not put secrets in command arguments, checked-in files, shell history, logs,
or issue reports.

## Production safety

Create production profiles as read-only until writes are deliberately needed:

```sh
servicenow auth login production --instance company --read-only
servicenow profile use production
```

Read-only mode blocks writes locally. Incident dry runs remain available so a
change can be inspected without sending it.

## Migrating from 0.1

Version 0.2 can read the legacy `password` and `token` TOML fields, but it never
writes them. Run `servicenow auth login PROFILE` to move the credential to the
OS keychain, verify with `servicenow auth status`, and then remove the plaintext
secret from any backups or copied configuration.
