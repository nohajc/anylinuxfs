#!/usr/bin/env bats
# 13-hdiutil-attach.bats — tests for mounting via hdiutil-attached virtual disks
#
# hdiutil attach exposes a raw disk image as a /dev/diskX block device.
# This lets anylinuxfs address the image as if it were a physical disk,
# using /dev/diskXsY partition slice notation.
#
# Tests:
#   1. Attach raw (no-partition-table) ext4 image, mount whole disk device, unmount, detach
#   2. Attach GPT-partitioned image, mount specific partition slice (/dev/diskXs1), unmount, detach
#
# Cleanup note: hdiutil detach does not require sudo when attach was performed
# by the same (non-root) user.

load 'test_helper/common'

RAW_LABEL="alfshdraw"
GPT_LABEL="alfshgpt"

setup_file() {
  # Raw ext4 (no partition table) — subtract 16 blocks to match mount-mode
  # device size.
  create_sparse_image "${BATS_FILE_TMPDIR}/hdi-raw.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/hdi-raw.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${RAW_LABEL} \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 )) /dev/vda"

  # GPT with single ext4 partition — end at 510MiB to keep the partition
  # away from the 64 KiB that mount mode trims from the raw device.
  create_sparse_image "${BATS_FILE_TMPDIR}/hdi-gpt.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/hdi-gpt.img" \
    "parted -s /dev/vda mklabel gpt mkpart primary ext4 1MiB 510MiB \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${GPT_LABEL} \
          \$(( \$(blockdev --getsz /dev/vda1) / 8 - 16 )) /dev/vda1"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "hdiutil: attach raw image, mount whole-disk device, unmount, detach" {
  local dev
  dev="$(hdiutil_attach "${BATS_FILE_TMPDIR}/hdi-raw.img")"

  "$ANYLINUXFS" "$dev" -w false &
  wait_for_mount "$RAW_LABEL"

  assert_file_roundtrip "$(get_mount_point "$RAW_LABEL")"

  do_unmount
  hdiutil_detach "$dev"
  HDIUTIL_DEV=""
}

@test "hdiutil: attach GPT image, mount partition slice /dev/diskXs1, unmount, detach" {
  local whole_dev
  whole_dev="$(hdiutil_attach "${BATS_FILE_TMPDIR}/hdi-gpt.img")"
  local part_dev="${whole_dev}s1"

  "$ANYLINUXFS" "$part_dev" -w false &
  wait_for_mount "$GPT_LABEL"

  assert_file_roundtrip "$(get_mount_point "$GPT_LABEL")"

  do_unmount
  hdiutil_detach "$whole_dev"
  HDIUTIL_DEV=""
}
