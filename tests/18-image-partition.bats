#!/usr/bin/env bats
# 18-image-partition.bats — tests for mounting image partitions via @sN syntax
#
# Tests mounting unattached disk image partitions using the image@sN syntax
# (e.g., myimage.img@s1, myimage.img@s2).
#
# Setup:
#   - Create a sparse GPT-partitioned image with two ext4 partitions
#   - Label them uniquely for identification
#
# Tests:
#   1. List command shows the image with @s1 and @s2 identifiers
#   2. Mount the first partition (@s1), verify mount succeeds
#   3. Mount the second partition (@s2), verify mount succeeds

load 'test_helper/common'

PART1_LABEL="alfs18part1"
PART2_LABEL="alfs18part2"
WHOLE_LABEL="alfs18whole"

setup_file() {
  # Create a sparse 512 MiB GPT-partitioned image
  create_sparse_image "${BATS_FILE_TMPDIR}/test-partitioned.img" 512M

  # Partition it: GPT with two ext4 partitions
  # Partition 1: 1 MiB to 256 MiB
  # Partition 2: 256 MiB to 511 MiB
  # This keeps both partitions away from the 64 KiB that mount-mode trims
  vm_exec "${BATS_FILE_TMPDIR}/test-partitioned.img" \
    "parted -s /dev/vda mklabel gpt mkpart primary ext4 1MiB 256MiB mkpart primary ext4 256MiB 511MiB && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${PART1_LABEL} /dev/vda1 && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${PART2_LABEL} /dev/vda2"

  create_sparse_image "${BATS_FILE_TMPDIR}/test-whole.img" 256M
  vm_exec "${BATS_FILE_TMPDIR}/test-whole.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${WHOLE_LABEL} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
}

teardown() {
  safe_teardown \
    "${BATS_FILE_TMPDIR}/test-partitioned.img@s1" \
    "${BATS_FILE_TMPDIR}/test-partitioned.img@s2" \
    "${BATS_FILE_TMPDIR}/test-whole.img"
}

# ---------------------------------------------------------------------------

@test "image partition: list shows @s1 and @s2 identifiers" {
  run "$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/test-partitioned.img"
  [ "$status" -eq 0 ]

  assert_list_section "$output" "<TMP>/test-partitioned.img (disk image):" "$(cat <<'EOF'
<TMP>/test-partitioned.img (disk image):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:      GUID_partition_scheme                        +<SIZE>     test-partitioned.img
   1:                       ext4 alfs18part1             <SIZE>     test-partitioned.img@s1
   2:                       ext4 alfs18part2             <SIZE>     test-partitioned.img@s2
EOF
)"
}

@test "image partition: list shows whole-disk filesystem section" {
  run "$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/test-whole.img"
  [ "$status" -eq 0 ]

  assert_list_section "$output" "<TMP>/test-whole.img (disk image):" "$(cat <<'EOF'
<TMP>/test-whole.img (disk image):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:                       ext4 alfs18whole            +<SIZE>     test-whole.img
EOF
)"
}

@test "image partition: mount partition 1 (@s1), verify mount, unmount" {
  # Mount the first partition — should succeed
  local disk_id="${BATS_FILE_TMPDIR}/test-partitioned.img@s1"
  do_mount "$disk_id"
  
  # Check that the mount exists
  mount | grep -qE "(alfs18part1|volumes/alfs18part1)" || {
    echo "Mount point not found in mount output"
    mount
    exit 1
  }
  
  do_unmount "$disk_id"
}

@test "image partition: mount partition 2 (@s2), verify mount, unmount" {
  # Mount the second partition — should succeed
  local disk_id="${BATS_FILE_TMPDIR}/test-partitioned.img@s2"
  do_mount "$disk_id"
  
  # Check that the mount exists
  mount | grep -qE "(alfs18part2|volumes/alfs18part2)" || {
    echo "Mount point not found in mount output"
    mount
    exit 1
  }
  
  do_unmount "$disk_id"
}
