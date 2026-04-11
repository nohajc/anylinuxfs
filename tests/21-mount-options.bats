#!/usr/bin/env bats
# 21-mount-options.bats — test custom mount point and diskless custom action
#
# Tests:
#   1. Custom mount point: mount a disk image to a caller-specified directory
#      instead of the default ~/Volumes/<label> path.
#   2. Diskless custom action: start a VM without any disk and export the
#      Alpine VM's /etc directory via a custom action (override_nfs_export).

load 'test_helper/common'

LABEL="alfsmpopts"

# Name for the temporary custom action injected into the user config.
# Prefixed with "alfs_test_" to make its origin obvious and avoid collisions.
TEST_ACTION_NAME="alfs_test_etc_export"
USER_CONFIG="${HOME}/.anylinuxfs/config.toml"

# ---------------------------------------------------------------------------
# Setup / teardown
# ---------------------------------------------------------------------------

setup_file() {
  # Prepare an ext4 image for the custom mount point test.
  create_sparse_image "${BATS_FILE_TMPDIR}/ext4.img" 256M
  vm_exec "${BATS_FILE_TMPDIR}/ext4.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  # Persist the current user config so teardown_file can restore it exactly.
  # We use BATS_FILE_TMPDIR (survives the whole test-file run) rather than a
  # shell variable (which is lost across the setup_file / teardown_file
  # subshell boundary).
  if [[ -f "$USER_CONFIG" ]]; then
    cp "$USER_CONFIG" "${BATS_FILE_TMPDIR}/user_config_backup.toml"
  else
    mkdir -p "$(dirname "$USER_CONFIG")"
    touch "$USER_CONFIG"
    # Leave a sentinel so teardown_file knows the file was absent originally.
    touch "${BATS_FILE_TMPDIR}/user_config_was_absent"
  fi

  # Append a diskless test action that NFS-exports the Alpine VM's /etc.
  # The action itself is empty — /etc exists on the VM rootfs with no
  # additional setup required.
  cat >> "$USER_CONFIG" <<TOML

[custom_actions.${TEST_ACTION_NAME}]
description = "Test: export Alpine VM /etc without a disk (diskless custom action)"
before_mount = ""
after_mount = ""
before_unmount = ""
environment = []
capture_environment = []
override_nfs_export = "/etc"
required_os = "Linux"
TOML
}

teardown_file() {
  if [[ -f "${BATS_FILE_TMPDIR}/user_config_was_absent" ]]; then
    rm -f "$USER_CONFIG"
  elif [[ -f "${BATS_FILE_TMPDIR}/user_config_backup.toml" ]]; then
    cp "${BATS_FILE_TMPDIR}/user_config_backup.toml" "$USER_CONFIG"
  fi
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "mount-options: custom mount point" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  local custom_mp="${BATS_FILE_TMPDIR}/custom_mp"
  mkdir -p "$custom_mp"

  # Pass the custom directory as the [MOUNT_POINT] positional argument.
  # The directory must exist before calling mount.
  "$ANYLINUXFS" "$img" "$custom_mp" -w false

  # Verify the custom mount point is recorded in anylinuxfs status output.
  run "$ANYLINUXFS" status
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$custom_mp"

  # Verify the custom mount point appears in macOS mount table.
  run mount
  [ "$status" -eq 0 ]
  echo "$output" | grep -F "$custom_mp"

  assert_file_roundtrip "$custom_mp"

  do_unmount
}

@test "mount-options: diskless custom action mounts VM /etc" {
  # No disk argument — the VM boots from its own Alpine rootfs and exports
  # /etc via the NFS export override defined in the custom action.
  "$ANYLINUXFS" mount -a "$TEST_ACTION_NAME" -w false

  # Default mount point is ~/Volumes/etc (last path component of "/etc").
  local mp
  mp="$(get_mount_point "etc")"

  # Alpine Linux ships /etc/os-release; its presence confirms that the VM's
  # /etc was exported and mounted successfully.
  [[ -f "${mp}/hosts" ]]

  do_unmount
}
