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
vm_exec() {
  local disk_arg="$1"
  local cmd="$2"
  "$ANYLINUXFS" shell -c "$cmd" "$disk_arg"
}

# vm_exec_freebsd <disk_arg> <shell_command>
#   Same as vm_exec but uses the FreeBSD image (for ZFS formatting).
vm_exec_freebsd() {
  local disk_arg="$1"
  local cmd="$2"
  "$ANYLINUXFS" shell -i freebsd -c "$cmd" "$disk_arg"
}

# ---------------------------------------------------------------------------
# Mount detection via diskutil activity
# ---------------------------------------------------------------------------
# wait_for_mount <volume_name> [timeout_seconds]
#   Blocks until diskutil activity reports a DiskAppeared event for the NFS
#   volume at /Volumes/<volume_name>, or until the timeout (default 90s).
#
#   Example log line we match:
#   ***DiskAppeared (..., DAVolumePath = 'file:///Volumes/myfs/', DAVolumeKind = 'nfs', ...)
#
# Sets global MOUNTED_PATH to the resolved mount point on success.
wait_for_mount() {
  local volume_name="$1"
  local timeout="${2:-90}"
  local fifo
  fifo="$(mktemp -u /tmp/anylinuxfs-da.XXXXXX)"
  mkfifo "$fifo"

  # Start diskutil activity feeding into the fifo in the background.
  diskutil activity > "$fifo" &
  local da_pid=$!

  local matched=0
  local deadline=$(( $(date +%s) + timeout ))

  while IFS= read -r line; do
    if [[ "$line" == *"***DiskAppeared"* ]] \
        && [[ "$line" == *"DAVolumePath = 'file://${HOME}/Volumes/${volume_name}/'"* ]] \
        && [[ "$line" == *"DAVolumeKind = 'nfs'"* ]]; then
      matched=1
      break
    fi
    if (( $(date +%s) >= deadline )); then
      break
    fi
  done < "$fifo"

  kill "$da_pid" 2>/dev/null
  wait "$da_pid" 2>/dev/null
  rm -f "$fifo"

  if (( matched )); then
    MOUNTED_PATH="${HOME}/Volumes/${volume_name}"
    export MOUNTED_PATH
    return 0
  else
    echo "TIMEOUT: volume '${volume_name}' did not appear within ${timeout}s" >&2
    return 1
  fi
}

# get_mount_point <label>
#   Returns the expected macOS mount path for a volume with the given label.
get_mount_point() {
  echo "${HOME}/Volumes/${1}"
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
}
