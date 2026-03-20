#!/usr/bin/env bats
# 10-partitioned-disk.bats — partition table tests
#
# Tests raw disk images containing a partition table rather than a bare
# filesystem.  Both GPT and MBR layouts are covered.  Each test uses
# a fresh image so partition device nodes are stable (/dev/vda1, /dev/vda2).
#
# Tests:
#   1. GPT disk, single ext4 partition — mount via direct image path
#   2. GPT disk, single ext4 partition — mount via hdiutil-attached /dev/diskXs1
#   3. MBR disk, first partition ext4, second partition btrfs — mount each in turn

load 'test_helper/common'

GPT_LABEL="alfsgpt"
MBR1_LABEL="alfsmbrp1"
MBR2_LABEL="alfsmbrp2"

setup_file() {
  # --- GPT image ---
  # End partition at 510MiB rather than 100% so the last partition never
  # reaches the 64 KiB that mount mode trims from the raw device.
  create_sparse_image "${BATS_FILE_TMPDIR}/gpt.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/gpt.img" \
    "parted -s /dev/vda mklabel gpt mkpart primary ext4 1MiB 510MiB \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${GPT_LABEL} \
          \$(( \$(blockdev --getsz /dev/vda1) / 8 - 16 )) /dev/vda1"

  # --- MBR image ---
  create_sparse_image "${BATS_FILE_TMPDIR}/mbr.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/mbr.img" \
    "parted -s /dev/vda mklabel msdos \
       mkpart primary ext4  1MiB 255MiB \
       mkpart primary btrfs 256MiB 510MiB \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${MBR1_LABEL} \
          \$(( \$(blockdev --getsz /dev/vda1) / 8 - 16 )) /dev/vda1 \
     && mkfs.btrfs -L ${MBR2_LABEL} /dev/vda2"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "partitioned: GPT ext4 — direct image path mount" {
  "$ANYLINUXFS" "$GPT_IMG" &
  wait_for_mount "$GPT_LABEL"

  assert_file_roundtrip "$(get_mount_point "$GPT_LABEL")"

  do_unmount
}

@test "partitioned: GPT ext4 — hdiutil-attached /dev/diskXs1 mount" {
  local whole_dev
  whole_dev="$(hdiutil_attach "$GPT_IMG")"
  local part_dev="${whole_dev}s1"

  "$ANYLINUXFS" "$part_dev" &
  wait_for_mount "$GPT_LABEL"

  assert_file_roundtrip "$(get_mount_point "$GPT_LABEL")"

  do_unmount
  hdiutil_detach "$whole_dev"
  HDIUTIL_DEV=""
}

@test "partitioned: MBR first partition (ext4)" {
  "$ANYLINUXFS" "$MBR_IMG" &
  wait_for_mount "$MBR1_LABEL"

  assert_file_roundtrip "$(get_mount_point "$MBR1_LABEL")"

  do_unmount
}

@test "partitioned: MBR second partition (btrfs) via direct path" {
  # anylinuxfs auto-detects partitions; pass the image and let it find vda2
  # by specifying the partition offset explicitly is not needed — anylinuxfs
  # chooses the partition whose filesystem matches.  Use hdiutil to address
  # vda2 explicitly.
  local whole_dev
  whole_dev="$(hdiutil_attach "$MBR_IMG")"
  local part_dev="${whole_dev}s2"

  "$ANYLINUXFS" "$part_dev" &
  wait_for_mount "$MBR2_LABEL"

  assert_file_roundtrip "$(get_mount_point "$MBR2_LABEL")"

  do_unmount
  hdiutil_detach "$whole_dev"
  HDIUTIL_DEV=""
}
