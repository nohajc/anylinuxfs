#!/usr/bin/env bats
# 03-exfat.bats — exFAT filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw exFAT image, file I/O, unmount

load 'test_helper/common'

LABEL="alfsexfat"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/exfat.img" 512M
  # Subtract 16 blocks (64 KiB) to match the device size seen in mount mode.
  vm_exec "${BATS_FILE_TMPDIR}/exfat.img" \
    "mkfs.exfat -L ${LABEL} -s 4096 \$(( \$(blockdev --getsz /dev/vda) - 128 )) /dev/vda"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/exfat.img"
}

# ---------------------------------------------------------------------------

@test "exfat: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/exfat.img"
  "$ANYLINUXFS" "$img" 

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
