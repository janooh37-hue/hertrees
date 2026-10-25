#!/bin/sh
# Herdr build step: compile bin/hertrees from source.
set -eu
cd "$(dirname "$0")/.."

if command -v cargo >/dev/null 2>&1; then
  cargo_cmd="cargo"
elif command -v mise >/dev/null 2>&1 && mise which cargo >/dev/null 2>&1; then
  cargo_cmd="mise exec -- cargo"
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
  cargo_cmd="$HOME/.cargo/bin/cargo"
else
  echo "hertrees: cargo (Rust 1.88+) is needed to build the plugin: https://rustup.rs" >&2
  exit 1
fi

$cargo_cmd build --release --locked
mkdir -p bin
cp target/release/hertrees bin/hertrees.tmp
mv bin/hertrees.tmp bin/hertrees
echo "hertrees: built bin/hertrees"
