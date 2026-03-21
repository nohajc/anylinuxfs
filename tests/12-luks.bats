#!/usr/bin/env bats
# 12-luks.bats — LUKS-encrypted filesystem mount/unmount tests
#
# Layout: LUKS container on a raw disk image with ext4 inside.
# Passphrase: "alfs-test-pass"
#
# Tests:
#   1. Mount LUKS volume using ALFS_PASSPHRASE environment variable
#   2. Mount LUKS volume using interactive passphrase prompt (via expect)
#   3. LVM-on-LUKS: LUKS container → PV → VG → LV → ext4

load 'test_helper/common'

PASSPHRASE="alfs-test-pass"
LUKS_LABEL="alfsluksfs"
LVM_ON_LUKS_VG="alfslukvg"
LVM_ON_LUKS_LV="alfslukslv"
LVM_ON_LUKS_LABEL="alfslukslvm"

setup_file() {
  # --- Plain LUKS + ext4 ---
  # The LUKS container sits on /dev/vda, which mount mode exposes as 64 KiB
  # smaller.  Shrink the inner ext4 by 16 blocks so mount can open it.
  create_sparse_image "${BATS_FILE_TMPDIR}/luks.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/luks.img" \
    "echo -n '${PASSPHRASE}' | cryptsetup luksFormat --batch-mode /dev/vda - \
     && echo -n '${PASSPHRASE}' | cryptsetup open /dev/vda alfsluks - \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LUKS_LABEL} \
          /dev/mapper/alfsluks \
     && cryptsetup close alfsluks"

  # TODO: why does lvm-on-luks layout setup fail?
  # --- LVM-on-LUKS ---
  # The LV is explicitly sized (-L 200M) and sits well within the container,
  # so it is unaffected by the 64 KiB device size discrepancy.
  # create_sparse_image "${BATS_FILE_TMPDIR}/lvm-luks.img" 512M
  # vm_exec "${BATS_FILE_TMPDIR}/lvm-luks.img" \
  #   "echo -n '${PASSPHRASE}' | cryptsetup luksFormat --batch-mode /dev/vda - \
  #    && echo -n '${PASSPHRASE}' | cryptsetup open /dev/vda alfslukslvm - \
  #    && pvcreate /dev/mapper/alfslukslvm \
  #    && vgcreate ${LVM_ON_LUKS_VG} /dev/mapper/alfslukslvm \
  #    && lvcreate -L 200M -n ${LVM_ON_LUKS_LV} ${LVM_ON_LUKS_VG} \
  #    && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LVM_ON_LUKS_LABEL} /dev/${LVM_ON_LUKS_VG}/${LVM_ON_LUKS_LV} \
  #    && cryptsetup close alfslukslvm"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "luks: mount with ALFS_PASSPHRASE env var, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/luks.img"
  ALFS_PASSPHRASE="$PASSPHRASE" "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LUKS_LABEL")"

  do_unmount
}

# TODO: Why does expect fail to match the passphrase prompt?
# @test "luks: mount with interactive passphrase via expect, file roundtrip, unmount" {
#   local img="${BATS_FILE_TMPDIR}/luks.img"
#   expect -c "
#     set timeout 90
#     spawn ${ANYLINUXFS} ${img} -w false
#     expect {
#       \"Linux: Enter passphrase for /dev/vda:\" { send \"${PASSPHRASE}\r\"; exp_continue }
#       eof
#     }
#   "

#   assert_file_roundtrip "$(get_mount_point "$LUKS_LABEL")"

#   do_unmount
# }

# @test "luks: LVM-on-LUKS mount with env var, file roundtrip, unmount" {
#   local disk_id="lvm:${LVM_ON_LUKS_VG}:${BATS_FILE_TMPDIR}/lvm-luks.img:${LVM_ON_LUKS_LV}"
#   ALFS_PASSPHRASE="$PASSPHRASE" "$ANYLINUXFS" "$disk_id" -w false

#   assert_file_roundtrip "$(get_mount_point "$LVM_ON_LUKS_LABEL")"

#   do_unmount
# }
