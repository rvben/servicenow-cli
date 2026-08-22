# Releasing

Releases are tag-driven and must come from a clean, fully tested `main` branch.
The tag must exactly match the Cargo package version.

## One-time repository setup

1. Create protected GitHub environments named `crates-io` and `pypi`.
2. On crates.io, configure a trusted GitHub publisher for
   `.github/workflows/release.yml`, environment `crates-io`.
3. On PyPI, configure a trusted GitHub publisher for the same workflow,
   environment `pypi`.
4. Enable GitHub artifact attestations and private vulnerability reporting.

No long-lived registry publishing token is required by the workflow.

## Release checklist

```sh
make check
make test-e2e
cargo package --locked --allow-dirty
```

Update `Cargo.toml` and `CHANGELOG.md` in one Conventional Commit. Push that
commit, then create and push the matching signed tag. The workflow verifies the
version again before building anything.

The workflow builds native archives and Python wheels, verifies all CI gates,
creates source distributions, generates SHA-256 checksums and a CycloneDX SBOM,
attests the artifacts, creates the GitHub release, and only then publishes to
crates.io and PyPI through short-lived OIDC credentials.

## Failed releases

If a failure occurs after only a brief public tag was created and nothing else
was published, delete and recreate that tag and retry the same version. Once a
release, artifact, package, checksum, SBOM, or attestation is public, preserve
the tag and issue a patch version instead.
