#!/usr/bin/env bats
# 06-zfs.bats — ZFS filesystem mount/unmount tests
#
# Two images are created: one unencrypted pool (mounted twice for platform tests)
# and one encrypted pool (mounted twice with ALFS_PASSPHRASE for encryption tests).
#
# Tests:
#   1. Mount unencrypted ZFS pool with --zfs-os freebsd, file I/O, unmount
#   2. Mount unencrypted ZFS pool with --zfs-os linux, file I/O, unmount
#   3. Mount encrypted ZFS pool with --zfs-os freebsd using ALFS_PASSPHRASE, file I/O, unmount
#   4. Mount encrypted ZFS pool with --zfs-os linux using ALFS_PASSPHRASE, file I/O, unmount

load 'test_helper/common'

# zpool name is also the volume label as seen by diskutil
POOL="alfszfspool"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/zfs.img" 1G
  # Create and immediately export pool so it can be imported fresh each test.
  vm_exec "${BATS_FILE_TMPDIR}/zfs.img" \
    "modprobe zfs && zpool create -R /tmp -f ${POOL} /dev/vda \
     && zfs create ${POOL}/data \
     && chown -R $(id -u):$(id -g) /tmp/${POOL} \
     && zpool export ${POOL}"

  # --- Encrypted ZFS pool with ALFS_PASSPHRASE ---
  create_sparse_image "${BATS_FILE_TMPDIR}/zfs-encrypted.img" 1G
  vm_exec "${BATS_FILE_TMPDIR}/zfs-encrypted.img" \
    "modprobe zfs && echo -n 'alfszfsencryptedpass' | zpool create -R /tmp -f \
         -O encryption=on -O keyformat=passphrase -O keylocation=prompt \
         ${POOL} /dev/vda \
     && zfs create ${POOL}/data \
     && chown -R $(id -u):$(id -g) /tmp/${POOL} \
     && zpool export ${POOL}"
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/zfs.img"
  safe_teardown "${BATS_FILE_TMPDIR}/zfs-encrypted.img"
}

# ---------------------------------------------------------------------------

@test "zfs: mount with FreeBSD kernel, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs.img"
  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"

  "$ANYLINUXFS" "$part_dev" --zfs-os freebsd -w false

  assert_file_roundtrip "$(get_mount_point "zfs_root/$POOL")"

  do_unmount
}

@test "zfs: mount with Linux kernel, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs.img"
  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"

  "$ANYLINUXFS" "$part_dev" --zfs-os linux -w false

  assert_file_roundtrip "$(get_mount_point "zfs_root/$POOL")"

  do_unmount
}

@test "zfs: mount encrypted with FreeBSD kernel using ALFS_PASSPHRASE, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs-encrypted.img"
  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"

  ALFS_PASSPHRASE="alfszfsencryptedpass" "$ANYLINUXFS" "$part_dev" --zfs-os freebsd -w false

  assert_file_roundtrip "$(get_mount_point "zfs_root/$POOL")"

  do_unmount
}

@test "zfs: mount encrypted with Linux kernel using ALFS_PASSPHRASE, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs-encrypted.img"
  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"

  ALFS_PASSPHRASE="alfszfsencryptedpass" "$ANYLINUXFS" "$part_dev" --zfs-os linux -w false

  assert_file_roundtrip "$(get_mount_point "zfs_root/$POOL")"

  do_unmount
}
