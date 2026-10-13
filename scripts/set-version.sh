#!/usr/bin/env bash
# Sets the [package] version in Cargo.toml; scripts/build.sh then refreshes Cargo.lock and pkg/.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

VERSION="${1:?usage: set-version.sh <version>}"
sed -i.bak -E "1,/^version = /s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
rm Cargo.toml.bak
grep -q "^version = \"$VERSION\"$" Cargo.toml
