#!/usr/bin/env bash
# Shared helpers for anylinuxfs e2e tests.
# Loaded via `load 'test_helper/common'` at the top of each .bats file.

# ---------------------------------------------------------------------------
# Binary resolution
# ---------------------------------------------------------------------------
# Override ANYLINUXFS_BIN in the environment to point at an alternate binary.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ANYLINUXFS="${ANYLINUXFS_BIN:-"${REPO_ROOT}/bin/anylinuxfs"}"

if [[ ! -x "$ANYLINUXFS" ]]; then
  echo "ERROR: anylinuxfs binary not found at: $ANYLINUXFS" >&2
  echo "       Set ANYLINUXFS_BIN to override." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Temp directory management
# Called from setup_file() / teardown_file() in each .bats file.
# ---------------------------------------------------------------------------
create_test_dir() {
  TEST_DIR="$(mktemp -d /tmp/anylinuxfs-test.XXXXXX)"
  export TEST_DIR
}

remove_test_dir() {
  [[ -n "$TEST_DIR" && -d "$TEST_DIR" ]] && rm -rf "$TEST_DIR"
}

# ---------------------------------------------------------------------------
# Disk image helpers
# ---------------------------------------------------------------------------
# NOTE: the anylinuxfs shell (mkfs) and mount paths present the virtio-blk
# device at slightly different sizes: mount mode sees the device as 64 KiB
# (16 × 4096-byte blocks) smaller than shell mode.  To avoid the ext4 kernel
# check "block count N exceeds size of device (N-16 blocks)", pass the block
# count explicitly to mkfs commands as:
#   $(( $(blockdev --getsz /dev/vda) / 8 - 16 ))
# This applies to all filesystems on a raw whole-disk device.  LVM logical
# volumes and LUKS inners that are explicitly sized are not affected as long
# as they don't extend to the very end of the underlying device.

# create_sparse_image <path> <size>    e.g. create_sparse_image "$TEST_DIR/disk.img" 512M
create_sparse_image() {
  local path="$1" size="$2"
  truncate -s "$size" "$path"
}

# ---------------------------------------------------------------------------
# VM shell execution
# ---------------------------------------------------------------------------
# vm_exec <disk_arg> <shell_command>
#   Runs <shell_command> inside the Alpine Linux microVM with <disk_arg> as the
#   disk identifier (file path or colon-separated multi-disk).
#   Mounts tmpfs at specified directories before executing the command.
vm_exec() {
  local disk_arg="$1"
  local cmd="$2"
  local tmpfs_dirs=("/tmp" "/run" "/etc/lvm/archive" "/etc/lvm/backup")

  # Build mount script from directory list
  local mount_script=""
  for dir in "${tmpfs_dirs[@]}"; do
    mount_script+="mount -t tmpfs tmpfs $dir && "
  done

  echo "Running VM shell command: ${mount_script}$cmd"
  "$ANYLINUXFS" shell -c "${mount_script}$cmd" "$disk_arg"
}

# vm_exec_freebsd <disk_arg> <shell_command>
#   Same as vm_exec but uses the FreeBSD image (for ZFS formatting).
vm_exec_freebsd() {
  local disk_arg="$1"
  local cmd="$2"
  "$ANYLINUXFS" shell -i freebsd -c "$cmd" "$disk_arg"
}

# get_mount_point <label>
#   Returns the expected macOS mount path for a volume with the given label.
get_mount_point() {
  if [[ $(id -u) -eq 0 ]]; then
    echo "/Volumes/${1}"
  else
     echo "${HOME}/Volumes/${1}"
  fi
}

# ---------------------------------------------------------------------------
# File I/O assertion
# ---------------------------------------------------------------------------
# assert_file_roundtrip <mount_point>
#   Creates a unique file, writes content, reads it back, asserts it matches.
assert_file_roundtrip() {
  local mount_point="$1"
  local test_file="${mount_point}/alfs_test_$(date +%s%N).txt"
  local content="anylinuxfs-test-$(uname -n)-$$-$(date +%s)"

  echo "MOUNT_POINT:"
  ls -ld "$mount_point"

  echo "$content" > "$test_file"
  local readback
  readback="$(cat "$test_file")"
  rm -f "$test_file"

  if [[ "$readback" != "$content" ]]; then
    echo "FAIL: file roundtrip mismatch" >&2
    echo "  wrote:  '$content'" >&2
    echo "  read:   '$readback'" >&2
    return 1
  fi
}

# ---------------------------------------------------------------------------
# Unmount
# ---------------------------------------------------------------------------
# do_unmount [disk_arg]
#   Unmounts via anylinuxfs. If disk_arg is omitted, unmounts all.
do_unmount() {
  local disk_arg="${1:-}"
  if [[ -n "$disk_arg" ]]; then
    "$ANYLINUXFS" unmount -w "$disk_arg" || true
  else
    "$ANYLINUXFS" unmount -w
  fi
}

# ---------------------------------------------------------------------------
# hdiutil helpers (for 13-hdiutil-attach.bats)
# ---------------------------------------------------------------------------
# hdiutil_attach <image_path>
#   Attaches a raw disk image as a virtual /dev/disk* device (no auto-mount).
#   Prints the /dev/diskX device node to stdout and sets HDIUTIL_DEV.
hdiutil_attach() {
  local image_path="$1"
  local out
  out="$(hdiutil attach \
    -imagekey diskimage-class=CRawDiskImage \
    -nomount \
    "$image_path" 2>&1)"
  # hdiutil prints one line per partition: "/dev/disk5   (whole disk)"
  # The whole-disk device is the first line.
  local dev
  dev="$(echo "$out" | awk 'NR==1{print $1}')"
  if [[ -z "$dev" || ! -b "$dev" ]]; then
    echo "ERROR: hdiutil_attach failed for $image_path" >&2
    echo "$out" >&2
    return 1
  fi
  HDIUTIL_DEV="$dev"
  export HDIUTIL_DEV
  echo "$dev"
}

# hdiutil_detach <dev_node>
#   Detaches a virtual disk. Note: hdiutil detach does not require sudo when
#   the attach was performed by the same user.
hdiutil_detach() {
  local dev="$1"
  hdiutil detach "$dev" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Generic teardown called from each test's teardown()
# ---------------------------------------------------------------------------
# safe_teardown [disk_arg]
#   Unmounts (best-effort), detaches any hdiutil device, removes TEST_DIR.
safe_teardown() {
  local disk_arg="${1:-}"
  do_unmount
  if [[ -n "${HDIUTIL_DEV:-}" ]]; then
    hdiutil_detach "$HDIUTIL_DEV"
    HDIUTIL_DEV=""
  fi
  # Optionally preserve created test artifacts (images) for manual inspection.
  if [[ "${KEEP_TEST_ARTIFACTS:-}" == "1" ]]; then
    local artifacts_root="${ARTIFACTS_DIR:-"${REPO_ROOT}/tests/artifacts"}"
    mkdir -p "$artifacts_root"

    # Prefer the bats test filename when available, fall back to a timestamp.
    local testname
    if [[ -n "${BATS_TEST_FILENAME:-}" ]]; then
      testname="$(basename "${BATS_TEST_FILENAME%.*}")"
    else
      testname="unnamed-$(date +%s)"
    fi

    local destdir="$artifacts_root/$testname"
    mkdir -p "$destdir"

    # If a specific disk_arg (file) was provided, copy it; otherwise copy
    # common image file types from the BATS temporary directory.
    if [[ -n "$disk_arg" && -e "$disk_arg" ]]; then
      if [[ -d "$disk_arg" ]]; then
        cp -a "$disk_arg"/* "$destdir"/ 2>/dev/null || true
      else
        cp -a "$disk_arg" "$destdir"/ 2>/dev/null || true
      fi
    else
      if [[ -n "${BATS_FILE_TMPDIR:-}" && -d "$BATS_FILE_TMPDIR" ]]; then
        shopt -s nullglob
        local copied=0
        for f in "$BATS_FILE_TMPDIR"/*.img "$BATS_FILE_TMPDIR"/*.hdd "$BATS_FILE_TMPDIR"/*.raw; do
          cp -a "$f" "$destdir"/ 2>/dev/null || true
          copied=1
        done
        shopt -u nullglob
        if [[ $copied -eq 0 ]]; then
          echo "KEEP_TEST_ARTIFACTS=1: no images found in $BATS_FILE_TMPDIR" >&2
        fi
      fi
    fi

    echo "Artifacts preserved at: $destdir"
  fi
}
