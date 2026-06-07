#!/usr/bin/env bash
# Shared helpers for anylinuxfs e2e tests.
# Loaded via `load 'test_helper/common'` at the top of each .bats file.

# ---------------------------------------------------------------------------
# Host OS detection
# ---------------------------------------------------------------------------
HOST_OS="$(uname -s)"

# ---------------------------------------------------------------------------
# Binary resolution
# ---------------------------------------------------------------------------
# Override ANYLINUXFS_BIN in the environment to point at an alternate binary.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ANYLINUXFS="${ANYLINUXFS_BIN:-"${REPO_ROOT}/bin/anylinuxfs"}"

# user_home_dir
#   Resolves the invoking user's home directory. Under sudo, $HOME points at
#   /root on Linux, while anylinuxfs reads ~/.anylinuxfs/ for the user named
#   in SUDO_USER (the original invoker). Tests that touch the config file or
#   the default mount point need the invoker's home, not root's.
user_home_dir() {
  if [[ -n "${SUDO_USER:-}" ]]; then
    eval echo "~${SUDO_USER}"
  else
    echo "$HOME"
  fi
}

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
# Mount wrapper
# ---------------------------------------------------------------------------
# do_mount [...args]
#   Invokes anylinuxfs to mount a disk. Appends `-w false` on macOS to
#   suppress the Finder window that would otherwise pop open for each test
#   mount; that flag belongs to the mount subcommand (gated behind
#   #[cfg(target_os = "macos")] in cli.rs) and would error out on Linux.
#   Append rather than prepend so the wrapper works whether the caller uses
#   the implicit mount form (\`do_mount /tmp/x.img\`) or names the subcommand
#   explicitly (\`do_mount mount -a action_name\`).
do_mount() {
  if [[ "$HOST_OS" == "Darwin" ]]; then
    "$ANYLINUXFS" "$@" -w false
  else
    "$ANYLINUXFS" "$@"
  fi
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
#   Returns the expected mount path for a volume with the given label.
#   macOS: /Volumes/<label> (or ~/Volumes/<label> for non-root invocations)
#   Linux: /mnt/<label>     (or ~/mnt/<label> for non-root invocations)
get_mount_point() {
  local base
  if [[ "$HOST_OS" == "Darwin" ]]; then
    base="Volumes"
  else
    base="mnt"
  fi
  if [[ $(id -u) -eq 0 ]]; then
    echo "/${base}/${1}"
  else
    echo "${HOME}/${base}/${1}"
  fi
}

# partition_dev <attach_dev> <part_num>
#   Compose the partition device node for a virtual disk.
#   macOS hdiutil:  /dev/disk5  -> /dev/disk5s1
#   Linux losetup:  /dev/loop0  -> /dev/loop0p1
partition_dev() {
  local dev="$1" num="$2"
  if [[ "$HOST_OS" == "Darwin" ]]; then
    echo "${dev}s${num}"
  else
    echo "${dev}p${num}"
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
# List-output assertions
# ---------------------------------------------------------------------------
# normalize_list_output <output>
#   Replaces volatile paths and size values while preserving the rest of
#   anylinuxfs list's row text for exact section comparisons. Size fields
#   are normalized to the formatter's 10-character column width.
normalize_list_output() {
  local output="$1"
  if [[ -n "${BATS_FILE_TMPDIR:-}" ]]; then
    output="${output//"$BATS_FILE_TMPDIR"/<TMP>}"
  fi
  printf '%s\n' "$output" \
    | sed -E \
      -e 's/([+*]?)[0-9]+([.][0-9]+)? [KMGTPE]?B/\1<SIZE>/g' \
      -e 's/([+*]?)<SIZE> +/\1<SIZE>     /g'
}

# extract_list_section <normalized-output> <heading>
#   Prints the section that starts at <heading> and ends before the next blank
#   line. The caller should pass normalized output and a normalized heading.
extract_list_section() {
  local output="$1"
  local heading="$2"
  awk -v heading="$heading" '
    found && $0 == "" { exit }
    $0 == heading { found = 1 }
    found { print }
  ' <<< "$output"
}

# assert_list_section <output> <normalized-heading> <expected-normalized-section>
assert_list_section() {
  local output="$1"
  local heading="$2"
  local expected="$3"
  local normalized section

  normalized="$(normalize_list_output "$output")"
  section="$(extract_list_section "$normalized" "$heading")"

  if [[ -z "$section" ]]; then
    echo "FAIL: list section not found: $heading" >&2
    echo "Normalized output:" >&2
    echo "$normalized" >&2
    return 1
  fi

  if [[ "$section" != "$expected" ]]; then
    echo "FAIL: list section mismatch: $heading" >&2
    diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$section") >&2 || true
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
# Virtual-disk attach helpers
# ---------------------------------------------------------------------------
# attach_image <image_path>
#   Attaches a raw disk image as a virtual block device (no auto-mount).
#   macOS: hdiutil attach -nomount  ->  /dev/diskN
#   Linux: losetup -P -f --show     ->  /dev/loopN
#          (-P forces the kernel to scan the partition table and create
#          /dev/loopNpX nodes for any partitions; harmless on raw images.)
#   Prints the device node to stdout and sets ATTACH_DEV.
attach_image() {
  local image_path="$1"
  local dev out
  if [[ "$HOST_OS" == "Darwin" ]]; then
    out="$(hdiutil attach \
      -imagekey diskimage-class=CRawDiskImage \
      -nomount \
      "$image_path" 2>&1)"
    # hdiutil prints one line per partition; the first is the whole disk.
    dev="$(echo "$out" | awk 'NR==1{print $1}')"
  else
    out="$(losetup -P -f --show "$image_path" 2>&1)"
    dev="$out"
    # Wait for udev to finish processing the hotplug events (blkid scan,
    # lvm2-pvscan, etc.) it triggers on a fresh loop device. Without this,
    # anylinuxfs can lose a flock race with the kernel-triggered scanners
    # and fail with "file already locked".
    udevadm settle 2>/dev/null || true
  fi
  if [[ -z "$dev" || ! -b "$dev" ]]; then
    echo "ERROR: attach_image failed for $image_path" >&2
    echo "$out" >&2
    return 1
  fi
  ATTACH_DEV="$dev"
  export ATTACH_DEV
  echo "$dev"
}

# attach_image_automount <image_path>
#   Like attach_image but lets the host auto-mount any recognised volumes.
#   Used by --remount tests that need a disk to already be mounted natively
#   before anylinuxfs takes over.
#   macOS: hdiutil attach (no -nomount)  -> diskarbitrationd auto-mounts
#   Linux: no built-in equivalent — losetup never auto-mounts. Callers that
#   require an already-mounted volume must mount it manually after attach.
attach_image_automount() {
  local image_path="$1"
  local dev out
  if [[ "$HOST_OS" == "Darwin" ]]; then
    out="$(hdiutil attach \
      -imagekey diskimage-class=CRawDiskImage \
      "$image_path" 2>&1)"
    dev="$(echo "$out" | awk 'NR==1{print $1}')"
  else
    # No auto-mount on Linux — fall back to plain attach. Tests that rely
    # on auto-mount semantics should skip on non-Darwin hosts.
    attach_image "$image_path"
    return $?
  fi
  if [[ -z "$dev" || ! -b "$dev" ]]; then
    echo "ERROR: attach_image_automount failed for $image_path" >&2
    echo "$out" >&2
    return 1
  fi
  ATTACH_DEV="$dev"
  export ATTACH_DEV
  echo "$dev"
}

# detach_image <dev_node>
#   Detaches a virtual disk.
#   macOS: hdiutil detach (does not require sudo when the attacher matches).
#   Linux: losetup -d
detach_image() {
  local dev="$1"
  if [[ "$HOST_OS" == "Darwin" ]]; then
    hdiutil detach "$dev" 2>/dev/null || true
  else
    losetup -d "$dev" 2>/dev/null || true
  fi
}

# ---------------------------------------------------------------------------
# Generic teardown called from each test's teardown()
# ---------------------------------------------------------------------------
# safe_teardown [disk_arg]
#   Unmounts (best-effort), detaches any attached image, removes TEST_DIR.
safe_teardown() {
  local disk_arg="${1:-}"
  do_unmount
  if [[ -n "${ATTACH_DEV:-}" ]]; then
    detach_image "$ATTACH_DEV"
    ATTACH_DEV=""
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
