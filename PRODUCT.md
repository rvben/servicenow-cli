# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Users

ServiceNow administrators, developers, and operators working in a terminal who need to inspect instance data quickly without repeatedly composing one-off commands or moving to the browser UI.

## Product Purpose

`servicenow-cli` makes routine ServiceNow work fast, safe, and scriptable. Its interactive TUI complements the command interface with a keyboard-driven way to browse arbitrary tables, begin from a polished incident view, filter records, paginate, and inspect complete record data.

Success means a user can connect through an existing profile, find the relevant record, understand its important fields, and return to their shell without exposing credentials or losing the CLI's predictable behavior.

## Positioning

The product combines generic ServiceNow Table API coverage with focused, human-friendly workflows while preserving terminal-native safety, composability, and machine-readable output.

## Operating Context

Users work across ServiceNow instances and named CLI profiles, often from terminals alongside scripts, source code, and operational tooling. The TUI runs only in an interactive terminal and reuses the same configuration, authentication, and API client as existing commands.

## Capabilities and Constraints

- The package is a Rust CLI named `servicenow-cli`; its binary is `servicenow`.
- Generic Table API operations must remain usable for custom tables.
- The initial TUI is read-only and centers on arbitrary table browsing with an incident-focused starting view.
- The TUI launches through `servicenow tui` and must not interfere with piped, machine-readable command output.
- Secrets are never accepted as command-line flags or rendered by the TUI.
- Existing configuration profiles, credential resolution, read-only policy, and API behavior remain authoritative.
- Informational CLI messages belong on stderr; structured command output remains unchanged.

## Brand Commitments

The product voice is fast, safe, direct, and human-friendly. ServiceNow terminology should remain recognizable without reproducing the visual clutter of the browser product.

## Evidence on Hand

The existing CLI implementation, README, tests, and command behavior are the source of truth. No external testimonials, benchmarks, customer claims, or visual assets are available and none should be fabricated.

## Product Principles

- Preserve terminal speed: every common action should be discoverable and efficient from the keyboard.
- Reveal complexity progressively: emphasize useful record summaries before complete raw field data.
- Keep custom tables first-class: focused resource affordances must build on generic browsing.
- Make state unmistakable: profile, instance, table, query, loading, errors, and navigation position should always be legible.
- Stay safe by default: begin read-only and never expose credentials or secret-bearing configuration.

## Accessibility & Inclusion

The TUI must remain usable without a mouse, avoid relying on color alone, respect `--no-color`, adapt to small terminal sizes, and provide explicit key hints and focus indicators.
