# Authentication and profiles

The recommended setup is an OS-keychain-backed named profile:

```sh
servicenow setup work --instance company
servicenow auth status
servicenow profile list
```

When `--method` is omitted, setup requests an authenticated UI route without
credentials. It recognizes Microsoft Entra and other external SSO redirects and
chooses zero-admin browser-session authentication. A classic `/login.do` form
is not treated as proof that your account has a usable ServiceNow password: that
side door can exist on fully federated instances. In an ambiguous case,
interactive setup asks you to choose browser sign-in, managed OAuth, a
local/service-account password, or an access token. The probe never sends
credentials and never follows a redirect to the external identity provider.

The credential is stored in Keychain Access on macOS, Credential Manager on
Windows, or the platform credential service on Linux. The profile's instance,
username, authentication type, and safety mode are stored in the configuration
file. Pass `--method browser`, `basic`, `oauth`, or `bearer` to select a method
explicitly.

## Sign in from the TUI

`servicenow tui` is also a valid starting point before authentication is ready.
When the active profile is new, missing a credential, expired, or rejected by
the instance, the TUI shows a dedicated connection state. Press `enter` or `a`
to start the same guided secure sign-in used by `servicenow auth login`.

Authentication runs in the normal terminal so password and browser prompts are
never drawn into the ledger. After a successful sign-in, the CLI reopens the
TUI automatically and loads the requested table and query. Press `q`, `esc`, or
Ctrl-C from the connection state to leave without changing the profile.

## Browser sign-in for SSO

Browser sign-in is the recommended employee experience for SAML/Entra-federated
instances when no OAuth application is available:

```sh
servicenow setup work --instance company --method browser
```

The CLI launches Edge InPrivate or Chrome/Chromium Incognito with a new
temporary profile and a localhost-only debugging channel. Complete the normal
SSO and MFA flow in that window. A managed Windows device can still authenticate
silently through Entra device SSO, even in a private window, so setup displays
the resolved ServiceNow name and username for confirmation before storing
anything. The CLI retains only cookies valid for the requested ServiceNow
hostname and API path, validates them against the Table API, closes the private
browser, and removes its temporary profile. Identity-provider cookies—including
Microsoft Entra cookies—are never retained by the CLI. The resulting ServiceNow
cookie and anti-CSRF user token are protected like any other credential. This uses
[ServiceNow's documented support for binding REST requests to an existing
session with cookies](https://www.servicenow.com/docs/r/api-reference/rest-api-explorer/c_RESTAPI.html).
The matching `X-UserToken` is included for operations protected by
[ServiceNow's anti-CSRF validation](https://www.servicenow.com/docs/r/platform-security/instance-security-hardening-settings/sc-prevent-users-from-accepting-warning-to-bypass-csrf-validation.html).

No ServiceNow Application Registry entry or administrator action is required.
The user's normal ServiceNow ACLs and REST access policies still apply. Browser
sessions do not have OAuth refresh tokens, so when ServiceNow expires the
session, sign in again without repeating setup:

```sh
servicenow auth login work --method browser
```

The CLI never retries a failed command automatically after session expiry. This
avoids accidentally repeating a write whose outcome is uncertain.

## WSL2 and headless Linux

On WSL2, browser sign-in prefers Chrome, Edge, or Chromium installed inside the
Linux distribution. With WSLg this avoids Windows interop entirely. If no Linux
browser is installed, the CLI falls back to Windows Edge or Chrome through
PowerShell and performs the cookie handoff entirely on the Windows loopback
interface. The fallback works with both NAT and mirrored WSL networking and does
not open a debugging port to the LAN.

`SERVICENOW_BROWSER` accepts an executable path or a friendly name:

```sh
# Prefer the matching Linux browser on WSL, then use its Windows counterpart.
SERVICENOW_BROWSER=chrome servicenow auth login work --method browser
SERVICENOW_BROWSER=edge servicenow auth login work --method browser
SERVICENOW_BROWSER=chromium servicenow auth login work --method browser

# Explicitly use the Windows bridge.
SERVICENOW_BROWSER=windows-edge servicenow auth login work --method browser
SERVICENOW_BROWSER=windows-chrome servicenow auth login work --method browser
```

For the Windows fallback, the CLI checks the managed
[`RemoteDebuggingAllowed` Edge policy](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/remotedebuggingallowed)
or [Chrome policy](https://chromeenterprise.google/policies/remote-debugging-allowed/)
before opening the browser. A disabled policy produces an immediate actionable
error instead of waiting for the browser channel to time out.

To see where an in-progress handoff is waiting, enable safe verbose diagnostics:

```sh
servicenow auth login work --method browser --verbose
```

The timestamped stderr log includes only allowlisted stage transitions and HTTP
status codes. It never includes URLs, cookies, tokens, usernames, executable
paths, PowerShell output, or browser page content.

WSL2 and minimal Linux installations often have no Secret Service provider.
After browser authentication succeeds, setup explains the problem and offers to
store the credential in the CLI config file instead. The file is created with
mode `0600` on Unix, but the credential is plaintext, so setup asks before
writing it. Cancelling or failing browser sign-in writes nothing and does not
show the storage prompt.

For non-interactive setup, opt in explicitly:

```sh
servicenow setup work --insecure-storage \
  --instance company --method basic --username api-user \
  --secret-stdin
```

To avoid persistent credential storage entirely, set `SERVICENOW_PASSWORD` for
Basic authentication, `SERVICENOW_COOKIE` for an existing ServiceNow session,
`SERVICENOW_USER_TOKEN` for its matching anti-CSRF token, or `SERVICENOW_TOKEN`
for bearer/OAuth authentication. Environment variables take precedence over
credentials stored by a profile. Treat session values as passwords and never
put them in command arguments, shell history, or logs.

## Managed OAuth

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
then issues the API tokens. Managed OAuth is durable because refresh tokens can
renew access without capturing a new browser session, but it requires an
instance administrator to register the client and redirect URI.

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
