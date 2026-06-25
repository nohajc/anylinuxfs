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

VG="alfs11vg"
LV1="alfs11lv1"
LV2="alfs11lv2"
LV1_LABEL="alfs11lvm1"
LV2_LABEL="alfs11lvm2"

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
  local targets=(
    "lvm:${VG}:${BATS_FILE_TMPDIR}/lvm.img:${LV1}" \
    "lvm:${VG}:${BATS_FILE_TMPDIR}/lvm.img:${LV2}"
  )
  if [[ -n "${ATTACH_DEV:-}" ]]; then
    targets+=("lvm:${VG}:${ATTACH_DEV}:${LV1}" "lvm:${VG}:${ATTACH_DEV}:${LV2}")
  fi
  safe_teardown "${targets[@]}"
}

# ---------------------------------------------------------------------------

@test "lvm: mount first logical volume, file roundtrip, unmount" {
  local disk_id="lvm:${VG}:${BATS_FILE_TMPDIR}/lvm.img:${LV1}"
  do_mount "$disk_id"

  assert_file_roundtrip "$(mounted_path_for "$disk_id" "$LV1_LABEL")"

  do_unmount "$disk_id"
}

@test "lvm: mount second logical volume, file roundtrip, unmount" {
  local disk_id="lvm:${VG}:${BATS_FILE_TMPDIR}/lvm.img:${LV2}"
  do_mount "$disk_id"

  assert_file_roundtrip "$(mounted_path_for "$disk_id" "$LV2_LABEL")"

  do_unmount "$disk_id"
}

@test "lvm: mount first logical volume via hdiutil-attached device, file roundtrip, unmount" {
  attach_image "${BATS_FILE_TMPDIR}/lvm.img"
  local disk_id="lvm:${VG}:${ATTACH_DEV}:${LV1}"
  do_mount "$disk_id"

  assert_file_roundtrip "$(mounted_path_for "$disk_id" "$LV1_LABEL")"

  do_unmount "$disk_id"
}

@test "lvm: mount second logical volume via hdiutil-attached device, file roundtrip, unmount" {
  attach_image "${BATS_FILE_TMPDIR}/lvm.img"
  local disk_id="lvm:${VG}:${ATTACH_DEV}:${LV2}"
  do_mount "$disk_id"

  assert_file_roundtrip "$(mounted_path_for "$disk_id" "$LV2_LABEL")"

  do_unmount "$disk_id"
}
