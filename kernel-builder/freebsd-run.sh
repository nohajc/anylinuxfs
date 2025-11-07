#!/bin/sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

NAME=${1:-vm}

VFKIT_SOCK="vfkit-sock"

gvproxy --listen unix://$PWD/network.sock --listen-vfkit unixgram://$PWD/$VFKIT_SOCK --ssh-port -1 2>/dev/null &
GVPROXY_PID=$!
trap "kill $GVPROXY_PID; rm $VFKIT_SOCK-krun.sock" EXIT

$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/freebsd-$NAME-config.lua \
    --set "vm.net.gvproxy_sock=$VFKIT_SOCK"
