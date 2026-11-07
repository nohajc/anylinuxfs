#!/usr/bin/env bats
# Focused macOS helper tests; no disk operations or privileges required.
# Run with: bats tests/helpers/disk-image-helpers.bats

ANYLINUXFS_BIN=/usr/bin/true
load '../test_helper/common'

setup() {
  [[ "$HOST_OS" == Darwin ]] || skip "requires macOS plutil and /dev/disk0"
  ATTACH_DEV=""
  ATTACHED_DEVS=""
  MOCK_STATUS=0
  MOCK_DIAGNOSTIC=""
  MOCK_ENTITY=disk0
  MOCK_OUTPUT=plist
  export MOCK_LOG="${BATS_TEST_TMPDIR}/calls"
}

diskutil() {
  printf '%s\n' "$*" >> "$MOCK_LOG"
  [[ "$1" == eject ]] && return 0
  [[ -z "$MOCK_DIAGNOSTIC" ]] || printf '%s\n' "$MOCK_DIAGNOSTIC" >&2
  if [[ "$MOCK_OUTPUT" == plist ]]; then
    cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>system-entities</key><array>
<dict><key>content-hint</key><string>metadata</string></dict>
<dict><key>dev-entry</key><string>${MOCK_ENTITY}s1</string></dict>
<dict><key>dev-entry</key><string>${MOCK_ENTITY}</string></dict>
</array></dict></plist>
EOF
  else
    printf '%s\n' "$MOCK_OUTPUT"
  fi
  return "$MOCK_STATUS"
}

@test "disk-image helper: noMount parses whole disk and records cleanup" {
  attach_image '/tmp/image with spaces.img' > "${BATS_TEST_TMPDIR}/device"
  [[ "$(cat "${BATS_TEST_TMPDIR}/device")" == /dev/disk0 ]]
  [[ "$ATTACH_DEV" == /dev/disk0 ]]
  [[ "$ATTACHED_DEVS" == /dev/disk0 ]]
  [[ "$(cat "$MOCK_LOG")" == 'image attach --plist --noMount /tmp/image with spaces.img' ]]
}

@test "disk-image helper: automount uses autoMount policy" {
  run attach_image_automount /tmp/image.img
  [[ "$status" -eq 0 ]]
  [[ "$output" == /dev/disk0 ]]
  [[ "$(cat "$MOCK_LOG")" == 'image attach --plist --mountPolicy autoMount /tmp/image.img' ]]
}

@test "disk-image helper: diagnostics do not contaminate device stdout" {
  MOCK_DIAGNOSTIC='diskutil: warning before plist'
  attach_image /tmp/image.img > "${BATS_TEST_TMPDIR}/device" 2> "${BATS_TEST_TMPDIR}/diagnostic"
  [[ "$(cat "${BATS_TEST_TMPDIR}/device")" == /dev/disk0 ]]
  [[ "$(cat "${BATS_TEST_TMPDIR}/diagnostic")" == "$MOCK_DIAGNOSTIC" ]]
}

@test "disk-image helper: accepts an absolute device path in the plist" {
  MOCK_ENTITY=/dev/disk0
  run attach_image /tmp/image.img
  [[ "$status" -eq 0 ]]
  [[ "$output" == /dev/disk0 ]]
}

@test "disk-image helper: preserves hard failure status and diagnostics" {
  MOCK_STATUS=17
  MOCK_OUTPUT=''
  MOCK_DIAGNOSTIC='diskutil: attach failed - image not recognized'
  run attach_image /tmp/image.img
  [[ "$status" -eq 17 ]]
  [[ "$output" == *"$MOCK_DIAGNOSTIC"* ]]
  [[ "$output" == *'(exit 17)'* ]]
  [[ "$(cat "$MOCK_LOG")" != *eject* ]]
}

@test "disk-image helper: failed attach with a device ejects it even in command substitution" {
  MOCK_STATUS=23
  run bash_failure_capture
  [[ "$status" -eq 23 ]]
  [[ "$(tail -n 1 "$MOCK_LOG")" == 'eject /dev/disk0' ]]
}

bash_failure_capture() {
  local dev
  dev="$(attach_image_automount /tmp/image.img)"
}

@test "disk-image helper: rejects malformed output and missing disk devices" {
  MOCK_OUTPUT='not a plist'
  run attach_image /tmp/image.img
  [[ "$status" -ne 0 ]]
  [[ "$output" == *'no valid whole-disk device'* ]]
  MOCK_ENTITY=disk0s1
  MOCK_OUTPUT=plist
  run attach_image /tmp/image.img
  [[ "$status" -ne 0 ]]
}

@test "disk-image helper: teardown ejects tracked devices through diskutil" {
  attach_image /tmp/image.img > /dev/null
  detach_all_attached_images
  [[ "$(tail -n 1 "$MOCK_LOG")" == 'eject /dev/disk0' ]]
  [[ -z "$ATTACHED_DEVS" && -z "$ATTACH_DEV" ]]
}
