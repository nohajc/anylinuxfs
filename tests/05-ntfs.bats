#!/usr/bin/env bats
# 05-ntfs.bats — NTFS filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw NTFS image using the ntfs3 in-kernel driver, file I/O, unmount
#   2. Mount using ntfs-3g FUSE driver

load 'test_helper/common'

LABEL="alfsntfs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/ntfs.img" 512M
  # Subtract 64 KiB (128 sectors) so mkfs.ntfs sizes the filesystem to fit
  # within the device that mount mode exposes.
  vm_exec "${BATS_FILE_TMPDIR}/ntfs.img" \
    "mkfs.ntfs -f -L ${LABEL} --sectors-per-track 63 \
      /dev/vda \$(( \$(blockdev --getsz /dev/vda) - 128 ))"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/ntfs.img"
}

# ---------------------------------------------------------------------------

@test "ntfs: mount with ntfs3 driver, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  "$ANYLINUXFS" "$img" -t ntfs3 -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "ntfs: mount with ntfs-3g driver, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  "$ANYLINUXFS" "$img" -t ntfs-3g -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
