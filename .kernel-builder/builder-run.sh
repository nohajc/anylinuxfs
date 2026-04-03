#!/bin/sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

DATA_PARAM=${1:-../../}
DATA_DIR=$(realpath "$DATA_PARAM")

$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/debian-vm-config.lua \
    --set "data_paths[0].tag=shared" \
    --set "data_paths[0].path=$DATA_DIR"
