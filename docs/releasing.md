# Releasing

Releases are cut from `main` by pushing a version tag. Binaries are built and
published by [cargo-dist](https://opensource.axo.dev/cargo-dist/)
(`.github/workflows/release.yml`, generated — edit `dist-workspace.toml` and
run `dist generate` instead of editing it by hand). Crates are published to
crates.io by `.github/workflows/publish-crates.yml` when the GitHub release is
published.

## Steps

1. Update `version` in the root `Cargo.toml` (`[workspace.package]` and the
   `rueca-*` entries in `[workspace.dependencies]`).
2. Move the `Unreleased` entries in `CHANGELOG.md` under the new version and
   update the compare links.
3. Check locally:
   ```sh
   cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
   cargo publish --workspace --dry-run
   dist plan
   ```
4. Commit (`git commit -s -m "chore: release vX.Y.Z"`), merge to `main`, then
   tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
   Pre-release versions (`-alpha.N`, `-rc.N`) are marked as pre-releases on
   GitHub automatically.
5. If extraction, resolution or ranking changed, rerun the benchmark and
   commit the refreshed `docs/bench/` results.

## One-time setup

- **crates.io, first publish**: Trusted Publishing can only be configured for
  crates that already exist, so the first release is published by hand from
  a clean checkout of the tag:
  ```sh
  cargo login            # token with publish-new scope
  cargo publish --workspace
  ```
  Then, for each crate on crates.io → Settings → Trusted Publishing, add
  repository `DevArKaDiA/rueca`, workflow `publish-crates.yml`, environment
  `crates-io`. Revoke the token afterwards.
- **GitHub**: create the `crates-io` environment (optionally with required
  reviewers), enable private vulnerability reporting and Discussions, and
  protect `main` (require CI, DCO and linear history).
