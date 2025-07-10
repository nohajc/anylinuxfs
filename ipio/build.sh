#!/bin/bash
set -e

export SWIFT_BRIDGE_OUT_DIR="$(pwd)/../generated"

cargo build
cargo build --release
