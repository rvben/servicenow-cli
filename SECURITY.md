# Security policy

## Supported versions

Security fixes are released for the latest published minor version. Users
should upgrade to the newest patch before reporting a problem.

## Reporting a vulnerability

Please use the repository's private security-advisory form. Do not disclose a
suspected vulnerability in a public issue. Include the affected version,
platform, authentication method, reproduction steps, and potential impact, but
never include live ServiceNow credentials, session cookies, or access tokens.

You should receive an acknowledgement within five business days. Confirmed
issues will be coordinated privately until a fix and advisory are ready.

## Credential model

New profiles store secrets in the operating-system credential store. The TOML
configuration contains only non-secret settings. Environment-variable secrets
remain available for ephemeral CI use and take precedence over stored values.

The project does not collect telemetry. Diagnostic output must never include
unmasked secrets.
