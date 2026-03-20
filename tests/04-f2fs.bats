#!/usr/bin/env bats
# 04-f2fs.bats — F2FS filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw F2FS image, file I/O, unmount

load 'test_helper/common'

LABEL="alfsf2fs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/f2fs.img" 512M
  # Use the device's own reported size so mkfs sees what mount will see.
  # f2fs does not take an explicit block count argument but does auto-detect
  # device size; we pass the sector count manually to avoid the size skew.
  vm_exec "${BATS_FILE_TMPDIR}/f2fs.img" \
    "mkfs.f2fs -R $(id -u):$(id -g) -l ${LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/f2fs.img"
}

# ---------------------------------------------------------------------------

@test "f2fs: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/f2fs.img"
  "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
