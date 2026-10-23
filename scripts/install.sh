#!/usr/bin/env bash
# Builds larvae in release mode and installs it to ~/.larvae/bin with `larvae self install`.
#
#   scripts/install.sh           installs the build with the version in Cargo.toml
#   scripts/install.sh 0.11.0    stamps 0.11.0 on this build only
#
# A stamped version goes into Cargo.toml for the build. The script then puts
# Cargo.toml and Cargo.lock back as they were, also when the build fails.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $# -gt 0 ]]; then
  backup=$(mktemp -d)
  cp Cargo.toml Cargo.lock "$backup/"
  trap 'cp "$backup/Cargo.toml" "$backup/Cargo.lock" . && rm -rf "$backup"' EXIT
  # The first `version = "..."` line is the version of the build.
  awk -v v="$1" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
    "$backup/Cargo.toml" > Cargo.toml
fi

cargo build --release -p larvae
target/release/larvae self install
target/release/larvae --version
