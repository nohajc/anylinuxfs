#!/usr/bin/env bats
# 20-subcommands.bats — test list and status subcommands
#
# Tests:
#   1. 'anylinuxfs status' when no drives are mounted
#   2. 'anylinuxfs list' output format and accuracy (using hdiutil attach)
#   3. 'anylinuxfs list --linux' filter
#   4. 'anylinuxfs list <DISK>' filter by disk/image identifier

load 'test_helper/common'

LABEL="alfs20list1"
LABEL2="alfs20list2"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/list_test.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/list_test.img" \
    "mkfs.ext4 -L ${LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  # Second image for identifier filter test.
  create_sparse_image "${BATS_FILE_TMPDIR}/list_test2.img" 256M
  vm_exec "${BATS_FILE_TMPDIR}/list_test2.img" \
    "mkfs.ext4 -L ${LABEL2} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  if [[ -n "${ATTACH_DEV:-}" ]]; then
    detach_image "$ATTACH_DEV"
    ATTACH_DEV=""
  fi
  if [[ -n "${ATTACH_DEV2:-}" ]]; then
    detach_image "$ATTACH_DEV2"
    ATTACH_DEV2=""
  fi
}

# ---------------------------------------------------------------------------

@test "subcommands: status when empty" {
  # This file is serial-only; ensure nothing is mounted before asserting the
  # global status output is empty.
  do_unmount_all

  run "$ANYLINUXFS" status
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "subcommands: list identifies filesystem on attached image" {
  local img="${BATS_FILE_TMPDIR}/list_test.img"
  local dev
  dev="$(attach_image "$img")"
  ATTACH_DEV="$dev"
  export ATTACH_DEV
  record_attached_dev "$dev"

  # anylinuxfs list depends on diskutil/hdiutil for visibility
  run "$ANYLINUXFS" list
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev"
  echo "$output" | grep -F "ext4"
  echo "$output" | grep -F "$LABEL"

  detach_image "$dev"
  ATTACH_DEV=""
}

@test "subcommands: list --linux filter" {
  local img="${BATS_FILE_TMPDIR}/list_test.img"
  local dev
  dev="$(attach_image "$img")"
  ATTACH_DEV="$dev"
  export ATTACH_DEV
  record_attached_dev "$dev"

  run "$ANYLINUXFS" list --linux
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev"

  # Since it's ext4, it should NOT show up if we filter for microsoft
  run "$ANYLINUXFS" list --microsoft
  [ "$status" -eq 0 ]
  ! echo "$output" | grep -F "$dev"

  detach_image "$dev"
  ATTACH_DEV=""
}

@test "subcommands: list filters by disk identifier" {
  local img1="${BATS_FILE_TMPDIR}/list_test.img"
  local img2="${BATS_FILE_TMPDIR}/list_test2.img"
  local dev1 dev2

  dev1="$(attach_image "$img1")"
  dev2="$(attach_image "$img2")"
  ATTACH_DEV="$dev1"
  ATTACH_DEV2="$dev2"
  export ATTACH_DEV ATTACH_DEV2
  record_attached_dev "$dev1"
  record_attached_dev "$dev2"

  # list with both disks attached — should show all.
  run "$ANYLINUXFS" list
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev1"
  echo "$output" | grep -F "$dev2"
  echo "$output" | grep -F "$LABEL"
  echo "$output" | grep -F "$LABEL2"

  # list with filter for dev1 only — should exclude dev2.
  run "$ANYLINUXFS" list "$dev1"
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev1"
  echo "$output" | grep -F "$LABEL"
  ! echo "$output" | grep -F "$dev2"
  ! echo "$output" | grep -F "$LABEL2"

  # list with filter for dev2 only — should exclude dev1.
  run "$ANYLINUXFS" list "$dev2"
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev2"
  echo "$output" | grep -F "$LABEL2"
  ! echo "$output" | grep -F "$dev1"
  ! echo "$output" | grep -F "$LABEL"

  detach_image "$dev1"
  detach_image "$dev2"
  ATTACH_DEV=""
  ATTACH_DEV2=""
}
