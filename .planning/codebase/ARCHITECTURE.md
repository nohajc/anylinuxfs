# Architecture

**Analysis Date:** 2026-04-10

## Pattern Overview

**Overall:** Two-process model — macOS host orchestrates; Linux/FreeBSD microVM guest executes

**Key Characteristics:**
- Host process (`anylinuxfs`) never touches the raw filesystem data directly; all I/O is mediated inside the microVM
- Guest process (`vmproxy`) is a purpose-built agent compiled for the target OS (Linux musl or FreeBSD)
- All communication between host and guest flows over three distinct channels: control socket, NFS, and the host-side API Unix socket
- The system is entirely synchronous — no async runtime; concurrency via `std::thread` and `std::sync::mpsc`
- `libkrun` is used as an embedded hypervisor: the host calls `krun_start_enter` in a forked child; that child becomes the VMM process itself

## Layers

**macOS CLI Host (`anylinuxfs/src/`):**
- Purpose: CLI entrypoint, config loading, disk discovery, network setup, VM launch, NFS mount orchestration, API server
- Location: `anylinuxfs/src/`
- Entry point: `anylinuxfs/src/main.rs` → `AppRunner::run()`
- Depends on: libkrun (FFI), `common-utils`, macOS frameworks (DiskArbitration, CoreFoundation, oncrpc), gvproxy/vmnet-helper helper binaries
- Produces: NFS volume mounted at `/Volumes/<label>` on the host

**MicroVM Guest Agent (`vmproxy/src/`):**
- Purpose: network init, disk stack setup (LUKS→RAID→LVM→FS), NFS export, control socket server, custom action hooks
- Location: `vmproxy/src/main.rs`
- Entry point: `vmproxy` binary launched inside the VM by libkrun's init shim
- Depends on: `common-utils`, Alpine/FreeBSD Linux tools (`cryptsetup`, `mdadm`, `lvm2`, `rpc.nfsd`, etc.)
- Produces: running NFS daemon + control socket listener

**Shared Protocol Library (`common-utils/src/`):**
- Purpose: IPC wire format, control protocol message types, RAII utilities, logging macros, constants
- Location: `common-utils/src/`
- Key files: `ipc.rs` (framing/serialization), `vmctrl.rs` (message enums), `lib.rs` (Deferred, OSType, NetHelper, constants), `log.rs` (dual console+file logging)
- Used by: both `anylinuxfs` and `vmproxy`

**Rootfs Bootstrapper (`init-rootfs/`):**
- Purpose: Download Alpine Linux OCI image, unpack rootfs, install NFS and filesystem tools, copy vmproxy binary
- Location: `init-rootfs/main.go`
- Run at: `anylinuxfs init`, also auto-invoked on first mount (`vm_image.rs` → `alpine::init_rootfs`)
- Not the VM init process — libkrun has its own bundled `/init.krun`

**FreeBSD Image Bootstrapper (`freebsd-bootstrap/`):**
- Purpose: Same as init-rootfs but for FreeBSD images; runs inside a special FreeBSD bootstrap VM
- Location: `freebsd-bootstrap/main.go`
- Feature-gated: only used when `#[cfg(feature = "freebsd")]` / `--features freebsd` is active

## Data Flow

**Mount Flow (normal case):**

1. User runs `anylinuxfs mount /dev/disk2s1`
2. `main.rs` → `load_config()` reads `~/.anylinuxfs/config.toml` and env vars (`SUDO_UID`, `SUDO_GID`)
3. `cmd_mount.rs` → `devinfo::DevInfo::pv()` probes device via libblkid (label, UUID, fstype)
4. `vm_image.rs` → `alpine::init_rootfs()` verifies/updates Alpine rootfs at `~/.anylinuxfs/alpine/rootfs`
5. `vm_network.rs` → `start_gvproxy()` or `start_vmnet_helper()` launches network bridge process
6. `vm.rs` → `setup_vm()` calls libkrun FFI: `krun_create_ctx`, `krun_add_disk`, `krun_set_kernel`, `krun_add_vsock_port2`, `krun_start_enter` (in child)
7. Inside VM: `vmproxy` binary runs as PID 1 workload; calls `init_network()`, assembles disk stack, mounts at `/mnt/disk`
8. Inside VM: `vmproxy` writes `/etc/exports`, starts `rpc.mountd`, `rpc.nfsd`, `rpc.statd`
9. Inside VM: `vmproxy` opens control socket listener (vsock port 12700 on Linux, TCP port 7350 on FreeBSD/macOS)
10. `cmd_mount.rs` → `wait_for_nfs_server()` waits for NFS port 2049 to be reachable at `192.168.127.2`
11. `cmd_mount.rs` calls macOS `mount_nfs` with options built by `fsutil::NfsOptions`
12. `api.rs` → `serve_info()` starts Unix socket server at `/tmp/anylinuxfs-<id>.sock`
13. Main thread blocks waiting for Quit signal; on `anylinuxfs unmount`, sends `vmctrl::Request::Quit` via control socket

**Unmount Flow:**

1. `anylinuxfs unmount` connects to API socket → reads `RuntimeInfo` → finds VM control socket address
2. Sends `vmctrl::Request::Quit` → receives `vmctrl::Response::Ack`
3. Host calls `diskutil unmount` on the NFS volume
4. Host terminates gvproxy/vmnet-helper process
5. VMM child process exits

**State Management:**
- `RuntimeInfo` (in `api.rs`) is an `Arc<Mutex<RuntimeInfo>>` shared between the API server thread and the main mount thread
- Signal handling uses `PubSub<Signal>` (`pubsub.rs`) to broadcast to multiple listeners

## Key Abstractions

**`Config` (`anylinuxfs/src/settings.rs`):**
- Purpose: All resolved runtime paths, UIDs, socket paths, kernel config, and preferences loaded at startup
- Pattern: Built once in `load_config()`, passed by reference throughout

**`MountConfig` (`anylinuxfs/src/settings.rs`):**
- Purpose: Disk-specific mount parameters (disk path, fstype, mount options, passphrase config, custom actions)
- Pattern: Serializable; stored inside `RuntimeInfo` to make it queryable via the API socket

**`DevInfo` (`anylinuxfs/src/devinfo.rs`):**
- Purpose: Device metadata (path, raw-path, label, UUID, fstype, block size, DA info) obtained via libblkid
- Pattern: `DevInfo::pv()` for physical partitions; `DevInfo::lv()` for LVM logical volumes

**`VMContext` (`anylinuxfs/src/vm.rs`):**
- Purpose: Holds the libkrun context ID and network config; returned by `setup_vm()` and passed to `start_vm()`
- Pattern: Consumed by `krun_start_enter()` in the forked child

**`Deferred` (`common-utils/src/lib.rs`):**
- Purpose: RAII cleanup registration — closures are run in reverse-registration order on drop
- Pattern: `let _cleanup = deferred.add(|| { ... });` in every scope that acquires resources

**`CtrlSocketServer` (`vmproxy/src/main.rs`):**
- Purpose: Spawns a dedicated thread to accept and dispatch `vmctrl::Request` messages; bridges the socket IO to internal mpsc channels
- Pattern: `CtrlSocketServer::new(listener)` → main calls `wait_for_quit_cmd()` or `send_report()`

**`PubSub<T>` (`anylinuxfs/src/pubsub.rs`):**
- Purpose: Generic in-process publish/subscribe hub backed by `mpsc` channels
- Pattern: Used to broadcast Unix signals to multiple listeners without coupling them

## Entry Points

**`anylinuxfs/src/main.rs` — `fn main()`:**
- Location: `anylinuxfs/src/main.rs`
- Triggers: User runs `anylinuxfs <subcommand>` from terminal
- Responsibilities: Creates `AppRunner`, calls `app.run()`, handles exit codes and error printing

**`vmproxy/src/main.rs` — `fn main()`:**
- Location: `vmproxy/src/main.rs`
- Triggers: libkrun's `krun_start_enter()` launches the `vmproxy` binary as the VM workload
- Responsibilities: Parses CLI args (disk path, mount name, options), calls `init_network()`, assembles disk stack, starts NFS daemon, opens control socket

**`init-rootfs/main.go` — `func main()`:**
- Location: `init-rootfs/main.go`
- Triggers: `anylinuxfs init` or auto-invoked by `vm_image.go` when rootfs is missing/outdated
- Responsibilities: Downloads Alpine OCI image, unpacks rootfs, installs packages, copies vmproxy binary into rootfs

## Communication Protocols

**Control Socket (host ↔ vmproxy):**
- Wire format: `[4-byte u32 big-endian length][RON-serialized payload]`, max 1 MiB per message
- Implementation: `common-utils/src/ipc.rs` — `Handler` (server-side) and `Client` (caller-side)
- Message types: `vmctrl::Request::{Quit, SubscribeEvents}` → `vmctrl::Response::{Ack, ReportEvent(Report)}`
- Transport (Linux guest): vsock port 12700 — mapped to host Unix socket at `/tmp/anylinuxfs-<id>-vsock` via `krun_add_vsock_port2`
- Transport (FreeBSD guest): TCP port 7350 (`VM_CTRL_PORT`) on `192.168.127.2`
- Host connects: `vm_network::connect_to_vm_ctrl_socket()` in `anylinuxfs/src/vm_network.rs`

**API Server (host-local, caller → anylinuxfs):**
- Transport: Unix socket at `/tmp/anylinuxfs-<id>.sock`
- Wire format: same RON framing as control socket (uses same `ipc.rs`)
- Messages: `api::Request::GetConfig` → `api::Response::Config(RuntimeInfo)`
- Security: socket ownership changed to invoker's UID/GID after creation (`api.rs` lines ~60)
- Server: `api::serve_info()` spawns a thread; client: `api::UnixClient::make_request()`

**NFS (vmproxy → macOS host):**
- VM exports `/mnt/disk` as NFS3 share
- VM NFS IP: `192.168.127.2` (constant `VM_IP` in `common-utils/src/lib.rs`)
- Ports: 2049 (nfsd), 32767 (mountd), 32765 (statd) — forwarded from VM to host via gvproxy
- Host mount: `mount -t nfs <opts> 192.168.127.2:/mnt/disk /Volumes/<label>`
- Default NFS options (from `fsutil::NfsOptions`): `deadtimeout=45,nfc,vers=3,nolocks,port=2049,mountport=32767`

**gvproxy HTTP API (vmproxy → gvproxy, internal):**
- Used by vmproxy to register port forwarding rules at startup
- Endpoint: `http://192.168.127.1/services/forwarder/expose` (VM gateway `VM_GATEWAY_IP`)
- Used to forward NFS ports (111, 2049, 32765, 32767) from guest to host interfaces

## Error Handling

**Strategy:** `anyhow::Result<T>` propagated with `.context("...")` chaining throughout

**Patterns:**
- All `?` operators preceded by `.context(...)` for error message enrichment
- Child process failures wrapped in `StatusError` (from `anylinuxfs/src/utils.rs`) to capture exit codes
- VM stdout line-tagged with `<tag:value>` format; parsed by `parse_vm_tag_value()` (`main.rs`) for status signalling (e.g. `<nfs-ready:1>`)
- Guest errors printed with `GUEST_LINUX_PREFIX` / `GUEST_BSD_PREFIX` log prefix; host errors with `HOST_PREFIX`

## Cross-Cutting Concerns

**Logging:** Dual-sink via `common-utils/src/log.rs` — writes to console (when `CONSOLE_LOG_ENABLED`) and to `~/Library/Logs/anylinuxfs-<id>.log` simultaneously. Log macros: `log!`, `host_println!`, `host_eprintln!`.

**Privilege Handling:** `SUDO_UID` / `SUDO_GID` captured at startup in `load_config()`; passed to libkrun via `krun_setuid`/`krun_setgid`; used to set socket/file ownership; `drop_privileges()` / `drop_effective_privileges()` available in `main.rs`.

**Platform Guards:** `#[cfg(target_os = "linux")]` in vmproxy for vsock and procfs; `#[cfg(any(target_os = "freebsd", target_os = "macos"))]` for TCP control socket and BSD NFS paths; `#[cfg(feature = "freebsd")]` for FreeBSD image management in anylinuxfs.

**Passphrase Security:** LUKS passphrases read from `ALFS_PASSPHRASE` / `ALFS_PASSPHRASE1..N` env vars or interactive TTY (via `rpassword`); never logged. Parsed in vmproxy by `get_pwds_from_env()`.

---

*Architecture analysis: 2026-04-10*
