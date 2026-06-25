#!/usr/bin/env bats
# 15-multi-instance.bats — concurrent multiple microVM instances tests
#
# Note: 'run' is a BATS built-in that populates $status and $output variables.
#
# Tests:
#   1. Mount two independent ext4 images simultaneously
#   2. Verify 'anylinuxfs status' shows both active mounts
#   3. Verify simultaneous file I/O on both mounts
#   4. Unmount one specifically and verify the other remains
#   5. Handle duplicate labels (incrementing mount points)

load 'test_helper/common'

LABEL1="alfs15multi1"
LABEL2="alfs15multi2"
LABEL_DUP="alfs15dup"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/multi1.img" 512M
  create_sparse_image "${BATS_FILE_TMPDIR}/multi2.img" 512M
  create_sparse_image "${BATS_FILE_TMPDIR}/dup1.img" 512M
  create_sparse_image "${BATS_FILE_TMPDIR}/dup2.img" 512M

  vm_exec "${BATS_FILE_TMPDIR}/multi1.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL1} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
  vm_exec "${BATS_FILE_TMPDIR}/multi2.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL2} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
  vm_exec "${BATS_FILE_TMPDIR}/dup1.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL_DUP} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
  vm_exec "${BATS_FILE_TMPDIR}/dup2.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL_DUP} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  safe_teardown \
    "${BATS_FILE_TMPDIR}/multi1.img" \
    "${BATS_FILE_TMPDIR}/multi2.img" \
    "${BATS_FILE_TMPDIR}/dup1.img" \
    "${BATS_FILE_TMPDIR}/dup2.img"
}

# ---------------------------------------------------------------------------

@test "multi-instance: mount two independent images" {
  local img1="${BATS_FILE_TMPDIR}/multi1.img"
  local img2="${BATS_FILE_TMPDIR}/multi2.img"

  do_mount "$img1"
  do_mount "$img2"

  # Verify status shows both (image and mount point on the same line)
  run "$ANYLINUXFS" status
  [ "$status" -eq 0 ]
  local mp1="$(mounted_path_for "$img1" "$LABEL1")"
  local mp2="$(mounted_path_for "$img2" "$LABEL2")"
  echo "$output" | grep -F "$img1" | grep -F "$mp1"
  echo "$output" | grep -F "$img2" | grep -F "$mp2"

  # Verify I/O on both
  assert_file_roundtrip "$mp1"
  assert_file_roundtrip "$mp2"

  # Unmount first one specifically
  do_unmount "$img1"

  # Verify second is still there
  run "$ANYLINUXFS" status
  ! echo "$output" | grep -F "$img1"
  echo "$output" | grep -F "$img2" | grep -F "$mp2"
  assert_file_roundtrip "$mp2"

  # Unmount second
  do_unmount "$img2"
}

@test "multi-instance: duplicate labels increment mount points" {
  local img1="${BATS_FILE_TMPDIR}/dup1.img"
  local img2="${BATS_FILE_TMPDIR}/dup2.img"

  do_mount "$img1"
  do_mount "$img2"

  local mp1="$(mounted_path_for "$img1" "$LABEL_DUP")"
  local mp2="$(mounted_path_for "$img2" "$LABEL_DUP")"

  [ -d "$mp1" ]
  [ -d "$mp2" ]

  assert_file_roundtrip "$mp1"
  assert_file_roundtrip "$mp2"

  do_unmount "$img1" "$img2"
}
