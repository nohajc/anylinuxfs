#!/usr/bin/env bats
# 02-btrfs.bats — btrfs filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw single-disk btrfs image, file I/O, unmount
#   2. Mount with a Linux mount option (-o compress=zstd)

load 'test_helper/common'

LABEL="alfsbtrfs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/btrfs.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/btrfs.img" \
    "mkfs.btrfs -L ${LABEL} /dev/vda && \
     mount /dev/vda /mnt && \
     chown $(id -u):$(id -g) /mnt && \
     umount /mnt"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/btrfs.img"
}

# ---------------------------------------------------------------------------

@test "btrfs: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/btrfs.img"
  "$ANYLINUXFS" "$img" 

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "btrfs: mount with compress=zstd option" {
  local img="${BATS_FILE_TMPDIR}/btrfs.img"
  "$ANYLINUXFS" "$img" -o compress=zstd 

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
