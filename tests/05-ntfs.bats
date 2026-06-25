#!/usr/bin/env bats
# 05-ntfs.bats — NTFS filesystem mount/unmount tests
#
# Tests:
#   1. Mount raw NTFS image using the ntfs3 in-kernel driver, file I/O, unmount
#   2. Mount using ntfs-3g FUSE driver
#   3. Names written by ntfs3 remain Unicode-correct when read by ntfs-3g
#   4. User-provided ownership options are preserved
#   5. --remount takes over a macOS read-only native NTFS mount for read-write access

load 'test_helper/common'

LABEL="alfs05ntfs"
REMOUNT_LABEL="alfs05ntfsrm"

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
  local targets=("${BATS_FILE_TMPDIR}/ntfs.img" "${BATS_FILE_TMPDIR}/ntfs-remount.img")
  if [[ -n "${ATTACH_DEV:-}" ]]; then
    targets+=("$(partition_dev "$ATTACH_DEV" 1)")
  fi
  safe_teardown "${targets[@]}"
}

# ---------------------------------------------------------------------------

@test "ntfs: mount with ntfs3 driver, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  do_mount "$img" -t ntfs3

  assert_file_roundtrip "$(mounted_path_for "$img" "$LABEL")"

  do_unmount "$img"
}

@test "ntfs: mount with ntfs-3g driver, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  do_mount "$img" -t ntfs-3g

  assert_file_roundtrip "$(mounted_path_for "$img" "$LABEL")"

  do_unmount "$img"
}

@test "ntfs: ntfs3 filenames remain Unicode-correct under ntfs-3g" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  local mount_point
  local dirname="中文目录"
  local filename="繁體中文-𠀀.txt"

  do_mount "$img" -t ntfs3
  mount_point="$(mounted_path_for "$img" "$LABEL")"
  mkdir "${mount_point}/${dirname}"
  echo "Unicode filename roundtrip" > "${mount_point}/${dirname}/${filename}"
  do_unmount "$img"

  do_mount "$img" -t ntfs-3g
  mount_point="$(mounted_path_for "$img" "$LABEL")"
  [[ -f "${mount_point}/${dirname}/${filename}" ]]
  [[ "$(cat "${mount_point}/${dirname}/${filename}")" == "Unicode filename roundtrip" ]]
  do_unmount "$img"
}

@test "ntfs: preserves user-provided ownership options" {
  local img="${BATS_FILE_TMPDIR}/ntfs.img"
  local custom_uid=123
  local default_uid="${SUDO_UID:-$(id -u)}"
  local expected_gid="${SUDO_GID:-$(id -g)}"

  do_mount "$img" -t ntfs3 -o "uid=${custom_uid}"

  run "$ANYLINUXFS" status
  [ "$status" -eq 0 ]
  [[ "$output" == *"uid=${custom_uid}"* ]]
  [[ "$output" == *"gid=${expected_gid}"* ]]
  if [[ "$default_uid" != "$custom_uid" ]]; then
    [[ "$output" != *"uid=${default_uid}"* ]]
  fi

  do_unmount "$img"
}

@test "ntfs: --remount takes over macOS read-only native mount for read-write access" {
  if [[ "$HOST_OS" != "Darwin" ]]; then
    skip "diskarbitrationd-driven auto-mount has no Linux equivalent"
  fi

  local img="${BATS_FILE_TMPDIR}/ntfs-remount.img"

  # Attach without -nomount so macOS's diskarbitrationd auto-mounts the NTFS
  # partition read-only at /Volumes/<REMOUNT_LABEL>.
  local dev
  dev="$(attach_image_automount "$img")"
  ATTACH_DEV="$dev"
  export ATTACH_DEV
  record_attached_dev "$dev"
  local part_dev="$(partition_dev "$dev" 1)"

  # Poll until macOS completes the auto-mount (usually instant, but be safe).
  local retries=15
  while [[ ! -d "/Volumes/${REMOUNT_LABEL}" && $retries -gt 0 ]]; do
    sleep 1
    (( retries-- ))
  done
  [[ -d "/Volumes/${REMOUNT_LABEL}" ]]

  # Without -r: anylinuxfs should refuse because the disk is already mounted.
  run do_mount "$part_dev"
  [ "$status" -ne 0 ]

  # With -r: anylinuxfs unmounts the macOS read-only mount first, then mounts
  # the volume read-write via the Linux VM.
  do_mount "$part_dev" -r

  assert_file_roundtrip "$(mounted_path_for "$part_dev" "$REMOUNT_LABEL")"

  do_unmount "$part_dev"
  detach_image "$dev"
  ATTACH_DEV=""
}
