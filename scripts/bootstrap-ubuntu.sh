#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "bootstrap-ubuntu.sh must run on Linux" >&2
  exit 1
fi

if ! command -v apt-get >/dev/null 2>&1; then
  echo "bootstrap-ubuntu.sh currently supports apt-based Ubuntu hosts" >&2
  exit 1
fi

sudo apt-get update
sudo apt-get install --yes \
  build-essential \
  ca-certificates \
  clang \
  cmake \
  criu \
  curl \
  git \
  jq \
  libseccomp-dev \
  libsqlite3-dev \
  linux-tools-common \
  pkg-config \
  sqlite3 \
  strace

if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/epoch-rustup.sh
  sh /tmp/epoch-rustup.sh -y --profile minimal --component clippy,rustfmt
fi

export PATH="${HOME}/.cargo/bin:${PATH}"
rustup default stable

echo
echo "Epoch Linux host bootstrap complete."
echo "Run: cargo run -p epoch-cli -- doctor"

