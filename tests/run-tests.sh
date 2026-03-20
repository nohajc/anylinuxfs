#!/usr/bin/env bash
# run-tests.sh — Entry point for the anylinuxfs end-to-end test suite.
#
# Prerequisites:
#   brew install bats-core
#   anylinuxfs built:  ./build-app.sh  (binary at bin/anylinuxfs)
#   Alpine rootfs initialized:  anylinuxfs init
#   macOS with hypervisor entitlement (local dev machine only)
#
# Usage:
#   ./tests/run-tests.sh                      # run all tests
#   ./tests/run-tests.sh --filter ext4        # run tests whose names contain "ext4"
#   ./tests/run-tests.sh --tap                # TAP output (for CI parsers)
#   ANYLINUXFS_BIN=/path/to/anylinuxfs ./tests/run-tests.sh
#
# Environment variables:
#   ANYLINUXFS_BIN   Override path to the anylinuxfs binary (default: bin/anylinuxfs)
#   SKIP_APK_SETUP   Set to 1 to skip the one-time Alpine package installation
#                    (useful when packages are already installed from a previous run)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ANYLINUXFS="${ANYLINUXFS_BIN:-"${REPO_ROOT}/bin/anylinuxfs"}"

if [[ ! -x "$ANYLINUXFS" ]]; then
  echo "ERROR: anylinuxfs binary not found at: $ANYLINUXFS"
  echo "       Build it first with: ./build-app.sh"
  echo "       Or set ANYLINUXFS_BIN to override the path."
  exit 1
fi

if ! command -v bats &>/dev/null; then
  echo "ERROR: bats not found. Install with: brew install bats-core"
  exit 1
fi

# ---------------------------------------------------------------------------
# One-time Alpine package installation
# These packages are needed by the VM shell to format disk images.
# ---------------------------------------------------------------------------
APK_PACKAGES=(
  parted          # partition tables (GPT, MBR)
  e2fsprogs       # mkfs.ext4, e2fsck
  btrfs-progs     # mkfs.btrfs, btrfs
  exfatprogs      # mkfs.exfat
  f2fs-tools      # mkfs.f2fs
  ntfs-3g-progs   # mount.ntfs-3g
  ntfsprogs       # mkfs.ntfs, ntfsfix
  cryptsetup      # LUKS encryption
  lvm2            # pvcreate, vgcreate, lvcreate
)

if [[ "${SKIP_APK_SETUP:-0}" != "1" ]]; then
  echo "==> Installing Alpine packages into anylinuxfs rootfs..."
  echo "    (Set SKIP_APK_SETUP=1 to skip if already installed)"
  "$ANYLINUXFS" apk add "${APK_PACKAGES[@]}"
  echo "==> Alpine packages installed."
else
  echo "==> Skipping Alpine package installation (SKIP_APK_SETUP=1)"
fi

# ---------------------------------------------------------------------------
# Run bats, forwarding any extra arguments (--filter, --tap, etc.)
# Files are sorted alphabetically, enforcing sequential execution order.
# anylinuxfs itself serialises concurrent mounts via /tmp/anylinuxfs.lock,
# and bats default mode is sequential, so there is no parallelism risk.
# ---------------------------------------------------------------------------
BATS_FILES=("${SCRIPT_DIR}"/*.bats)

if [[ ${#BATS_FILES[@]} -eq 0 || ! -f "${BATS_FILES[0]}" ]]; then
  echo "ERROR: No .bats files found in ${SCRIPT_DIR}"
  exit 1
fi

echo "==> Running ${#BATS_FILES[@]} test file(s) with bats..."
exec bats "$@" "${BATS_FILES[@]}"
