#!/usr/bin/env bats
# 05-ntfs.bats — NTFS filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw NTFS image using the ntfs3 in-kernel driver, file I/O, unmount
#   2. Mount using ntfs-3g FUSE driver
#   3. --remount takes over a macOS read-only native NTFS mount for read-write access

load 'test_helper/common'

LABEL="alfsntfs"
REMOUNT_LABEL="alfsntfsrm"

setup_file() {
  # Raw whole-disk NTFS — used by the driver tests.
  create_sparse_image "${BATS_FILE_TMPDIR}/ntfs.img" 512M
  # Subtract 64 KiB (128 sectors) so mkfs.ntfs sizes the filesystem to fit
  # within the device that mount mode exposes.
  vm_exec "${BATS_FILE_TMPDIR}/ntfs.img" \
    "mkfs.ntfs -f -L ${LABEL} --sectors-per-track 63 \
      /dev/vda \$(( \$(blockdev --getsz /dev/vda) - 128 ))"

  # GPT-partitioned NTFS — used by the --remount test.
  # macOS's diskarbitrationd only auto-mounts volumes that sit inside a
  # recognised partition table, so we need GPT here.
  # parted assigns the "Microsoft Basic Data" GUID to an ntfs-typed partition
  # on GPT, which is what macOS uses to identify and auto-mount NTFS volumes.
  create_sparse_image "${BATS_FILE_TMPDIR}/ntfs-remount.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/ntfs-remount.img" \
    "parted -s /dev/vda mklabel gpt mkpart primary ntfs 1MiB 510MiB \
     && mkfs.ntfs -f -L ${REMOUNT_LABEL} --sectors-per-track 63 /dev/vda1"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "ntfs: mount with ntfs3 driver, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  "$ANYLINUXFS" "$img" -t ntfs3 -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "ntfs: mount with ntfs-3g driver, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  "$ANYLINUXFS" "$img" -t ntfs-3g -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}

@test "ntfs: --remount takes over macOS read-only native mount for read-write access" {
  local img="${BATS_FILE_TMPDIR}/ntfs-remount.img"

  # Attach without -nomount so macOS's diskarbitrationd auto-mounts the NTFS
  # partition read-only at /Volumes/<REMOUNT_LABEL>.
  local dev
  dev="$(hdiutil_attach_automount "$img")"
  local part_dev="${dev}s1"

  # Poll until macOS completes the auto-mount (usually instant, but be safe).
  local retries=15
  while [[ ! -d "/Volumes/${REMOUNT_LABEL}" && $retries -gt 0 ]]; do
    sleep 1
    (( retries-- ))
  done
  [[ -d "/Volumes/${REMOUNT_LABEL}" ]]

  # Without -r: anylinuxfs should refuse because the disk is already mounted.
  run "$ANYLINUXFS" "$part_dev" -w false
  [ "$status" -ne 0 ]

  # With -r: anylinuxfs unmounts the macOS read-only mount first, then mounts
  # the volume read-write via the Linux VM.
  "$ANYLINUXFS" "$part_dev" -r -w false

  assert_file_roundtrip "$(get_mount_point "$REMOUNT_LABEL")"

  do_unmount
  hdiutil_detach "$dev"
  HDIUTIL_DEV=""
}
