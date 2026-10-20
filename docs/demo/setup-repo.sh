#!/usr/bin/env bash
# Hidden by VHS. Prepares a temp git repo and a `codex` alias for the demo tapes.
set -euo pipefail
export LANG="${LANG:-en_US.UTF-8}"
export LC_ALL="${LC_ALL:-en_US.UTF-8}"
export PS1='$ '
shopt -s expand_aliases
root=${UNWRECK_DEMO_ROOT:?}
alias codex="$root/docs/demo/fake-codex.sh"
# shellcheck disable=SC2164
cd "$(mktemp -d -t uwdemo)"
export UNWRECK_DIR
UNWRECK_DIR=$(mktemp -d -t uwstore)
git init -q
git config user.email demo@example.com
git config user.name demo
mkdir -p src/gen
echo 'fn main() { println!(hi); }' > src/main.rs
echo 'pub fn a() {}' > src/gen/a.rs
echo SECRET=1 > .env
git add -A
git -c commit.gpgsign=false commit -qm init
unwreck hooks --write --agent codex --scope project >/dev/null
# Do not leak `set -e` into the VHS shell (`ls` of missing files must not abort).
set +euo pipefail 2>/dev/null || set +e
shopt -s expand_aliases
