---
name: quicsync-release
description: Project-local release workflow for quicsync. Use when merging current quicsync work to main, preparing a release commit, bumping Cargo.toml, packaging release assets, creating tags, pushing main/tags, or validating quicsync release readiness.
---

# quicsync Release

Use this project-local workflow for `quicsync` releases.

## Safety

- Preserve unrelated user changes. Do not use `git add -A` or `git add .`.
- Release from `main` only after syncing `origin/main`.
- Do not delete untracked `.xm/` or `proptest-regressions/` files unless explicitly asked.
- Ask before pushing `main` or tags if the user has not already requested release/push.
- Treat release asset publication as GitHub Releases-compatible: `quicsync_<os>_<arch>.tar.gz` plus `checksums.txt`.

## Workflow

1. Inspect state:
   - `git status --short --branch`
   - `git log --oneline --decorate -5`
   - `git remote get-url origin`
2. Sync base:
   - `git fetch origin`
   - if on `main`, merge `origin/main` before final verification.
   - if not on `main`, merge into `main` only after tests pass and conflicts are resolved.
3. Verify:
   - `cargo test`
   - `cargo build --release`
   - `rustfmt --check --edition 2024 src/cli.rs src/types.rs src/main.rs src/server.rs src/error.rs src/rsync.rs src/stats.rs src/doctor.rs src/lib.rs src/session.rs src/remote_install.rs src/update.rs`
   - `git diff --check`
   - `bash -n scripts/e2e-local.sh` if the script exists.
4. Version:
   - New commands or user-visible capabilities are MINOR unless the user requests PATCH.
   - Update `Cargo.toml` package version.
   - Run `cargo check` or `cargo test` afterward so `Cargo.lock` updates if needed.
5. Commit:
   - Stage explicit changed files only.
   - Use `feat(release): ...` or `chore(release): ...`.
   - Include `Co-Authored-By: OpenAI Codex <noreply@openai.com>`.
6. Tag and push:
   - Tag format: `v<version>`.
   - Push `main`, then push the tag.
   - If release assets are needed, run `make dist` only after confirming that its version bump behavior is desired.

## Notes

- `quicsync update` expects GitHub release assets named `quicsync_linux_x86_64.tar.gz`, `quicsync_linux_aarch64.tar.gz`, `quicsync_macos_x86_64.tar.gz`, or `quicsync_macos_aarch64.tar.gz`, plus `checksums.txt`.
- `install-remote` copies the current local binary to the remote host, so it assumes matching OS/architecture.
