#!/usr/bin/env bash
set -euxo pipefail

export COCO_LOG=${COCO_LOG-debug}

cargo build --bin coco
cargo nextest run "$@"
