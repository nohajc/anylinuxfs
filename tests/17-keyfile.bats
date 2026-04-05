#!/usr/bin/env bats
# 17-keyfile.bats — Key-file-based encryption unlock tests
#
# Tests mounting LUKS and ZFS encrypted volumes using a key file
# instead of an interactive passphrase.
#
# Tests:
#   1. LUKS: mount with --key-file CLI option, file roundtrip, unmount
#   2. LUKS: mount with ALFS_KEY_FILE env var (path to key file), file roundtrip, unmount
#   3. ZFS: mount encrypted ZFS pool with --key-file CLI option, file roundtrip, unmount

load 'test_helper/common'

LUKS_LABEL="alfskeyfilefs"
ZFS_POOL="alfskeyfilezfs"

setup_file() {
  local keyfile="${BATS_FILE_TMPDIR}/test.key"
  # Use a simple text passphrase stored in a key file.
  # cryptsetup treats this as a passphrase key file (keyformat=passphrase in LUKS terms).
  printf 'alfs-keyfile-secret' > "$keyfile"

  # --- LUKS + ext4 formatted with a key file ---
  create_sparse_image "${BATS_FILE_TMPDIR}/luks-keyfile.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/luks-keyfile.img" \
    "echo -n 'alfs-keyfile-secret' | cryptsetup luksFormat --batch-mode /dev/vda - \
     && echo -n 'alfs-keyfile-secret' | cryptsetup open /dev/vda alfsluks - \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LUKS_LABEL} \
          /dev/mapper/alfsluks \
     && cryptsetup close alfsluks"

  # --- ZFS pool encrypted with a passphrase key file ---
  create_sparse_image "${BATS_FILE_TMPDIR}/zfs-keyfile.img" 1G
  vm_exec "${BATS_FILE_TMPDIR}/zfs-keyfile.img" \
    "modprobe zfs \
     && echo -n 'alfs-keyfile-secret' | zpool create -R /tmp -f \
          -O encryption=on -O keyformat=passphrase -O keylocation=prompt \
          ${ZFS_POOL} /dev/vda \
     && zfs create ${ZFS_POOL}/data \
     && chown -R $(id -u):$(id -g) /tmp/${ZFS_POOL} \
     && zpool export ${ZFS_POOL}"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "keyfile: LUKS mount with --key-file option, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/luks-keyfile.img"
  local keyfile="${BATS_FILE_TMPDIR}/test.key"

  "$ANYLINUXFS" "$img" --key-file "$keyfile" -w false

  assert_file_roundtrip "$(get_mount_point "$LUKS_LABEL")"

  do_unmount
}

@test "keyfile: LUKS mount with ALFS_KEY_FILE env var, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/luks-keyfile.img"
  local keyfile="${BATS_FILE_TMPDIR}/test.key"

  ALFS_KEY_FILE="$keyfile" "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LUKS_LABEL")"

  do_unmount
}

@test "keyfile: Linux ZFS mount with --key-file option, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs-keyfile.img"
  local keyfile="${BATS_FILE_TMPDIR}/test.key"

  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"

  "$ANYLINUXFS" "$part_dev" --key-file "$keyfile" --zfs-os linux -w false

  assert_file_roundtrip "$(get_mount_point "zfs_root/${ZFS_POOL}")"

  do_unmount
}

@test "keyfile: FreeBSD ZFS mount with --key-file option, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs-keyfile.img"
  local keyfile="${BATS_FILE_TMPDIR}/test.key"

  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"

  "$ANYLINUXFS" "$part_dev" --key-file "$keyfile" --zfs-os freebsd -w false

  assert_file_roundtrip "$(get_mount_point "zfs_root/${ZFS_POOL}")"

  do_unmount
}
