#!/usr/bin/env bats
# 07-ufs.bats — ufs filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw ufs image (no partition table), verify file I/O, unmount

load 'test_helper/common'

LABEL="alfsufs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/ufs.img" 512M
  vm_exec_freebsd "${BATS_FILE_TMPDIR}/ufs.img" \
    "newfs -L $LABEL /dev/vtbd1 && mount /dev/vtbd1 /mnt && chown $(id -u):$(id -g) /mnt/ && umount /mnt"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/ufs.img"
}

# ---------------------------------------------------------------------------

@test "ufs: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ufs.img"
  "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
