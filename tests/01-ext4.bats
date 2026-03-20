#!/usr/bin/env bats
# 01-ext4.bats — ext4 filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw ext4 image (no partition table), verify file I/O, unmount
#   2. Mount with custom Linux mount option (-o noatime)
#   3. Mount read-only (-o ro) and verify writes are rejected
#   4. Remount an already-attached image (-r flag)

load 'test_helper/common'

LABEL="alfsext4"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/ext4.img" 512M
  # Subtract 16 blocks (64 KiB) from the device-reported size so the
  # superblock block count matches the smaller device mount mode exposes.
  vm_exec "${BATS_FILE_TMPDIR}/ext4.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/ext4.img"
}

# ---------------------------------------------------------------------------

@test "ext4: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "ext4: mount with noatime option" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  "$ANYLINUXFS" "$img" -w false -o noatime

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "ext4: read-only mount rejects writes" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  "$ANYLINUXFS" "$img" -w false -o ro

  local mp
  mp="$(get_mount_point "$LABEL")"
  # A write to a read-only mount must fail
  run bash -c "echo test > '${mp}/should_fail.txt'"
  [ "$status" -ne 0 ]

  do_unmount
}
