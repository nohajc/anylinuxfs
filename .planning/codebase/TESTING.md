# Testing Patterns

**Analysis Date:** 2026-04-10

## Test Frameworks

### Integration Tests — BATS

**Framework:** [bats-core](https://github.com/bats-core/bats-core)  
**Location:** `tests/` directory (22 test files as of this writing)  
**Prerequisite:** `brew install bats-core`, project built, Alpine rootfs initialized (`anylinuxfs init`)

**Run commands:**
```bash
# Run a single test file
bats tests/01-ext4.bats

# Run the full suite
./tests/run-tests.sh
```

**Important:** Always run individual test files with `bats tests/<file>.bats`, not `./tests/run-tests.sh`, when working on a specific feature or debugging a single scenario.

### Rust Unit Tests

**Runner:** `cargo test` (standard Rust test framework)  
**Config:** `run-rust-tests.sh` at the repo root  
**Run command:**
```bash
./run-rust-tests.sh
```

The script runs tests for all three Rust crates on the **host target** (macOS `aarch64-apple-darwin`), even for crates that are normally cross-compiled:
```bash
# From run-rust-tests.sh
(cd common-utils && cargo test -F freebsd)
(cd anylinuxfs && cargo test -F freebsd)
HOST_TARGET=$(rustc -vV | grep host: | awk '{print $2}')
(cd vmproxy && cargo test -F freebsd --target $HOST_TARGET)
```

The `freebsd` feature flag is always included so that FreeBSD-gated code paths are compiled and tested.

`vmproxy/` cross-compiles for `aarch64-unknown-linux-musl` in production but **unit tests run on the macOS host** to avoid needing a Linux environment. Platform-specific syscall code (`vsock`, `sys_mount`) is guarded with `#[cfg(target_os = "linux")]` and does not compile on macOS.

## BATS Test File Structure

Each `.bats` file follows this structure:
1. **Shebang + header comment** — filesystem type, list of test cases.
2. **`load 'test_helper/common'`** — loads all helpers from `tests/test_helper/common.bash`.
3. **Constants** — `LABEL` or other filesystem-specific strings defined at the top.
4. **`setup_file()`** — runs once per file. Creates sparse disk images, formats them via `vm_exec`.
5. **`teardown()`** — runs after every test. Calls `safe_teardown` to unmount, detach hdiutil devices, and clean up temp files.
6. **`@test "..."` blocks** — individual test cases.

```bash
#!/usr/bin/env bats
# 01-ext4.bats — ext4 filesystem mount/unmount tests

load 'test_helper/common'

LABEL="alfsext4"

setup_file() {
  create_sparse_image "${BATS_FILE_TMPDIR}/ext4.img" 512M
  vm_exec "${BATS_FILE_TMPDIR}/ext4.img" \
    "mkfs.ext4 -E root_owner=$(id -u):$(id -g) -L ${LABEL} /dev/vda ..."
}

teardown() {
  safe_teardown "${BATS_FILE_TMPDIR}/ext4.img"
}

@test "ext4: mount raw image, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/ext4.img"
  "$ANYLINUXFS" "$img" -w false

  assert_file_roundtrip "$(get_mount_point "$LABEL")"

  do_unmount
}
```

## Test Helper (`tests/test_helper/common.bash`)

All helpers are in `tests/test_helper/common.bash`, loaded with `load 'test_helper/common'`.

**Binary resolution:**
```bash
ANYLINUXFS="${ANYLINUXFS_BIN:-"${REPO_ROOT}/bin/anylinuxfs"}"
```
Override with `ANYLINUXFS_BIN=... bats tests/...` to test a different build.

**Key helpers:**

| Helper | Purpose |
|--------|---------|
| `create_sparse_image <path> <size>` | Creates a sparse disk image via `truncate` |
| `vm_exec <disk_arg> <cmd>` | Runs `cmd` inside the Alpine microVM (used for mkfs, cryptsetup, etc.) |
| `vm_exec_freebsd <disk_arg> <cmd>` | Same but uses the FreeBSD image |
| `get_mount_point <label>` | Returns `/Volumes/<label>` (root) or `~/Volumes/<label>` (user) |
| `assert_file_roundtrip <mount_point>` | Writes a unique file, reads it back, asserts equality |
| `do_unmount [disk_arg]` | Calls `anylinuxfs unmount -w [disk_arg]` |
| `hdiutil_attach <image_path>` | Attaches raw image as `/dev/diskX`, sets `HDIUTIL_DEV`, exports it |
| `hdiutil_attach_automount <image_path>` | Like above but allows macOS auto-mount (for `--remount` tests) |
| `hdiutil_detach <dev_node>` | Detaches a virtual disk |
| `safe_teardown [disk_arg]` | Orchestrates: `do_unmount → hdiutil_detach → rm -rf temp dir` |

**`vm_exec` mounts tmpfs at `/tmp`, `/run`, `/etc/lvm/archive`, `/etc/lvm/backup` before running `cmd` to satisfy Linux tools that need writable runtime directories.**

## Standard Test Pattern — File Roundtrip

Every test verifies end-to-end I/O via `assert_file_roundtrip`:
1. Writes a uniquely-named file with a unique content string.
2. Reads it back.
3. Asserts read content equals written content.
4. Removes the test file.

```bash
@test "<fs>: mount raw image, file roundtrip, unmount" {
  "$ANYLINUXFS" "$img" -w false      # mount, wait for NFS ready
  assert_file_roundtrip "$(get_mount_point "$LABEL")"
  do_unmount
}
```

## ZFS Tests (`tests/06-zfs.bats`) — Special Device Handling

ZFS always creates a partition table on the target device. ZFS test images must be:
1. Attached as a virtual disk via `hdiutil_attach` → sets `HDIUTIL_DEV`
2. Passed the **partition device** (`${HDIUTIL_DEV}s1`) to `anylinuxfs`, **not** the raw image path.
3. Detached in teardown via `safe_teardown` (which calls `hdiutil_detach` automatically when `HDIUTIL_DEV` is set).

```bash
@test "zfs: mount with FreeBSD kernel, file roundtrip, unmount" {
  local img="${BATS_FILE_TMPDIR}/zfs.img"
  hdiutil_attach "$img"
  local part_dev="${HDIUTIL_DEV}s1"     # use partition, not raw image

  "$ANYLINUXFS" "$part_dev" --zfs-os freebsd -w false
  assert_file_roundtrip "$(get_mount_point "zfs_root/$POOL")"
  do_unmount
}
```

## BATS Variable Scope Rules

Each `@test` block and `teardown` function runs in its own **subshell**. Variables set in a test are not visible to `teardown` unless they are `export`ed:

```bash
# WRONG — HDIUTIL_DEV won't be visible in teardown
hdiutil_attach "$img"

# CORRECT — hdiutil_attach already exports HDIUTIL_DEV
# (the helper does: HDIUTIL_DEV="$dev"; export HDIUTIL_DEV)
hdiutil_attach "$img"  # safe_teardown will see HDIUTIL_DEV
```

Any variable needed across test/teardown boundaries **must** be exported. The `hdiutil_attach` helper in `common.bash` handles this automatically for `HDIUTIL_DEV`.

## LUKS Tests (`tests/12-luks.bats`) — Passphrase Handling

Passphrases are passed via the `ALFS_PASSPHRASE` environment variable. Never embed passphrases in assertions or printed output.

```bash
ALFS_PASSPHRASE="$PASSPHRASE" "$ANYLINUXFS" "$img" -w false
```

For multi-disk with separate passphrases: `ALFS_PASSPHRASE1`, `ALFS_PASSPHRASE2`, etc.

## Rust Unit Test Locations

Unit tests live inside the source file they test, in a `#[cfg(test)]` module at the bottom:

```rust
// common-utils/src/ipc.rs
#[cfg(test)]
mod tests {
    #[test]
    fn test_roundtrip_handler_client() {
        let mut buf = Cursor::new(Vec::new());
        Client::write_request(&mut buf, &"hello").unwrap();
        buf.set_position(0);
        let msg: String = Handler::read_request(&mut buf).unwrap();
        assert_eq!(msg, "hello");
    }
}

// anylinuxfs/src/pubsub.rs
#[cfg(test)]
mod tests {
    #[test]
    fn basic_pub_sub() {
        let hub: PubSub<String> = PubSub::new();
        let sub1 = hub.subscribe();
        hub.publish("hello".to_string());
        assert_eq!(sub1.recv().unwrap(), "hello");
    }
}
```

**Assertions use `assert_eq!` / `assert!` / `.unwrap()` freely** — `.unwrap()` inside `#[cfg(test)]` is intentional and acceptable.

## What Is Tested

### Integration Tests (BATS)

| File | Coverage |
|------|---------|
| `tests/01-ext4.bats` | ext4 raw image, noatime, read-only, ignore-permissions |
| `tests/02-btrfs.bats` | btrfs raw image |
| `tests/03-exfat.bats` | exFAT raw image |
| `tests/04-f2fs.bats` | F2FS raw image |
| `tests/05-ntfs.bats` | NTFS raw image |
| `tests/06-zfs.bats` | ZFS (Linux + FreeBSD kernels, encrypted) via hdiutil |
| `tests/07-ufs.bats` | UFS (FreeBSD) |
| `tests/10-partitioned-disk.bats` | Partitioned disk image |
| `tests/11-lvm.bats` | LVM logical volumes |
| `tests/12-luks.bats` | LUKS, LVM-on-LUKS |
| `tests/13-hdiutil-attach.bats` | Attaching existing macOS disk images |
| `tests/14-multi-disk-btrfs.bats` | Multi-disk btrfs RAID1 |
| `tests/15-multi-instance.bats` | Multiple simultaneous mounts |
| `tests/16-freebsd-zfs-multi.bats` | Multiple ZFS pools on FreeBSD |
| `tests/17-keyfile.bats` | LUKS with keyfile authentication |
| `tests/18-image-partition.bats` | Image partition syntax (`img@sN`) |
| `tests/20-subcommands.bats` | CLI subcommand behavior |
| `tests/21-mount-options.bats` | Mount option pass-through |
| `tests/22-raid.bats` | Linux software RAID (mdadm RAID1) |

### Rust Unit Tests

| Location | What is tested |
|----------|---------------|
| `common-utils/src/ipc.rs` | IPC message framing (size validation, roundtrip encode/decode) |
| `anylinuxfs/src/pubsub.rs` | PubSub hub: subscribe, publish, drop cleanup, multi-thread, iterator |

## Disk Size Quirk (All Tests)

The microVM's mount mode exposes a virtio-blk device **64 KiB (16 × 4096-byte blocks) smaller** than the shell mode. All `mkfs` commands on raw whole-disk devices must subtract 16 blocks:

```bash
mkfs.ext4 ... /dev/vda $(( $(blockdev --getsz /dev/vda) / 8 - 16 ))
```

LVM logical volumes and LUKS inner volumes that are explicitly sized and don't reach the device boundary are not affected.

## Coverage Gaps

- No unit tests for `anylinuxfs/src/cmd_mount.rs`, `diskutil.rs`, `fsutil.rs`, `vm_network.rs` — these are integration-tested end-to-end via BATS.
- No unit tests for `vmproxy/src/main.rs` — platform-specific Linux code makes unit test isolation difficult.
- Interactive passphrase prompt test in `tests/12-luks.bats` is commented out pending `expect` investigation.

---

*Testing analysis: 2026-04-10*
