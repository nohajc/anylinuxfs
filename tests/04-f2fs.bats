#!/usr/bin/env bats
# 04-f2fs.bats — F2FS filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw F2FS image, file I/O, unmount

load 'test_helper/common'

LABEL="alfs04f2fs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/f2fs.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/f2fs.img" \
    "mkfs.f2fs -R $(id -u):$(id -g) -l ${LABEL} /dev/vda"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/f2fs.img"
}

# ---------------------------------------------------------------------------

@test "f2fs: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/f2fs.img"
  do_mount "$img"

  assert_file_roundtrip "$(mounted_path_for "$img" "$LABEL")"

  do_unmount "$img"
}
