# Releases

Vership is the release control plane for `servicenow-cli`. GitHub Actions is the
execution plane for cross-platform builds, publication, provenance, and
downstream packaging.

## One-time repository setup

1. Create protected GitHub environments named `crates-io` and `pypi`.
2. On crates.io, configure a trusted GitHub publisher for
   `.github/workflows/release.yml`, environment `crates-io`.
3. On PyPI, configure a trusted GitHub publisher for the same workflow,
   environment `pypi`.
4. Enable GitHub artifact attestations and private vulnerability reporting.

No long-lived registry publishing token is required by the workflow.

## Create a release

Start from a clean, up-to-date `main` branch and run:

```sh
vership preflight
vership bump patch # or minor/major
tarry cmd --timeout 20m -- vership verify
```

Vership runs `make check`, updates the package version and changelog, creates
the Conventional Commit release commit and tag, and pushes both. The tag starts
the release workflow. Use `vership release` only when the on-disk version was
intentionally set in advance.

Cargo and Python package versions are kept synchronized as one release unit.

Before bumping, it can help to run checks vership's own gate does not cover:

```sh
make test-e2e
cargo package --locked --allow-dirty
```

`make test-e2e` runs the ignored lifecycle suite against a Personal Developer
Instance; see the project AGENTS.md for setup.

## What the release workflow does

Once the tag is pushed, the workflow verifies the tag matches the Cargo package
version, re-runs all CI gates, then builds native archives and Python wheels,
creates source distributions, generates SHA-256 checksums and a CycloneDX SBOM,
and attests the artifacts before creating the GitHub release. Only then does it
publish to crates.io and PyPI through short-lived OIDC credentials. A final job
waits for both indexes, installs each public package in a clean environment,
and executes `servicenow --version` before the workflow is considered
successful.

## Failure policy

If a release fails before any release, artifact, package, checksum, or
attestation becomes public, delete and recreate the brief tag and retry the
same version. Once anything was published, preserve the tag and issue a patch
release for release-content corrections. Registry and downstream-packaging
jobs are separate recovery domains; retry only the failed job when possible.

## Dry runs

The Release workflow can be dispatched from `main` with `dry_run` enabled. It
uses the package version unless an explicit `version` is supplied, builds the
real artifacts, and exercises registry validation without publishing packages,
creating a GitHub release, generating attestations, or updating downstream
packaging.
