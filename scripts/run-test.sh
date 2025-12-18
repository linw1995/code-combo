#!/usr/bin/env bash
set -euxo pipefail

export RUST_LOG=${RUST_LOG-debug}

cargo build --bin coco
cargo nextest run "$@"
