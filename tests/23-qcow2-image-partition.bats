#!/usr/bin/env bats
# 23-qcow2-image-partition.bats — tests for mounting qcow2 partitions via @sN.

load 'test_helper/common'

QCOW_PART1_LABEL="qcowpart1"
QCOW_PART2_LABEL="qcowpart2"
QCOW_WHOLE_LABEL="qcowwhole"
QCOW_LVM_VG="qcowvg"
QCOW_LVM_LV="qcowlv"
QCOW_LVM_LABEL="qcowlvm"
RAW_FALLBACK_LABEL="rawfallback"

setup_file() {
  command -v qemu-img >/dev/null 2>&1 || skip "qemu-img is required for qcow2 tests"

  qemu-img create -f qcow2 "${BATS_FILE_TMPDIR}/test-partitioned.qcow2" 512M

  vm_exec "${BATS_FILE_TMPDIR}/test-partitioned.qcow2" \
    "parted -s /dev/vda mklabel gpt mkpart primary ext4 1MiB 256MiB mkpart primary ext4 256MiB 511MiB && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${QCOW_PART1_LABEL} /dev/vda1 && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${QCOW_PART2_LABEL} /dev/vda2"

  qemu-img create -f qcow2 "${BATS_FILE_TMPDIR}/test-whole.qcow2" 256M
  vm_exec "${BATS_FILE_TMPDIR}/test-whole.qcow2" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${QCOW_WHOLE_LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  qemu-img create -f qcow2 "${BATS_FILE_TMPDIR}/test-lvm.qcow2" 512M
  vm_exec "${BATS_FILE_TMPDIR}/test-lvm.qcow2" \
    "pvcreate /dev/vda \
     && vgcreate ${QCOW_LVM_VG} /dev/vda \
     && lvcreate -L 200M -n ${QCOW_LVM_LV} ${QCOW_LVM_VG} \
     && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${QCOW_LVM_LABEL} /dev/${QCOW_LVM_VG}/${QCOW_LVM_LV}"

  create_sparse_image "${BATS_FILE_TMPDIR}/fallback.img" 128M
  vm_exec "${BATS_FILE_TMPDIR}/fallback.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${RAW_FALLBACK_LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  printf "not a qcow2 image" > "${BATS_FILE_TMPDIR}/invalid.qcow2"
}

teardown() {
  safe_teardown
}

@test "qcow2 image partition: mount partition 1 (@s1), verify file I/O, unmount" {
  do_mount "${BATS_FILE_TMPDIR}/test-partitioned.qcow2@s1"

  local mount_point
  mount_point="$(get_mount_point "$QCOW_PART1_LABEL")"
  assert_file_roundtrip "$mount_point"

  do_unmount
}

@test "qcow2 image partition: mount partition 2 (@s2), verify file I/O, unmount" {
  do_mount "${BATS_FILE_TMPDIR}/test-partitioned.qcow2@s2"

  local mount_point
  mount_point="$(get_mount_point "$QCOW_PART2_LABEL")"
  assert_file_roundtrip "$mount_point"

  do_unmount
}

@test "qcow2 image partition: missing partition fails inside the VM" {
  run do_mount "${BATS_FILE_TMPDIR}/test-partitioned.qcow2@s99"

  [[ "$output" =~ /dev/vda99 ]]
  [[ "$output" =~ failed || "$output" =~ "Can't lookup blockdev" ]]
}

@test "qcow2 list: partitioned image shows partitions via guest inspection" {
  run "$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/test-partitioned.qcow2"

  [ "$status" -eq 0 ]
  assert_list_section "$output" "<TMP>/test-partitioned.qcow2 (disk image):" "$(cat <<'EOF'
<TMP>/test-partitioned.qcow2 (disk image):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:      GUID_partition_scheme                        +<SIZE>     test-partitioned.qcow2
   1:                       ext4 qcowpart1               <SIZE>     test-partitioned.qcow2@s1
   2:                       ext4 qcowpart2               <SIZE>     test-partitioned.qcow2@s2
EOF
)"
}

@test "qcow2 list: whole-disk filesystem shows as image row" {
  run "$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/test-whole.qcow2"

  [ "$status" -eq 0 ]
  assert_list_section "$output" "<TMP>/test-whole.qcow2 (disk image):" "$(cat <<'EOF'
<TMP>/test-whole.qcow2 (disk image):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:                       ext4 qcowwhole              +<SIZE>     test-whole.qcow2
EOF
)"
}

@test "qcow2 list: LVM PV produces logical volume entries" {
  run "$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/test-lvm.qcow2"

  [ "$status" -eq 0 ]
  assert_list_section "$output" "lvm:${QCOW_LVM_VG} (volume group):" "$(cat <<EOF
lvm:${QCOW_LVM_VG} (volume group):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:                LVM2_scheme                        +<SIZE>     ${QCOW_LVM_VG}
                                 Physical Store test-lvm.qcow2
   1:                       ext4 qcowlvm                 <SIZE>     ${QCOW_LVM_VG}:test-lvm.qcow2:${QCOW_LVM_LV}
EOF
)"
}

@test "qcow2 list: inspection failure does not hide unrelated raw image" {
  run "$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/fallback.img" "${BATS_FILE_TMPDIR}/invalid.qcow2"

  [ "$status" -eq 0 ]
  echo "$output" | grep -F "${BATS_FILE_TMPDIR}/fallback.img (disk image):"
  echo "$output" | grep -F "$RAW_FALLBACK_LABEL"
}
