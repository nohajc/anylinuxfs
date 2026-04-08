#!/usr/bin/env bats
# 22-raid.bats — Linux software RAID (mdadm) tests
#
# Tests:
#   1. Mount a RAID1 array formed from two ext4 images
#
# Note: LVM-on-RAID is implicitly tested via the hdiutil-attached LVM tests
# in 11-lvm.bats (which use /dev/diskXsY notation that supports RAID arrays).
#
# Mount identifier syntax:
#   raid:disk1[:disk2[:...]]     (colon-separated list of disks forming the array)

load 'test_helper/common'

RAID_LABEL="alfsraid1"

setup_file() {
  # Two equal-sized sparse images — will be assembled into RAID1.
  # We create them raw and format them during the test via the VM.
  create_sparse_image "${BATS_FILE_TMPDIR}/raid1a.img" 256M
  create_sparse_image "${BATS_FILE_TMPDIR}/raid1b.img" 256M
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "raid: mount RAID1 array formed from two images, file roundtrip, unmount" {
  local raid_id="raid:${BATS_FILE_TMPDIR}/raid1a.img:${BATS_FILE_TMPDIR}/raid1b.img"

  # Set up the RAID1 filesystem via anylinuxfs shell.
  # anylinuxfs handles the mdadm --create step automatically when it sees
  # the "raid:" prefix.
  "$ANYLINUXFS" shell -c \
    "mdadm --create --run /dev/md/alfsraid --level=1 --raid-devices=2 /dev/vda /dev/vdb && \
     mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${RAID_LABEL} /dev/md/alfsraid" \
    "$raid_id" > /dev/null 2>&1

  # Now mount it with anylinuxfs — it will assemble the array again.
  "$ANYLINUXFS" "$raid_id" -w false

  assert_file_roundtrip "$(get_mount_point "$RAID_LABEL")"

  do_unmount
}


