#!/usr/bin/env bash
set -euo pipefail

target="${1:-localhost}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/debug/quicsync"

skip() {
  echo "SKIP: $*" >&2
  exit 77
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || skip "$1 is not installed"
}

require_cmd cargo
require_cmd rsync
require_cmd ssh

ssh -o BatchMode=yes -o ConnectTimeout=5 "$target" true >/dev/null 2>&1 \
  || skip "ssh $target is not available without interaction"

ssh "$target" 'PATH=$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH command -v quicsync >/dev/null' \
  || skip "remote target must have quicsync in PATH"

cargo build

workdir="$(mktemp -d "${TMPDIR:-/tmp}/quicsync-e2e.XXXXXX")"
remote_dir="$(ssh "$target" 'mktemp -d "${TMPDIR:-/tmp}/quicsync-e2e-remote.XXXXXX"')"
cleanup() {
  rm -rf "$workdir"
  ssh "$target" "rm -rf '$remote_dir'" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$workdir/src/nested" "$workdir/pull"
printf 'hello quicsync\n' >"$workdir/src/file.txt"
printf 'path with spaces\n' >"$workdir/src/nested/file with spaces.txt"

"$bin" doctor "$target"
"$bin" --fallback=rsync -a "$workdir/src/" "$target:$remote_dir/pushed/"
"$bin" --fallback=rsync -a "$target:$remote_dir/pushed/" "$workdir/pull/"

diff -ru "$workdir/src/" "$workdir/pull/"
echo "E2E smoke passed for $target"
