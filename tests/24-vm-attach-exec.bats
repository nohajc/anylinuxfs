#!/usr/bin/env bats
# 24-vm-attach-exec.bats — interactive access to a running anylinuxfs VM
#
# Tests:
#   1. `vm attach` opens a usable interactive shell
#   2. `vm exec` transfers arguments and reports remote exit status
#   3. `vm attach` and `vm exec` work with the FreeBSD guest selected by UFS
#   4. Instance selection works for one VM and requires a target for many

load 'test_helper/common'

LABEL1="alfs24vm1"
LABEL2="alfs24vm2"
UFS_LABEL="alfs24ufs"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/vm1.img" 512M
  create_sparse_image "${BATS_FILE_TMPDIR}/vm2.img" 512M

  vm_exec "${BATS_FILE_TMPDIR}/vm1.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL1} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"
  vm_exec "${BATS_FILE_TMPDIR}/vm2.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL2} /dev/vda \$(( \$(blockdev --getsz /dev/vda) / 8 - 16 ))"

  # UFS mounts run in the FreeBSD guest, so this fixture covers the second
  # telnet-session implementation as well as the Alpine Linux cases above.
  create_sparse_image "${BATS_FILE_TMPDIR}/ufs.img" 512M
  vm_exec_freebsd "${BATS_FILE_TMPDIR}/ufs.img" \
    "newfs -L ${UFS_LABEL} /dev/vtbd1 && mount /dev/vtbd1 /mnt && chown $(id -u):$(id -g) /mnt && umount /mnt"
}

teardown() {
  safe_teardown \
    "${BATS_FILE_TMPDIR}/vm1.img" \
    "${BATS_FILE_TMPDIR}/vm2.img" \
    "${BATS_FILE_TMPDIR}/ufs.img"
}

# ---------------------------------------------------------------------------

@test "vm attach: opens a shell in the selected running VM" {
  local img="${BATS_FILE_TMPDIR}/vm1.img"
  do_mount "$img"
  local target
  target="$(mounted_path_for "$img" "$LABEL1")"

  run env ANYLINUXFS="$ANYLINUXFS" VM_TARGET="$target" expect -c '
    set timeout 30
    spawn -noecho $env(ANYLINUXFS) vm attach $env(VM_TARGET)
    expect {
      -re {[#$] $} { send -- "printf '\''vm-attach-marker\\n'\''; exit\r" }
      timeout { exit 124 }
      eof { exit 125 }
    }
    set marker_seen 0
    expect {
      -re {vm-attach-marker} { set marker_seen 1; exp_continue }
      timeout { exit 124 }
      eof { if {!$marker_seen} { exit 125 } }
    }
    lassign [wait] _ _ _ exit_status
    exit $exit_status
  '
  [ "$status" -eq 0 ]
  [[ "$output" == *"vm-attach-marker"* ]]
}

@test "vm exec: uses the only running VM and preserves command arguments" {
  local img="${BATS_FILE_TMPDIR}/vm1.img"
  do_mount "$img"

  run env ANYLINUXFS="$ANYLINUXFS" expect -c '
    set timeout 30
    spawn -noecho $env(ANYLINUXFS) vm exec -- sh -c {printf "vm-exec-arg=<%s>\\n" "$1"} sh {argument with spaces}
    set marker_seen 0
    expect {
      -re {vm-exec-arg=<argument with spaces>} { set marker_seen 1; exp_continue }
      timeout { exit 124 }
      eof { if {!$marker_seen} { exit 125 } }
    }
    lassign [wait] _ _ _ exit_status
    exit $exit_status
  '
  [ "$status" -eq 0 ]
  [[ "$output" == *"vm-exec-arg=<argument with spaces>"* ]]
}

@test "vm exec: selected VM propagates the remote command exit status" {
  local img="${BATS_FILE_TMPDIR}/vm1.img"
  do_mount "$img"
  local target
  target="$(mounted_path_for "$img" "$LABEL1")"

  run env ANYLINUXFS="$ANYLINUXFS" VM_TARGET="$target" expect -c '
    set timeout 30
    spawn -noecho $env(ANYLINUXFS) vm exec $env(VM_TARGET) -- sh -c {printf "vm-exec-before-exit\\n"; exit 23}
    set marker_seen 0
    expect {
      -re {vm-exec-before-exit} { set marker_seen 1; exp_continue }
      timeout { exit 124 }
      eof { if {!$marker_seen} { exit 125 } }
    }
    lassign [wait] _ _ _ exit_status
    exit $exit_status
  '
  [ "$status" -eq 23 ]
  [[ "$output" == *"vm-exec-before-exit"* ]]
}

@test "vm attach and exec: work in the FreeBSD guest selected by UFS" {
  local img="${BATS_FILE_TMPDIR}/ufs.img"
  do_mount "$img"
  local target
  target="$(mounted_path_for "$img" "$UFS_LABEL")"

  run env ANYLINUXFS="$ANYLINUXFS" VM_TARGET="$target" expect -c '
    set timeout 30
    spawn -noecho $env(ANYLINUXFS) vm exec $env(VM_TARGET) -- /bin/sh -c {printf "freebsd-vm-exec-marker\\n"}
    set marker_seen 0
    expect {
      -re {freebsd-vm-exec-marker} { set marker_seen 1; exp_continue }
      timeout { exit 124 }
      eof { if {!$marker_seen} { exit 125 } }
    }
    lassign [wait] _ _ _ exit_status
    exit $exit_status
  '
  [ "$status" -eq 0 ]
  [[ "$output" == *"freebsd-vm-exec-marker"* ]]

  run env ANYLINUXFS="$ANYLINUXFS" VM_TARGET="$target" expect -c '
    set timeout 30
    spawn -noecho $env(ANYLINUXFS) vm attach $env(VM_TARGET)
    expect {
      -re {[#$] $} { send -- "printf '\''freebsd-vm-attach-marker\\n'\''; exit\r" }
      timeout { exit 124 }
      eof { exit 125 }
    }
    set marker_seen 0
    expect {
      -re {freebsd-vm-attach-marker} { set marker_seen 1; exp_continue }
      timeout { exit 124 }
      eof { if {!$marker_seen} { exit 125 } }
    }
    lassign [wait] _ _ _ exit_status
    exit $exit_status
  '
  [ "$status" -eq 0 ]
  [[ "$output" == *"freebsd-vm-attach-marker"* ]]
}

@test "vm commands: require a target when multiple VMs are running" {
  do_mount "${BATS_FILE_TMPDIR}/vm1.img"
  do_mount "${BATS_FILE_TMPDIR}/vm2.img"

  run "$ANYLINUXFS" vm attach
  [ "$status" -ne 0 ]
  [[ "$output" == *"Multiple anylinuxfs instances are running"* ]]

  run "$ANYLINUXFS" vm exec -- true
  [ "$status" -ne 0 ]
  [[ "$output" == *"Multiple anylinuxfs instances are running"* ]]
}
