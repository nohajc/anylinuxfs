# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Quick Start

### Build commands

```bash
# Full build (debug, all components: anylinuxfs, vmproxy, init-rootfs, freebsd-bootstrap)
./build-app.sh

# Release build
./build-app.sh --release

# Just type-check a Rust component (faster)
cargo check -F freebsd              # anylinuxfs
cd vmproxy && cargo check -F freebsd
```

### Test commands

```bash
# Rust unit tests (all crates, including cross-compiled vmproxy)
./run-rust-tests.sh

# Individual BATS integration test file (end-to-end, includes ext4, btrfs, NTFS, LUKS, LVM, RAID, ZFS)
bats tests/<file>.bats

# Full integration test suite
./tests/run-tests.sh
```

**Integration test prerequisites**: `brew install bats-core`, project built, Alpine rootfs initialized (`anylinuxfs init`).

### Format before committing

```bash
cargo fmt
```

## Architecture Overview

This is a macOS utility that mounts Linux filesystems (ext4, btrfs, xfs, NTFS, exFAT, ZFS) on macOS with full read-write support using a `libkrun` microVM and NFS.

### Five components working together:

1. **anylinuxfs** (Rust, macOS host) — Main CLI. Manages microVM lifecycle, disk arbitration, networking, and NFS mounts.
2. **vmproxy** (Rust, Linux/FreeBSD guest) — Agent inside the microVM. Mounts filesystems, exports via NFS. Built for `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd`.
3. **common-utils** (Rust, shared) — IPC protocol, logging, RAII utilities, shared constants.
4. **init-rootfs** (Go, Linux) — Bootstraps Alpine Linux rootfs for the microVM (downloads OCI image, unpacks).
5. **freebsd-bootstrap** (Go, FreeBSD) — Same for FreeBSD images.

### Two-process model

The host (`anylinuxfs`) orchestrates; the guest (`vmproxy`) executes. Host and guest communicate via:
- **Control socket**: TCP on macOS (port 7350) carrying `Request` / `Response` messages in RON format.
- **NFS**: Standard NFS3/NFS4 on fixed `192.168.127.x/30` subnet.
- **API socket**: Unix socket at `/tmp/anylinuxfs-<id>.sock` exposing `RuntimeInfo`.

## Code Conventions

**Error handling**: Use `anyhow::Result<T>` throughout. Always chain `.context("...")` before `?` to preserve error context. Never use `.unwrap()` or `.expect()` in production paths.

**No async**: Purely synchronous Rust. Do not introduce `tokio`, `async_std`, or `async/await`. Use `std::thread::spawn` and `std::sync::mpsc` for concurrency.

**String types**: Use `bstr::BString` and `ByteSlice` for data that may not be valid UTF-8 but needs string manipulation (environment variables, disk names, etc.).

**RAII cleanup**: Use `Deferred::add(|| { ... })` from `common-utils` for cleanup on scope exit, even on error.

**Privilege handling**: Always capture `SUDO_UID` / `SUDO_GID` at startup. Change Unix socket ownership to invoking user's UID/GID after creation.

**Formatting**: Run `cargo fmt` before committing. Do not commit unformatted Rust code.

## Building Individual Components

**anylinuxfs** (macOS CLI):
```bash
cd anylinuxfs && cargo build -F freebsd
```
Requires `libblkid` (from `util-linux` Homebrew package).

**vmproxy** (guest binary):
```bash
# Linux (musl)
cd vmproxy && cargo build --target aarch64-unknown-linux-musl -F freebsd

# FreeBSD
cd vmproxy && cargo +nightly-2026-01-25 build -Z build-std --target aarch64-unknown-freebsd -F freebsd
```

**init-rootfs** (Go):
```bash
cd init-rootfs && go build -ldflags="-w -s" -tags containers_image_openpgp -o ../libexec/
```

**freebsd-bootstrap** (Go, cross-compile):
```bash
cd freebsd-bootstrap && CGO_ENABLED=0 GOOS=freebsd GOARCH=arm64 go build -tags netgo -ldflags '-extldflags "-static" -w -s' -o ../libexec/
```

## Module Responsibilities (anylinuxfs/src/)

| Module | Responsibility |
|--------|----------------|
| `main.rs` | CLI parsing (clap), lifecycle orchestration, AppRunner |
| `api.rs` | Unix socket RPC server for RuntimeInfo |
| `settings.rs` | Config file loading (Config, MountConfig, Preferences) |
| `diskutil.rs` | macOS DiskArbitration bindings for disk discovery |
| `fsutil.rs` | Mount table queries, NFS mount option building |
| `vm_network.rs` | gvproxy / vmnet-helper startup and port forwarding |
| `vm_image.rs` | Alpine and FreeBSD image initialization |
| `pubsub.rs` | Event hub for signal broadcasting |
| `netutil.rs` | IP allocation, DNS resolution |
| `devinfo.rs` | Device metadata (UUID, label, fs type) via libblkid |
| `rpcbind.rs` | RPC service registration via macOS oncrpc framework |
| `bindings.rs` | FFI bindings to libkrun C API |
| `utils.rs` | Process management, PTY, signal handling, ForkOutput, StatusError |
| `cmd_mount.rs` | Mount command implementation |
| `vm.rs` | Core microVM lifecycle |

## Testing

**After writing a BATS test**: Always run it to verify. Run individual files directly with `bats tests/<file>.bats`.

**After implementing a feature**: Offer to write a test. If the user declines, offer to run the existing suite. If adding tests, first check whether the change is already covered by existing tests (which may need updating).

**ZFS test note**: ZFS always creates a partition table on the target device. Use `hdiutil attach` to mount images as virtual disks, then use the resulting partition device (`${HDIUTIL_DEV}s1`) for mounting. Remember to `hdiutil detach` in `teardown`. Export any variables that must be visible across test subshells.

## Key Files and Directories

- `anylinuxfs/src/` — Core macOS CLI implementation
- `vmproxy/src/` — MicroVM agent
- `common-utils/src/` — Shared IPC, logging, RAII utilities
- `tests/` — BATS integration tests
- `etc/anylinuxfs.toml` — Default configuration
- `libexec/` — Bundled helper binaries (gvproxy, vmproxy, init-rootfs, etc.)
- `kernel-builder/` — **Unmaintained.** Do not modify.

## Known Context

See [GEMINI.md](GEMINI.md) for additional architectural context, security model, platform details, and workflow rules.
