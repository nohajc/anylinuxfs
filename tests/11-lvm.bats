#!/usr/bin/env bats
# 11-lvm.bats — LVM logical volume mount/unmount tests
#
# Layout: single disk image → PV → VG (testvg) → LV (testlv) → ext4
# Mount identifier syntax: lvm:<vg-name>:<disk_path>:<lv-name>
#
# Tests:
#   1. Mount an LVM logical volume, file I/O, unmount
#   2. Mount a second logical volume on the same VG

load 'test_helper/common'

VG="alfsvg"
LV1="alfslv1"
LV2="alfslv2"
LV1_LABEL="alfslvm1"
LV2_LABEL="alfslvm2"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/lvm.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/lvm.img" \
    "pvcreate /dev/vda \
     && vgcreate ${VG} /dev/vda \
     && lvcreate -L 200M -n ${LV1} ${VG} \
     && lvcreate -l 100%FREE -n ${LV2} ${VG} \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LV1_LABEL} /dev/${VG}/${LV1} \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LV2_LABEL} /dev/${VG}/${LV2}"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "lvm: mount first logical volume, file roundtrip, unmount" {
  local disk_id="lvm:${VG}:${BATS_FILE_TMPDIR}/lvm.img:${LV1}"
  "$ANYLINUXFS" "$disk_id" -w false &
  wait_for_mount "$LV1_LABEL"

  assert_file_roundtrip "$(get_mount_point "$LV1_LABEL")"

  do_unmount
}

@test "lvm: mount second logical volume, file roundtrip, unmount" {
  local disk_id="lvm:${VG}:${BATS_FILE_TMPDIR}/lvm.img:${LV2}"
  "$ANYLINUXFS" "$disk_id" -w false &
  wait_for_mount "$LV2_LABEL"

  assert_file_roundtrip "$(get_mount_point "$LV2_LABEL")"

  do_unmount
}
