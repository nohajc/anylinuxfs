#!/bin/sh

set -e

# Check if brew is installed
if ! command -v brew >/dev/null 2>&1; then
    echo "Error: Homebrew is not installed."
    echo "Please install Homebrew first by running:"
    echo '/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'
    exit 1
fi

# Check if go is installed
if ! command -v go >/dev/null 2>&1; then
    echo "Error: Go is not installed."
    echo "Please install Go first:"
    echo "  - On macOS: brew install go"
    echo "  - Or download from: https://golang.org/dl/"
    exit 1
fi

brew install e2fsprogs luajit skopeo
go install github.com/opencontainers/umoci/cmd/umoci@latest

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Sign luajit with the Hypervisor entitlement
LUAJIT_REAL_PATH=$(readlink -f /opt/homebrew/opt/luajit/bin/luajit)
codesign --entitlements "$SCRIPT_DIR/../anylinuxfs.entitlements" --force -s - "$LUAJIT_REAL_PATH"
