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

PART1_LABEL="imgpart1"
PART2_LABEL="imgpart2"

setup_file() {
  # Create a sparse 512 MiB GPT-partitioned image
  create_sparse_image "${BATS_FILE_TMPDIR}/test-partitioned.img" 512M

  # Partition it: GPT with two ext4 partitions
  # Partition 1: 1 MiB to 256 MiB
  # Partition 2: 256 MiB to 511 MiB
  # This keeps both partitions away from the 64 KiB that mount-mode trims
  vm_exec "${BATS_FILE_TMPDIR}/test-partitioned.img" \
    "parted -s /dev/vda mklabel gpt mkpart primary ext4 1MiB 256MiB mkpart primary ext4 256MiB 511MiB && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${PART1_LABEL} /dev/vda1 && mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${PART2_LABEL} /dev/vda2"
}

teardown() {
  safe_teardown
}

# ---------------------------------------------------------------------------

@test "image partition: list shows @s1 and @s2 identifiers" {
  output=$("$ANYLINUXFS" list "${BATS_FILE_TMPDIR}/test-partitioned.img")
  
  # Check that output contains the @s1 and @s2 identifiers
  echo "$output" | grep -q "@s1" || {
    echo "Output: $output"
    exit 1
  }
  echo "$output" | grep -q "@s2" || {
    echo "Output: $output"
    exit 1
  }
}

@test "image partition: mount partition 1 (@s1), verify mount, unmount" {
  # Mount the first partition — should succeed
  "$ANYLINUXFS" "${BATS_FILE_TMPDIR}/test-partitioned.img@s1" -w false
  
  # Check that the mount exists
  mount | grep -qE "(imgpart1|volumes/imgpart1)" || {
    echo "Mount point not found in mount output"
    mount
    exit 1
  }
  
  do_unmount
}

@test "image partition: mount partition 2 (@s2), verify mount, unmount" {
  # Mount the second partition — should succeed
  "$ANYLINUXFS" "${BATS_FILE_TMPDIR}/test-partitioned.img@s2" -w false
  
  # Check that the mount exists
  mount | grep -qE "(imgpart2|volumes/imgpart2)" || {
    echo "Mount point not found in mount output"
    mount
    exit 1
  }
  
  do_unmount
}
