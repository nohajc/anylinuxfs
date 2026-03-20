#!/usr/bin/env bats
# 14-multi-disk-btrfs.bats — multi-disk btrfs RAID1 mount/unmount tests
#
# Two sparse images are formatted together as a btrfs RAID1 volume.
# The colon-separated multi-disk syntax is used for both the shell command
# and the mount command.
#
# Tests:
#   1. Mount two-disk btrfs RAID1, file I/O, unmount
#   2. Mount same pool read-only (-o ro)

load 'test_helper/common'

LABEL="alfsmbtrfs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/btrfs0.img" 512M
  create_sparse_image "${BATS_FILE_TMPDIR}/btrfs1.img" 512M

  # Colon-separated syntax passes both images to the VM as /dev/vda and /dev/vdb
  vm_exec "${BATS_FILE_TMPDIR}/btrfs0.img:${BATS_FILE_TMPDIR}/btrfs1.img" \
    "mkfs.btrfs -L ${LABEL} -d raid1 -m raid1 /dev/vda /dev/vdb"
}

teardown() {
  local disk_id="${BATS_FILE_TMPDIR}/btrfs0.img:${BATS_FILE_TMPDIR}/btrfs1.img"
  safe_teardown "$disk_id"
}

# ---------------------------------------------------------------------------

@test "multi-disk btrfs: RAID1 mount, file roundtrip, unmount" {
  local disk_id="${BATS_FILE_TMPDIR}/btrfs0.img:${BATS_FILE_TMPDIR}/btrfs1.img"
  "$ANYLINUXFS" "$disk_id" 

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "multi-disk btrfs: RAID1 read-only mount" {
  local disk_id="${BATS_FILE_TMPDIR}/btrfs0.img:${BATS_FILE_TMPDIR}/btrfs1.img"
  "$ANYLINUXFS" "$disk_id" -o ro 

  local mp
  mp="$(get_mount_point "$LABEL")"

  # Writes must fail on a read-only mount
  run bash -c "echo test > '${mp}/should_fail.txt'"
  [ "$status" -ne 0 ]

  do_unmount
}
