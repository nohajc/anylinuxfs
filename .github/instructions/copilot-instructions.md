---
applyTo: '**'
---
# anylinuxfs Project Context

`anylinuxfs` is a macOS CLI utility that mounts any Linux-supported filesystem (ext4, btrfs, xfs, NTFS, exFAT, ZFS, etc.) with full read/write support. It achieves this by running a lightweight `libkrun` microVM and exposing the mounted filesystem to the host via NFS.

## Project Architecture

Five components work together:

- **anylinuxfs** (Rust/macOS): Main CLI. Manages microVM lifecycle, disk arbitration, networking (gvproxy), and NFS mounting on the host.
- **vmproxy** (Rust/Linux/FreeBSD): Guest-side agent running inside the microVM. Mounts filesystems, exports via NFS, and handles the control socket. Compiled for `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd`.
- **common-utils** (Rust): Shared library used by both `anylinuxfs` and `vmproxy`. Contains IPC protocol, logging, RAII utilities, and shared constants.
- **init-rootfs** (Go/Linux): Bootstraps the Alpine Linux rootfs for the microVM (downloads OCI image, unpacks it). This is NOT the VM init process — `libkrun` has its own bundled init.
- **freebsd-bootstrap** (Go/FreeBSD): Same as `init-rootfs` but for FreeBSD images.

### Two-Process Model

The host orchestrates; the guest executes:

```
macOS host (anylinuxfs)
  ├── Starts network helper (gvproxy or vmnet-helper)
  ├── Launches krun microVM via libkrun FFI
  │     └── vmproxy runs inside VM
  │           ├── Configures network (192.168.127.2/30)
  │           ├── Mounts disk (LUKS → RAID → LVM → filesystem)
  │           ├── Writes /etc/exports, starts NFS daemon
  │           └── Listens on control socket
  └── Mounts NFS share: mount -t nfs 192.168.127.2:/mnt/disk /Volumes/...
```

### Communication Protocols

**Control socket** (host ↔ vmproxy):
- Format: `[4-byte u32 BE length][RON-serialized payload]`, max 1 MiB per message
- Transport: TCP on macOS/FreeBSD (`VM_CTRL_PORT = 7350`), vsock inside Linux (`CID_ANY:12700`)
- Messages: `Request::Quit`, `Request::SubscribeEvents` → `Response::Ack`, `Response::ReportEvent`

**API server** (host → caller): Unix socket at `/tmp/anylinuxfs-<id>.sock`, returns `RuntimeInfo` (mount config, PIDs, device metadata, mount point).

**NFS**: Standard NFS3/NFS4 on the fixed `192.168.127.x/30` subnet. Ports 2049 (nfsd), 32767 (mountd), 32765 (statd) forwarded via gvproxy.

### Disk Path Formats

```
/dev/disk1s2          # single partition
lvm:vg_name:lv_name   # LVM logical volume
raid:dev1:dev2:...    # software RAID (mdadm)
```

## Code Conventions

### Error Handling
- Use `anyhow::Result<T>` throughout. Always chain `.context("...")` before `?` to preserve error context.
- Never use `.unwrap()` or `.expect()` in production paths. Reserve them for tests or provably-unreachable branches.
- For child process failures, wrap with `StatusError` (from `utils.rs`) to capture exit codes.

```rust
// Correct
let content = fs::read_to_string(&path)
    .context(format!("Failed to read {}", path.display()))?;

// Avoid
let content = fs::read_to_string(&path).unwrap();
```

### No Async
The codebase is **purely synchronous**. Do not introduce `tokio`, `async_std`, or `async/await`. Use `std::thread::spawn` and `std::sync::mpsc` channels for concurrency. This is intentional — simpler error handling, no executor overhead.

### String Types
Use `bstr::BString` and `ByteSlice` for data that may or may not be valid Unicode but still needs normal string manipulation operations (searching, slicing, formatting). Typical use cases include environment variables and other byte sequences that are usually text but have no Unicode guarantee. This avoids the `String` vs `Vec<u8>` false dichotomy — you get string-like operations without requiring valid UTF-8.

### RAII Cleanup
Use `Deferred::add(|| { ... })` from `common-utils` for cleanup that must run on scope exit (even on error). This is the project's RAII pattern — prefer it over manual cleanup in `Drop` impls or error branches.

```rust
let _cleanup = Deferred::add(|| { let _ = fs::remove_file(&socket_path); });
```

### Type State for File Descriptors
`ForkOutput<O, I, C>` uses generic type parameters to track fd capabilities at compile time (`PtyFd`, `PipeOutFds`, `CommFd`, `()`). When adding new fork-based code, match this pattern rather than storing raw fd integers.

### Privilege Handling
- Always capture `SUDO_UID` / `SUDO_GID` environment variables at startup to record the original invoker.
- Change Unix socket ownership to `(invoker_uid, invoker_gid)` after creation.

### Formatting
- Run `cargo fmt` / `rustfmt` on any Rust changes before committing. Do not commit unformatted Rust code.

## Building

### `anylinuxfs` (macOS host binary)

Requires `util-linux` for `libblkid` (via Homebrew) and must be built with the `freebsd` feature:

```sh
export PKG_CONFIG_PATH="/opt/homebrew/opt/util-linux/lib/pkgconfig"
cargo build -F freebsd            # from anylinuxfs/
cargo check -F freebsd            # for type-checking only
```

The `build-app.sh` script at the repo root sets these automatically and also signs and copies the binary to `bin/`.

### `vmproxy` (guest binary — Linux/FreeBSD)

Cross-compiled (FreeBSD also needs sysroot to be built natively on macOS but this is handled by the build-app.sh script):

```sh
# Linux (musl):
cargo build --target aarch64-unknown-linux-musl -F freebsd
# FreeBSD:
cargo build --target aarch64-unknown-freebsd -F freebsd
```

### Other components

```sh
# init-rootfs
cd init-rootfs && go build -ldflags="-w -s" -tags containers_image_openpgp -o ../libexec/

# freebsd-bootstrap (cross-compile for FreeBSD)
cd freebsd-bootstrap && CGO_ENABLED=0 GOOS=freebsd GOARCH=arm64 \
    go build -tags netgo -ldflags '-extldflags "-static" -w -s' -o ../libexec/
```

## Module Responsibilities (`anylinuxfs/src/`)

| Module | Responsibility |
|--------|----------------|
| `main.rs` | CLI parsing (`clap`), lifecycle orchestration, `AppRunner` |
| `api.rs` | Unix socket RPC server exposing `RuntimeInfo` |
| `settings.rs` | Config file loading/merging (`Config`, `MountConfig`, `Preferences`) |
| `diskutil.rs` | macOS DiskArbitration bindings for disk/partition discovery |
| `fsutil.rs` | Mount table queries, NFS mount option building |
| `vm_network.rs` | gvproxy / vmnet-helper startup and port forwarding |
| `vm_image.rs` | Alpine rootfs and FreeBSD image initialization |
| `pubsub.rs` | Generic `PubSub<T>` event hub (used for signal broadcasting) |
| `netutil.rs` | IP allocation (`pick_available_network`), DNS resolution |
| `devinfo.rs` | Device metadata probing via libblkid (UUID, label, fs type) |
| `rpcbind.rs` | RPC service registration via macOS `oncrpc` framework |
| `bindings.rs` | FFI bindings to libkrun C API (`krun_create_ctx`, `krun_start_enter`, etc.) |
| `utils.rs` | Process management, PTY, signal handling, `ForkOutput`, `StatusError` |

## Security Model

- **VM isolation**: Disk I/O is mediated inside the microVM. The host only sees an NFS share.
- **Privilege dropping**: Most commands require `sudo` for `/dev/disk*` access, but the microVM runs with dropped privileges.
- **Passphrases**: Supplied via `ALFS_PASSPHRASE` env var (or `ALFS_PASSPHRASE1`, `ALFS_PASSPHRASE2`, etc. for multi-disk), or via interactive TTY prompt using `rpassword`. Never log or print passphrases.
- **Socket security**: API socket ownership is changed to the invoking user's UID/GID after creation.

## Platform & Cross-Compilation

- **Host**: macOS only — uses `DiskArbitration`, `CoreFoundation`, `objc2` bindings.
- **Guest targets**: `aarch64-unknown-linux-musl` (default), `aarch64-unknown-freebsd` (optional).
- **Feature gate**: `#[cfg(feature = "freebsd")]` enables `Image` subcommands and ZFS support.
- **Platform guards**: `#[cfg(target_os = "linux")]` in vmproxy for vsock/procfs, `#[cfg(any(target_os = "freebsd", target_os = "macos"))]` for BSD paths.

## Building

```bash
./build-app.sh            # Debug build (all components)
./build-app.sh --release  # Release build
```

The script handles cross-compilation for vmproxy and places binaries in `bin/` and `libexec/`.

**Prerequisites**: Rust toolchain with `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd` targets, Go toolchain, and Homebrew packages: `util-linux`, `libkrun`, `lld`, `llvm`, `pkgconf`.

## Testing

**Rust unit tests** (even for vmproxy which is normally cross-compiled for Linux/FreeBSD):
```bash
./run-rust-tests.sh
```

**Integration tests** (BATS, end-to-end: ext4, btrfs, NTFS, LUKS, LVM, RAID, etc.):
```bash
./tests/run-tests.sh
```

Prerequisites for integration tests: `brew install bats-core`, project built, Alpine rootfs initialized (`anylinuxfs init`). Alpine packages are installed automatically on first run.

**ZFS test note**: ZFS always creates a partition table on the target device. Because of this, ZFS test images must be attached as a virtual disk via `hdiutil_attach` and mounted using the resulting partition device (`${HDIUTIL_DEV}s1`) rather than the raw image file. Remember to `hdiutil_detach` the device in `teardown`.

**bats-core variable scope**: Each test and `teardown` runs in its own subshell. Any variable that must be visible across these contexts (e.g. device nodes captured in a `@test` and detached in `teardown`) must be `export`ed.

## Key CLI Commands

```
anylinuxfs list                   # List disks with filesystem detection
anylinuxfs mount <DISK_IDENT>     # Mount filesystem (also the default command)
anylinuxfs unmount                # Safely unmount and terminate VM
anylinuxfs status                 # Show current mount status
anylinuxfs init                   # Reinitialize microVM root filesystem
anylinuxfs shell                  # Open debug shell in VM
anylinuxfs log [-f]               # Show/follow VM log
anylinuxfs dmesg                  # Show kernel messages
anylinuxfs apk {info|add|del}     # Manage Alpine packages in VM
anylinuxfs image {list|install}   # Manage VM images (FreeBSD feature)
```

## Key Files and Directories

- `anylinuxfs/src/` — Core macOS CLI implementation
- `vmproxy/src/` — MicroVM agent implementation
- `common-utils/src/` — Shared IPC protocol, logging, RAII utilities
- `init-rootfs/` — Alpine rootfs bootstrapping (Go, runs on host)
- `freebsd-bootstrap/` — FreeBSD image bootstrapping (Go, runs from within a special bootstrap VM)
- `tests/` — BATS integration tests
- `etc/anylinuxfs.toml` — Default configuration file
- `libexec/` — Bundled helper binaries (gvproxy, vmproxy, init-rootfs, etc.)
- `kernel-builder/` — **Unmaintained.** Originally a Lua-based microVM launcher used for experiments during early development (before `anylinuxfs shell` was usable) and for building custom Linux kernels (libkrun-based config with additional filesystem drivers enabled). Superseded by `anylinuxfs shell`, which now serves the same purpose on both Linux and FreeBSD. Do not modify or depend on this directory.

# Miscellaneous
When making changes to this file, apply the same changes to GEMINI.md, except this line.

## Workflow Rules
- **New test cases**: After writing a new BATS test case, always attempt to run it and report the result. Run individual test files directly with `bats tests/<file>.bats`; use `./tests/run-tests.sh` only to run the full suite.
- **New features / non-trivial behavior changes**: After implementing, offer to write a new test case. If the user declines, offer to run the existing test suite. If the user agrees to add tests, first check whether the change is already covered by existing tests (which may need updating); then either fix the existing test or create a new one.
- **"Please remember" / "take a note"**: When the user asks you to remember something, add it as a rule or note in this instructions file (and mirror it in GEMINI.md).
- **`git add` new files**: After creating a new file, run `git add <file>` on it. When creating multiple files, batch them into a single `git add` command.
