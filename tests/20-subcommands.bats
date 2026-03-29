#!/usr/bin/env bats
# 20-subcommands.bats — test list and status subcommands
#
# Tests:
#   1. 'anylinuxfs status' when no drives are mounted
#   2. 'anylinuxfs list' output format and accuracy (using hdiutil attach)
#   3. 'anylinuxfs list --linux' filter

load 'test_helper/common'

LABEL="alfs-list-test"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/list_test.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/list_test.img" \
    "mkfs.ext4 -L ${LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  if [[ -n "${HDIUTIL_DEV:-}" ]]; then
    hdiutil_detach "$HDIUTIL_DEV"
    HDIUTIL_DEV=""
  fi
}

# ---------------------------------------------------------------------------

@test "subcommands: status when empty" {
  # Ensure nothing is mounted
  do_unmount

  run "$ANYLINUXFS" status
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "subcommands: list identifies filesystem on attached image" {
  local img="${BATS_FILE_TMPDIR}/list_test.img"
  local dev
  dev="$(hdiutil_attach "$img")"

  # anylinuxfs list depends on diskutil/hdiutil for visibility
  run "$ANYLINUXFS" list
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev"
  echo "$output" | grep -F "ext4"
  echo "$output" | grep -F "$LABEL"

  hdiutil_detach "$dev"
  HDIUTIL_DEV=""
}

@test "subcommands: list --linux filter" {
  local img="${BATS_FILE_TMPDIR}/list_test.img"
  local dev
  dev="$(hdiutil_attach "$img")"

  run "$ANYLINUXFS" list --linux
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$dev"

  # Since it's ext4, it should NOT show up if we filter for microsoft
  run "$ANYLINUXFS" list --microsoft
  [ "$status" -eq 0 ]
  ! echo "$output" | grep -F "$dev"

  hdiutil_detach "$dev"
  HDIUTIL_DEV=""
}
