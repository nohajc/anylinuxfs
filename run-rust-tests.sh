#!/bin/bash

set -e

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
cd "$SCRIPT_DIR"

# Common configuration from build-app.sh
FEATURES="freebsd"
FEATURE_ARG=""
if [ -n "$FEATURES" ]; then
    FEATURE_ARG="-F $FEATURES"
fi

# Rust Unit Tests
# Unit tests are generally run on the host architecture even for cross-compiled projects
# to verify logic and algorithms, unless they contain platform-specific assembly or syscalls.

echo "=== Running tests for common-utils ==="
(cd common-utils && cargo test $FEATURE_ARG)

echo ""
echo "=== Running tests for anylinuxfs ==="
# anylinuxfs builds for the host (macOS)
(cd anylinuxfs && cargo test $FEATURE_ARG)

echo ""
echo "=== Running tests for vmproxy (Linux logic) ==="
# We run tests on host to verify shared logic.
# If there are Linux-specific tests that require a Linux environment, they might fail here,
# but usually unit tests are written to be platform-independent or mocked.
# We explicitly avoid the cross-compilation target for unit tests to run them on host.
# We use the host target from rustc to override any default target in .cargo/config.toml
HOST_TARGET=$(rustc -vV | grep host: | awk '{print $2}')
(cd vmproxy && cargo test $FEATURE_ARG --target $HOST_TARGET)

echo ""
echo "=== All Rust unit tests completed ==="
