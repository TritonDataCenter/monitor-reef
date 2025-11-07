#!/usr/bin/env bash
set -euo pipefail

echo "[rebase] ensure MPL headers present"
bash .git-tools/add_mpl_headers.sh

echo "[rebase] cargo fmt"
cargo fmt --all || true

if ! git diff --quiet || ! git diff --cached --quiet; then
  git add -A
  git commit --amend --no-edit
fi

echo "[rebase] cargo clippy"
cargo clippy --all-targets --all-features -- -D warnings

echo "[rebase] cargo test"
cargo test --workspace

exit 0

