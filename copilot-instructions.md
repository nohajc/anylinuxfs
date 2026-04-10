<!-- GSD:project-start source:PROJECT.md -->
## Project

**anylinuxfs — IPC Migration (Milestone 1)**

`anylinuxfs` is a macOS CLI utility that mounts any Linux-supported filesystem (ext4, btrfs, xfs, NTFS, exFAT, ZFS, etc.) with full read/write support. It runs a lightweight `libkrun` microVM that mounts the filesystem and exports it to the host via NFS. This milestone removes the fragile stdout tag-scraping protocol and replaces it with structured IPC events over the existing control socket.

**Core Value:** The host and vmproxy exchange structured data reliably — no stray VM output can corrupt the protocol.

### Constraints

- **Sync-only**: No `async/await` or tokio — codebase is purely synchronous; use threads + channels
- **RON serialization**: All IPC messages use RON format (already established)
- **Backward compat**: `common-utils` is shared; protocol changes must be coordinated across `anylinuxfs` and `vmproxy`
- **Cross-compiled target**: vmproxy compiles for `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd`
<!-- GSD:project-end -->

<!-- GSD:stack-start source:codebase/STACK.md -->
## Technology Stack

## Languages
- Rust (edition 2024) — `anylinuxfs/` (host CLI), `vmproxy/` (guest agent), `common-utils/` (shared library)
- Go 1.25.7 — `init-rootfs/` (Alpine OCI bootstrapping)
- Go 1.25.4 — `freebsd-bootstrap/` (FreeBSD image bootstrapping)
## Runtime
- Host: macOS (aarch64) only
- Guest targets: `aarch64-unknown-linux-musl` (vmproxy Linux), `aarch64-unknown-freebsd` (vmproxy FreeBSD)
- Cargo (Rust workspace with three member crates)
- Go modules (two independent Go modules)
- Stable Rust for host (anylinuxfs, common-utils, unit tests)
- `nightly-2026-01-25` required for vmproxy FreeBSD cross-compilation (`-Z build-std`)
- `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd` cross-compile targets required
## Frameworks & Runtime Libraries
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
- `anyhow`, `clap`, `serde`, `serde_json`, `bstr`, `rpassword`, `ipnet`, `libc` — same as above
- `reqwest` 0.12.15 (blocking, json) — HTTP client for downloading packages inside guest
- `procfs` 0.17.0 (Linux only) — reads `/proc` for filesystem/device info
- `sys-mount` 3.0.1 (Linux only) — kernel `mount(2)` syscall interface
- `vsock` 0.5.1 (Linux only) — AF_VSOCK socket support for host↔guest control channel
- `anyhow`, `bstr`, `clap`, `libc`, `serde`, `ron` — shared subset of above
- `percent-encoding` 2.3.1 — URL percent encoding for mount paths
- `wait-timeout` 0.2.1 — child process wait with timeout
- `github.com/BurntSushi/toml` v1.5.0 — TOML config reading
- `go.podman.io/image/v5` v5.39.1 — OCI/Docker container image pulling (Alpine `alpine:latest`)
- `github.com/opencontainers/umoci` v0.4.7 — OCI image unpacking to rootfs
- `github.com/opencontainers/runtime-spec` v1.3.0 — OCI runtime spec types
- `github.com/kdomanski/iso9660` (fork: `github.com/nohajc/iso9660`) — ISO9660 image creation
- `github.com/opencontainers/umoci` v0.4.7 — OCI image unpacking
- `github.com/opencontainers/image-spec` v1.1.1 — OCI image spec types
- `golang.org/x/sys` v0.38.0 — low-level syscall access
- `github.com/apex/log` v1.4.0 — structured logging
## FFI Bindings & System Frameworks
- Bound via `anylinuxfs/src/bindings.rs`: `krun_create_ctx`, `krun_start_enter`, `krun_add_disk`, `krun_set_vm_config`, `krun_add_vsock_port2`, `krun_add_net_unixgram`, etc.
- Used for launching the lightweight microVM
- Bound via `anylinuxfs/src/rpcbind.rs`: `rpcb_set`, `rpcb_unset`, `clnt_create_timeout`
- Used to register NFS services with macOS rpcbind
- Safe Rust bindings to macOS `CoreFoundation`
- Used in `anylinuxfs/src/diskutil.rs` and `anylinuxfs/src/netutil.rs`
- Safe Rust bindings to macOS `DiskArbitration` framework
- Used in `anylinuxfs/src/diskutil.rs` for disk appearance/disappearance callbacks
- Safe Rust bindings to macOS `SystemConfiguration`
- Used in `anylinuxfs/src/netutil.rs` via `SCDynamicStore` to read DNS server configuration
- Bound via `libblkid-rs` 0.4.1
- Used in `anylinuxfs/src/devinfo.rs` to probe device UUID, label, filesystem type
- Requires `PKG_CONFIG_PATH=/opt/homebrew/opt/util-linux/lib/pkgconfig`
- Direct POSIX syscall access (`getfsstat`, `kill`, `SIGTERM`, uid/gid)
## Build System
- `build-app.sh` — orchestrates full build: anylinuxfs → vmproxy (musl) → init-rootfs → freebsd-bootstrap → vmproxy (FreeBSD)
- `run-rust-tests.sh` — runs unit tests for all three Rust crates on host architecture
- `anylinuxfs/build.rs` — emits link search paths for `libkrun` and macOS private frameworks
- `bin/anylinuxfs` — main host binary (ad-hoc signed with `codesign`)
- `libexec/vmproxy` — Linux musl guest binary
- `libexec/vmproxy-bsd` — FreeBSD guest binary
- `libexec/init-rootfs` — Alpine rootfs bootstrapper (ad-hoc signed)
- `libexec/freebsd-bootstrap` — FreeBSD image bootstrapper (static, cross-compiled)
- `libexec/gvproxy` — network helper (pre-built, from containers/gvproxy)
- `libexec/vmnet-helper` — macOS vmnet helper (pre-built)
- FreeBSD sysroot pulled at build time from `ftp.cz.freebsd.org` (`base.txz`) if not present
- vmproxy FreeBSD build requires `cargo +nightly-2026-01-25 build -Z build-std`
## Configuration
- `etc/anylinuxfs.toml` — default config (VM images, custom actions, network helper selection)
- `~/.anylinuxfs/anylinuxfs.toml` — user override config
- Config schema defined in `anylinuxfs/src/settings.rs`: `Config`, `MountConfig`, `Preferences`
- `ALFS_PASSPHRASE` (or `ALFS_PASSPHRASE1`, `ALFS_PASSPHRASE2`, ...) — LUKS passphrases
- `SUDO_UID`, `SUDO_GID` — original invoker identity captured at startup
- `PKG_CONFIG_PATH` — must point to Homebrew `util-linux` for `libblkid`
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
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

## Error Handling
- Inline test assertions inside `#[cfg(test)]` modules.
- Provably-unreachable branches (e.g. iterating over a just-constructed Vec where `next()` can never return `None`).
- Mutex lock results on infallible in-process mutexes (`.lock().unwrap()` when poison is impossible).
## Concurrency Model
- `std::thread::spawn` for background work (event loops, output readers, control socket listeners).
- `std::sync::mpsc` channels for one-direction message passing between threads.
- `std::sync::{Arc, Mutex}` for shared mutable state.
- `signal_hook` for OS signal handling (in `anylinuxfs/src/utils.rs`).
## String Types
## RAII Cleanup — `Deferred`
## Type-State for File Descriptors — `ForkOutput<O, I, C>`
- `O` — output fd type: `PtyFd` (PTY master) or `PipeOutFds` (stdout/stderr pipes) or `()`
- `I` — input fd type: `PipeInFd` or `()`
- `C` — control fd type: `CommFd` or `()`
## Privilege Handling
## Feature Gates
#[cfg(feature = "freebsd")]
#[cfg(target_os = "linux")]          // vmproxy vsock / procfs paths
#[cfg(target_os = "macos")]          // DiskArbitration / CoreFoundation
#[cfg(any(target_os = "freebsd", target_os = "macos"))]  // BSD shared paths
## Naming Patterns
## Import Organization
## Logging
- `host_println!(...)` — prefixed output to the macOS host console.
- `host_eprintln!(...)` — prefixed error output.
- `prefix_println!(prefix, ...)` — output with an explicit `log::Prefix`.
## Code Formatting
## Module Visibility
## Comments
<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

## Pattern Overview
- Host process (`anylinuxfs`) never touches the raw filesystem data directly; all I/O is mediated inside the microVM
- Guest process (`vmproxy`) is a purpose-built agent compiled for the target OS (Linux musl or FreeBSD)
- All communication between host and guest flows over three distinct channels: control socket, NFS, and the host-side API Unix socket
- The system is entirely synchronous — no async runtime; concurrency via `std::thread` and `std::sync::mpsc`
- `libkrun` is used as an embedded hypervisor: the host calls `krun_start_enter` in a forked child; that child becomes the VMM process itself
## Layers
- Purpose: CLI entrypoint, config loading, disk discovery, network setup, VM launch, NFS mount orchestration, API server
- Location: `anylinuxfs/src/`
- Entry point: `anylinuxfs/src/main.rs` → `AppRunner::run()`
- Depends on: libkrun (FFI), `common-utils`, macOS frameworks (DiskArbitration, CoreFoundation, oncrpc), gvproxy/vmnet-helper helper binaries
- Produces: NFS volume mounted at `/Volumes/<label>` on the host
- Purpose: network init, disk stack setup (LUKS→RAID→LVM→FS), NFS export, control socket server, custom action hooks
- Location: `vmproxy/src/main.rs`
- Entry point: `vmproxy` binary launched inside the VM by libkrun's init shim
- Depends on: `common-utils`, Alpine/FreeBSD Linux tools (`cryptsetup`, `mdadm`, `lvm2`, `rpc.nfsd`, etc.)
- Produces: running NFS daemon + control socket listener
- Purpose: IPC wire format, control protocol message types, RAII utilities, logging macros, constants
- Location: `common-utils/src/`
- Key files: `ipc.rs` (framing/serialization), `vmctrl.rs` (message enums), `lib.rs` (Deferred, OSType, NetHelper, constants), `log.rs` (dual console+file logging)
- Used by: both `anylinuxfs` and `vmproxy`
- Purpose: Download Alpine Linux OCI image, unpack rootfs, install NFS and filesystem tools, copy vmproxy binary
- Location: `init-rootfs/main.go`
- Run at: `anylinuxfs init`, also auto-invoked on first mount (`vm_image.rs` → `alpine::init_rootfs`)
- Not the VM init process — libkrun has its own bundled `/init.krun`
- Purpose: Same as init-rootfs but for FreeBSD images; runs inside a special FreeBSD bootstrap VM
- Location: `freebsd-bootstrap/main.go`
- Feature-gated: only used when `#[cfg(feature = "freebsd")]` / `--features freebsd` is active
## Data Flow
- `RuntimeInfo` (in `api.rs`) is an `Arc<Mutex<RuntimeInfo>>` shared between the API server thread and the main mount thread
- Signal handling uses `PubSub<Signal>` (`pubsub.rs`) to broadcast to multiple listeners
## Key Abstractions
- Purpose: All resolved runtime paths, UIDs, socket paths, kernel config, and preferences loaded at startup
- Pattern: Built once in `load_config()`, passed by reference throughout
- Purpose: Disk-specific mount parameters (disk path, fstype, mount options, passphrase config, custom actions)
- Pattern: Serializable; stored inside `RuntimeInfo` to make it queryable via the API socket
- Purpose: Device metadata (path, raw-path, label, UUID, fstype, block size, DA info) obtained via libblkid
- Pattern: `DevInfo::pv()` for physical partitions; `DevInfo::lv()` for LVM logical volumes
- Purpose: Holds the libkrun context ID and network config; returned by `setup_vm()` and passed to `start_vm()`
- Pattern: Consumed by `krun_start_enter()` in the forked child
- Purpose: RAII cleanup registration — closures are run in reverse-registration order on drop
- Pattern: `let _cleanup = deferred.add(|| { ... });` in every scope that acquires resources
- Purpose: Spawns a dedicated thread to accept and dispatch `vmctrl::Request` messages; bridges the socket IO to internal mpsc channels
- Pattern: `CtrlSocketServer::new(listener)` → main calls `wait_for_quit_cmd()` or `send_report()`
- Purpose: Generic in-process publish/subscribe hub backed by `mpsc` channels
- Pattern: Used to broadcast Unix signals to multiple listeners without coupling them
## Entry Points
- Location: `anylinuxfs/src/main.rs`
- Triggers: User runs `anylinuxfs <subcommand>` from terminal
- Responsibilities: Creates `AppRunner`, calls `app.run()`, handles exit codes and error printing
- Location: `vmproxy/src/main.rs`
- Triggers: libkrun's `krun_start_enter()` launches the `vmproxy` binary as the VM workload
- Responsibilities: Parses CLI args (disk path, mount name, options), calls `init_network()`, assembles disk stack, starts NFS daemon, opens control socket
- Location: `init-rootfs/main.go`
- Triggers: `anylinuxfs init` or auto-invoked by `vm_image.go` when rootfs is missing/outdated
- Responsibilities: Downloads Alpine OCI image, unpacks rootfs, installs packages, copies vmproxy binary into rootfs
## Communication Protocols
- Wire format: `[4-byte u32 big-endian length][RON-serialized payload]`, max 1 MiB per message
- Implementation: `common-utils/src/ipc.rs` — `Handler` (server-side) and `Client` (caller-side)
- Message types: `vmctrl::Request::{Quit, SubscribeEvents}` → `vmctrl::Response::{Ack, ReportEvent(Report)}`
- Transport (Linux guest): vsock port 12700 — mapped to host Unix socket at `/tmp/anylinuxfs-<id>-vsock` via `krun_add_vsock_port2`
- Transport (FreeBSD guest): TCP port 7350 (`VM_CTRL_PORT`) on `192.168.127.2`
- Host connects: `vm_network::connect_to_vm_ctrl_socket()` in `anylinuxfs/src/vm_network.rs`
- Transport: Unix socket at `/tmp/anylinuxfs-<id>.sock`
- Wire format: same RON framing as control socket (uses same `ipc.rs`)
- Messages: `api::Request::GetConfig` → `api::Response::Config(RuntimeInfo)`
- Security: socket ownership changed to invoker's UID/GID after creation (`api.rs` lines ~60)
- Server: `api::serve_info()` spawns a thread; client: `api::UnixClient::make_request()`
- VM exports `/mnt/disk` as NFS3 share
- VM NFS IP: `192.168.127.2` (constant `VM_IP` in `common-utils/src/lib.rs`)
- Ports: 2049 (nfsd), 32767 (mountd), 32765 (statd) — forwarded from VM to host via gvproxy
- Host mount: `mount -t nfs <opts> 192.168.127.2:/mnt/disk /Volumes/<label>`
- Default NFS options (from `fsutil::NfsOptions`): `deadtimeout=45,nfc,vers=3,nolocks,port=2049,mountport=32767`
- Used by vmproxy to register port forwarding rules at startup
- Endpoint: `http://192.168.127.1/services/forwarder/expose` (VM gateway `VM_GATEWAY_IP`)
- Used to forward NFS ports (111, 2049, 32765, 32767) from guest to host interfaces
## Error Handling
- All `?` operators preceded by `.context(...)` for error message enrichment
- Child process failures wrapped in `StatusError` (from `anylinuxfs/src/utils.rs`) to capture exit codes
- VM stdout line-tagged with `<tag:value>` format; parsed by `parse_vm_tag_value()` (`main.rs`) for status signalling (e.g. `<nfs-ready:1>`)
- Guest errors printed with `GUEST_LINUX_PREFIX` / `GUEST_BSD_PREFIX` log prefix; host errors with `HOST_PREFIX`
## Cross-Cutting Concerns
<!-- GSD:architecture-end -->

<!-- GSD:skills-start source:skills/ -->
## Project Skills

No project skills found. Add skills to any of: `.github/skills/`, `.agents/skills/`, `.cursor/skills/`, or `.github/skills/` with a `SKILL.md` index file.
<!-- GSD:skills-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd-quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd-debug` for investigation and bug fixing
- `/gsd-execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->



<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd-profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
