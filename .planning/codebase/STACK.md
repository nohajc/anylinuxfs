# Technology Stack

**Analysis Date:** 2026-04-10

## Languages

**Primary:**
- Rust (edition 2024) — `anylinuxfs/` (host CLI), `vmproxy/` (guest agent), `common-utils/` (shared library)

**Secondary:**
- Go 1.25.7 — `init-rootfs/` (Alpine OCI bootstrapping)
- Go 1.25.4 — `freebsd-bootstrap/` (FreeBSD image bootstrapping)

## Runtime

**Environment:**
- Host: macOS (aarch64) only
- Guest targets: `aarch64-unknown-linux-musl` (vmproxy Linux), `aarch64-unknown-freebsd` (vmproxy FreeBSD)

**Package Manager:**
- Cargo (Rust workspace with three member crates)
- Go modules (two independent Go modules)

**Toolchains:**
- Stable Rust for host (anylinuxfs, common-utils, unit tests)
- `nightly-2026-01-25` required for vmproxy FreeBSD cross-compilation (`-Z build-std`)
- `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd` cross-compile targets required

## Frameworks & Runtime Libraries

**Core — anylinuxfs (`anylinuxfs/Cargo.toml`):**
- `clap` 4.5.35 — CLI argument parsing (`derive`, `cargo` features)
- `anyhow` 1.0.97 — error handling with context chaining
- `serde` 1.0.219 + `serde_json` 1.0.140 — serialization/deserialization
- `ron` 0.12.0 — RON format for IPC protocol messages
- `toml` 0.8.20 + `toml_edit` 0.23.7 — config file parsing and editing
- `nix` 0.29.0 — UNIX signal and user management APIs
- `rayon` 1.11.0 — parallel iteration (parallel device probing)
- `notify` 8.0.0 — filesystem event watching (log file following)
- `signal-hook` 0.3.17 — signal handling with iterator interface
- `crossterm` 0.29.0 — raw terminal input for interactive VM shell (`anylinuxfs/src/utils.rs`)
- `rpassword` 7.4.0 — secure TTY passphrase input
- `bstr` 1.12.0 — byte strings for paths/env vars with non-UTF-8 content
- `ipnet` 2.11.0 — IP network address types and CIDR arithmetic
- `rand` 0.9.2 — random values (MAC address generation, socket name randomness)
- `nanoid` 0.4.0 — short random ID generation for socket paths
- `derive_more` 2.0.1 — derive `AddAssign`, `Deref`, `DerefMut`
- `indexmap` 2.9.0 — ordered map for disk listing display
- `regex` 1.11.1 — disk identifier pattern matching
- `url` 2.5.4 — URL parsing for image sources
- `plist` 1.7.1 — Apple property list parsing (`anylinuxfs/src/diskutil.rs`)
- `serde_with` 3.16.1 — serde field-level transformations
- `homedir` 0.3.4 — home directory detection
- `if-addrs` 0.13.4 — network interface enumeration
- `getifaddrs` 0.6.0 — IPv4 network interface listing
- `os_socketaddr` 0.2.5 — OS-level socket address bridging
- `os-version` 0.2.1 — macOS version detection
- `versions` 7.0.0 — semver and version string parsing
- `dns-sd` 0.1.3 (patched, fork `nohajc/rust-dns-sd`) — DNS-SD/Bonjour service discovery

**Core — vmproxy (`vmproxy/Cargo.toml`):**
- `anyhow`, `clap`, `serde`, `serde_json`, `bstr`, `rpassword`, `ipnet`, `libc` — same as above
- `reqwest` 0.12.15 (blocking, json) — HTTP client for downloading packages inside guest
- `procfs` 0.17.0 (Linux only) — reads `/proc` for filesystem/device info
- `sys-mount` 3.0.1 (Linux only) — kernel `mount(2)` syscall interface
- `vsock` 0.5.1 (Linux only) — AF_VSOCK socket support for host↔guest control channel

**Common — common-utils (`common-utils/Cargo.toml`):**
- `anyhow`, `bstr`, `clap`, `libc`, `serde`, `ron` — shared subset of above
- `percent-encoding` 2.3.1 — URL percent encoding for mount paths
- `wait-timeout` 0.2.1 — child process wait with timeout

**Go — init-rootfs (`init-rootfs/go.mod`):**
- `github.com/BurntSushi/toml` v1.5.0 — TOML config reading
- `go.podman.io/image/v5` v5.39.1 — OCI/Docker container image pulling (Alpine `alpine:latest`)
- `github.com/opencontainers/umoci` v0.4.7 — OCI image unpacking to rootfs
- `github.com/opencontainers/runtime-spec` v1.3.0 — OCI runtime spec types

**Go — freebsd-bootstrap (`freebsd-bootstrap/go.mod`):**
- `github.com/kdomanski/iso9660` (fork: `github.com/nohajc/iso9660`) — ISO9660 image creation
- `github.com/opencontainers/umoci` v0.4.7 — OCI image unpacking
- `github.com/opencontainers/image-spec` v1.1.1 — OCI image spec types
- `golang.org/x/sys` v0.38.0 — low-level syscall access
- `github.com/apex/log` v1.4.0 — structured logging

## FFI Bindings & System Frameworks

**libkrun** (dynamic, `/opt/homebrew/opt/libkrun/lib`):
- Bound via `anylinuxfs/src/bindings.rs`: `krun_create_ctx`, `krun_start_enter`, `krun_add_disk`, `krun_set_vm_config`, `krun_add_vsock_port2`, `krun_add_net_unixgram`, etc.
- Used for launching the lightweight microVM

**oncrpc** (macOS private framework, `/System/Library/PrivateFrameworks`):
- Bound via `anylinuxfs/src/rpcbind.rs`: `rpcb_set`, `rpcb_unset`, `clnt_create_timeout`
- Used to register NFS services with macOS rpcbind

**objc2-core-foundation** 0.3.1:
- Safe Rust bindings to macOS `CoreFoundation`
- Used in `anylinuxfs/src/diskutil.rs` and `anylinuxfs/src/netutil.rs`

**objc2-disk-arbitration** 0.3.1:
- Safe Rust bindings to macOS `DiskArbitration` framework
- Used in `anylinuxfs/src/diskutil.rs` for disk appearance/disappearance callbacks

**objc2-system-configuration** 0.3.1:
- Safe Rust bindings to macOS `SystemConfiguration`
- Used in `anylinuxfs/src/netutil.rs` via `SCDynamicStore` to read DNS server configuration

**libblkid** (from Homebrew `util-linux`):
- Bound via `libblkid-rs` 0.4.1
- Used in `anylinuxfs/src/devinfo.rs` to probe device UUID, label, filesystem type

**libc** 0.2.177:
- Direct POSIX syscall access (`getfsstat`, `kill`, `SIGTERM`, uid/gid)

## Build System

**Build scripts:**
- `build-app.sh` — orchestrates full build: anylinuxfs → vmproxy (musl) → init-rootfs → freebsd-bootstrap → vmproxy (FreeBSD)
- `run-rust-tests.sh` — runs unit tests for all three Rust crates on host architecture
- `anylinuxfs/build.rs` — emits link search paths for `libkrun` and macOS private frameworks

**Build artifacts:**
- `bin/anylinuxfs` — main host binary (ad-hoc signed with `codesign`)
- `libexec/vmproxy` — Linux musl guest binary
- `libexec/vmproxy-bsd` — FreeBSD guest binary
- `libexec/init-rootfs` — Alpine rootfs bootstrapper (ad-hoc signed)
- `libexec/freebsd-bootstrap` — FreeBSD image bootstrapper (static, cross-compiled)
- `libexec/gvproxy` — network helper (pre-built, from containers/gvproxy)
- `libexec/vmnet-helper` — macOS vmnet helper (pre-built)

**Cross-compilation notes:**
- FreeBSD sysroot pulled at build time from `ftp.cz.freebsd.org` (`base.txz`) if not present
- vmproxy FreeBSD build requires `cargo +nightly-2026-01-25 build -Z build-std`

## Configuration

**Runtime config:**
- `etc/anylinuxfs.toml` — default config (VM images, custom actions, network helper selection)
- `~/.anylinuxfs/anylinuxfs.toml` — user override config
- Config schema defined in `anylinuxfs/src/settings.rs`: `Config`, `MountConfig`, `Preferences`

**Key environment variables:**
- `ALFS_PASSPHRASE` (or `ALFS_PASSPHRASE1`, `ALFS_PASSPHRASE2`, ...) — LUKS passphrases
- `SUDO_UID`, `SUDO_GID` — original invoker identity captured at startup

**macOS entitlement:**
- `com.apple.security.hypervisor` — required for libkrun microVM execution
- `com.apple.security.cs.disable-library-validation` — required for libkrun dynamic linking

## Feature Flags

- `freebsd` (default) — enables `anylinuxfs image` subcommands, FreeBSD image management, and ZFS support via FreeBSD guest
- All three Rust crates share the same `freebsd` feature gate

## Prerequisite Homebrew Packages

- `util-linux` — provides `libblkid`
- `libkrun` — microVM library
- `lld`, `llvm` — linker for cross-compilation
- `pkgconf` — pkg-config for build

---

*Stack analysis: 2026-04-10*
