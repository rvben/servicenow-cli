# Authentication and profiles

The recommended setup is an OS-keychain-backed named profile:

```sh
servicenow setup work --instance company
servicenow auth status
servicenow profile list
```

When `--method` is omitted, setup requests an authenticated UI route without
credentials. It recognizes Microsoft Entra and other external SSO redirects and
chooses browser-based OAuth. A classic `/login.do` form is not treated as proof
that your account has a usable ServiceNow password: that side door can exist on
fully federated instances. In that ambiguous case, interactive setup asks you to
choose browser sign-in, a local/service-account password, or an access token.
The probe never sends credentials and never follows a redirect to the external
identity provider. Interactive terminals show progress during the probe and a
compact connection summary when setup succeeds.

The credential is stored in Keychain Access on macOS, Credential Manager on
Windows, or the platform credential service on Linux. The profile's instance,
username, authentication type, and safety mode are stored in the configuration
file. Pass `--method basic`, `oauth`, or `bearer` to make scripts deterministic.

## WSL2 and headless Linux

WSL2 and minimal Linux installations often have no Secret Service provider. If
the OS credential store is unavailable, interactive setup explains the problem
and offers to store the credential in the CLI config file instead. The file is
created with mode `0600` on Unix, but the credential is plaintext, so setup asks
before writing it.

For non-interactive setup, opt in explicitly:

```sh
servicenow setup work --insecure-storage \
  --instance company --method basic --username api-user \
  --secret-stdin
```

To avoid persistent credential storage entirely, set `SERVICENOW_PASSWORD` for
Basic authentication or `SERVICENOW_TOKEN` for bearer/OAuth authentication.
Environment variables take precedence over credentials stored by a profile.

## OAuth

Under **System OAuth → Application Registry**, a ServiceNow administrator must
create an **OAuth API endpoint for external clients** and register the loopback
redirect URI. Then run:

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

For an instance whose web login redirects to `login.microsoftonline.com`, this
is still normally a **ServiceNow OAuth application**, not a new app registration
made directly against Microsoft Entra. ServiceNow starts the browser flow,
redirects the user through the configured enterprise identity provider, and
then issues the API tokens. If setup detects SSO but no client ID was provided,
it prints a copy-ready request containing the redirect URI for your ServiceNow
administrator.

If the client ID is not ready yet, choose not to continue. Setup saves only the
instance and OAuth settings—no credential or secret—and prints the resume
command. Once the administrator responds, continue without repeating discovery:

```sh
servicenow auth login work --client-id YOUR_CLIENT_ID
```

The client secret prompt is explicitly marked optional; press Enter when the
ServiceNow OAuth application is configured as a public client.

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
