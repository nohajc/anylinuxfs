#!/usr/bin/env bats
# 13-attach-image.bats — tests for mounting via virtual block devices
#
# Image attachment exposes a raw disk image as a regular block device, letting
# anylinuxfs address it as if it were a physical disk:
#   macOS: hdiutil attach   ->  /dev/diskNsM   (slice notation)
#   Linux: losetup -P       ->  /dev/loopNpM   (partition notation)
#
# Tests:
#   1. Attach raw (no-partition-table) ext4 image, mount whole disk device, unmount, detach
#   2. Attach GPT-partitioned image, mount specific partition, unmount, detach

load 'test_helper/common'

RAW_LABEL="alfshdraw"
GPT_LABEL="alfshgpt"

setup_file() {
  # Raw ext4 (no partition table) — subtract 16 blocks to match mount-mode
  # device size.
  create_sparse_image "${BATS_FILE_TMPDIR}/hdi-raw.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/hdi-raw.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${RAW_LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  # GPT with single ext4 partition — end at 510MiB to keep the partition
  # away from the 64 KiB that mount mode trims from the raw device.
  create_sparse_image "${BATS_FILE_TMPDIR}/hdi-gpt.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/hdi-gpt.img" \
    "parted -s /dev/vda mklabel gpt mkpart primary ext4 1MiB 510MiB \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${GPT_LABEL} /dev/vda1"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "attach-image: raw image, mount whole-disk device, unmount, detach" {
  attach_image "${BATS_FILE_TMPDIR}/hdi-raw.img"

  do_mount "$ATTACH_DEV"

  assert_file_roundtrip "$(get_mount_point "$RAW_LABEL")"

  do_unmount
}

@test "attach-image: GPT image, mount first partition, unmount, detach" {
  attach_image "${BATS_FILE_TMPDIR}/hdi-gpt.img"
  local part_dev="$(partition_dev "$ATTACH_DEV" 1)"

  do_mount "$part_dev"

  assert_file_roundtrip "$(get_mount_point "$GPT_LABEL")"

  do_unmount
}
