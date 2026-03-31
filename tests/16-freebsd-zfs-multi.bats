#!/usr/bin/env bats
# 16-freebsd-zfs-multi.bats — FreeBSD ZFS multi-mount test with loopback alias verification
#
# Mounts four drives simultaneously using gvproxy: two ext4 filesystems
# (explicitly via --net-helper gvproxy) and two FreeBSD ZFS pools (which
# always force gvproxy regardless of the --net-helper flag). Running four
# concurrent gvproxy instances exhausts the available loopback NFS ports and
# triggers creation of at least one new lo0 inet6 alias; this test verifies
# that and cleans up the alias afterward.
#
# Requires: sudo (anylinuxfs itself needs elevated privileges, and the lo0
# alias cleanup uses ifconfig which requires root).
#
# Tests:
#   1. Mount two ext4 images with --net-helper gvproxy and two FreeBSD ZFS
#      pools (implicitly gvproxy); verify at least one new lo0 inet6 alias
#      was created, file I/O works on all four mounts, and all aliases are
#      removed after the test.

load 'test_helper/common'

EXT_LABEL1="alfs-fbm-ext1"
EXT_LABEL2="alfs-fbm-ext2"
ZFS_POOL1="alfsfbmulti1"
ZFS_POOL2="alfsfbmulti2"

setup_file() {
  # Two ext4 images
  create_sparse_image "${BATS_FILE_TMPDIR}/ext1.img" 512M
  create_sparse_image "${BATS_FILE_TMPDIR}/ext2.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/ext1.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${EXT_LABEL1} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
  vm_exec "${BATS_FILE_TMPDIR}/ext2.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${EXT_LABEL2} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  # Two ZFS images created via the Linux microVM (modprobe zfs + zpool create)
  create_sparse_image "${BATS_FILE_TMPDIR}/zfs1.img" 1G
  create_sparse_image "${BATS_FILE_TMPDIR}/zfs2.img" 1G
  vm_exec "${BATS_FILE_TMPDIR}/zfs1.img" \
    "modprobe zfs \
     && zpool create -R /tmp -f ${ZFS_POOL1} /dev/vda \
     && zfs create ${ZFS_POOL1}/data \
     && chown -R $(id -u):$(id -g) /tmp/${ZFS_POOL1} \
     && zpool export ${ZFS_POOL1}"
  vm_exec "${BATS_FILE_TMPDIR}/zfs2.img" \
    "modprobe zfs \
     && zpool create -R /tmp -f ${ZFS_POOL2} /dev/vda \
     && zfs create ${ZFS_POOL2}/data \
     && chown -R $(id -u):$(id -g) /tmp/${ZFS_POOL2} \
     && zpool export ${ZFS_POOL2}"
}

teardown() {
  [[ -n "${ZFS1_DEV:-}" ]] && hdiutil_detach "$ZFS1_DEV" && ZFS1_DEV=""
  [[ -n "${ZFS2_DEV:-}" ]] && hdiutil_detach "$ZFS2_DEV" && ZFS2_DEV=""
  cleanup_lo0_aliases
}

# Remove all inet6 lo0 aliases that are not the two standard ones:
#   ::1              (IPv6 loopback)
#   fe80::1%lo0      (link-local default)
# anylinuxfs adds fe80::-range aliases with random suffixes when the NFS ports
# are exhausted on the existing lo0 addresses. All such extras are safe to
# remove after the test.
cleanup_lo0_aliases() {
  ifconfig lo0 | awk '/inet6/ {print $2}' \
    | grep -v '^::1$' \
    | grep -vE '^fe80::1(%lo0)?$' \
    | while IFS= read -r addr; do
        ifconfig lo0 inet6 "$addr" remove 2>/dev/null || true
      done
}

# ---------------------------------------------------------------------------

@test "freebsd-zfs-multi: four gvproxy mounts trigger loopback alias creation and cleanup" {
  skip "TODO: need to make it more reliable"

  local ext1_img="${BATS_FILE_TMPDIR}/ext1.img"
  local ext2_img="${BATS_FILE_TMPDIR}/ext2.img"
  local zfs1_img="${BATS_FILE_TMPDIR}/zfs1.img"
  local zfs2_img="${BATS_FILE_TMPDIR}/zfs2.img"

  # Count lo0 inet6 addresses before mounting; the fourth concurrent gvproxy
  # instance will exhaust the three standard addresses (::1, fe80::1%lo0,
  # 127.0.0.1 is IPv4 so not counted here) and force creation of a new one.
  local lo0_before_count
  lo0_before_count="$(ifconfig lo0 | grep -c 'inet6')"

  # Mount two ext4 filesystems, explicitly choosing gvproxy as the network
  # helper.
  "$ANYLINUXFS" "$ext1_img" --net-helper gvproxy -w false
  "$ANYLINUXFS" "$ext2_img" --net-helper gvproxy -w false

  # ZFS automatically creates a partition table, so anylinuxfs expects an
  # individual partition device rather than the whole image file.
  hdiutil_attach "$zfs1_img"
  ZFS1_DEV="$HDIUTIL_DEV"
  export ZFS1_DEV
  hdiutil_attach "$zfs2_img"
  ZFS2_DEV="$HDIUTIL_DEV"
  export ZFS2_DEV

  # Mount two FreeBSD ZFS pools. FreeBSD always uses gvproxy internally
  # regardless of the --net-helper flag, so these also go through gvproxy.
  "$ANYLINUXFS" "${ZFS1_DEV}s1" --zfs-os freebsd -w false
  "$ANYLINUXFS" "${ZFS2_DEV}s1" --zfs-os freebsd -w false

  # Verify that at least one new lo0 inet6 alias was created.
  local lo0_after_count
  lo0_after_count="$(ifconfig lo0 | grep -c 'inet6')"
  [ "$lo0_after_count" -gt "$lo0_before_count" ]

  # Verify file I/O on all four mounts.
  assert_file_roundtrip "$(get_mount_point "$EXT_LABEL1")"
  assert_file_roundtrip "$(get_mount_point "$EXT_LABEL2")"
  assert_file_roundtrip "$(get_mount_point "zfs_root/$ZFS_POOL1")"
  assert_file_roundtrip "$(get_mount_point "zfs_root-1/$ZFS_POOL2")"

  do_unmount
}
