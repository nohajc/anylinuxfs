#!/usr/bin/env bats
# 01-ext4.bats — ext4 filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw ext4 image (no partition table), verify file I/O, unmount
#   2. Mount with custom Linux mount option (-o noatime)
#   3. Mount read-only (-o ro) and verify writes are rejected
#   4. Remount an already-attached image (-r flag)
#   5. --ignore-permissions allows file roundtrip on root-owned filesystem

load 'test_helper/common'

LABEL="alfs01ext4"
LABEL_ROOTOWNED="alfs01ext4ro"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/ext4.img" 512M
  # Subtract 16 blocks (64 KiB) from the device-reported size so the
  # superblock block count matches the smaller device mount mode exposes.
  vm_exec "${BATS_FILE_TMPDIR}/ext4.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  # Root-owned image: no root_owner flag, so / is owned by root:root.
  # Used by the --ignore-permissions test.
  create_sparse_image "${BATS_FILE_TMPDIR}/ext4-rootowned.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/ext4-rootowned.img" \
    "mkfs.ext4 -L ${LABEL_ROOTOWNED} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/ext4.img" "${BATS_FILE_TMPDIR}/ext4-rootowned.img"
}

# ---------------------------------------------------------------------------

@test "ext4: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  do_mount "$img"

  assert_file_roundtrip "$(mounted_path_for "$img" "$LABEL")"

  do_unmount "$img"
}

@test "ext4: mount with noatime option" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  do_mount "$img" -o noatime

  assert_file_roundtrip "$(mounted_path_for "$img" "$LABEL")"

  do_unmount "$img"
}

@test "ext4: read-only mount rejects writes" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  do_mount "$img" -o ro

  local mp
  mp="$(mounted_path_for "$img" "$LABEL")"
  # A write to a read-only mount must fail
  run bash -c "echo test > '${mp}/should_fail.txt'"
  [ "$status" -ne 0 ]

  do_unmount "$img"
}

@test "ext4: --ignore-permissions allows file roundtrip on root-owned filesystem" {
  # The root directory is owned by root:root, which would normally block writes.
  local img="${BATS_FILE_TMPDIR}/ext4-rootowned.img"
  do_mount "$img" --ignore-permissions

  assert_file_roundtrip "$(mounted_path_for "$img" "$LABEL_ROOTOWNED")"

  do_unmount "$img"
}
