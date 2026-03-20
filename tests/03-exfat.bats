#!/usr/bin/env bats
# 03-exfat.bats — exFAT filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw exFAT image, file I/O, unmount

load 'test_helper/common'

LABEL="alfsexfat"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/exfat.img" 512M
  # exfat auto-detects device size; no need to specify it.
  vm_exec "${BATS_FILE_TMPDIR}/exfat.img" \
    "mkfs.exfat -L ${LABEL} /dev/vda"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/exfat.img"
}

# ---------------------------------------------------------------------------

@test "exfat: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/exfat.img"
  "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
