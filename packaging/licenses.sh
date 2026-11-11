#!/bin/sh
# Rewrite the license texts of the Rust crates each shipped binary is built
# from, for the targets it is released for. The first run builds the pinned
# cargo-about into target/tools.
# Usage: packaging/licenses.sh
set -eu
cd "$(dirname "$0")/.."
version=0.9.3
about=target/tools/bin/cargo-about
if [ "$("$about" --version 2>/dev/null)" != "cargo-about $version" ]; then
  cargo install cargo-about --version "$version" --locked --features cli --root target/tools
fi
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

list() {
  out=licenses/$1.txt
  shift
  "$about" generate --fail --locked "$@" -o "$tmp" licenses/template.hbs
  # Some crates ship their license with CRLF line endings; git stores LF.
  tr -d '\r' < "$tmp" > "$out"
}

list desktop -m crates/desktop/Cargo.toml --target x86_64-unknown-linux-gnu \
  --target aarch64-unknown-linux-gnu --target x86_64-pc-windows-msvc
list android -m crates/phone/Cargo.toml --target aarch64-linux-android \
  --target x86_64-linux-android
