# External Integrations

**Analysis Date:** 2026-04-10

## MicroVM Runtime

**libkrun** (local Homebrew package, `/opt/homebrew/opt/libkrun/lib`):
- Purpose: Lightweight KVM-based microVM launch
- Integration: FFI via `anylinuxfs/src/bindings.rs` — direct C linkage (`#[link(name = "krun")]`)
- API surface: `krun_create_ctx`, `krun_start_enter`, `krun_add_disk`, `krun_set_vm_config`, `krun_set_exec`, `krun_set_kernel`, `krun_add_vsock_port2`, `krun_add_net_unixgram`, `krun_set_gvproxy_path`, `krun_setuid`, `krun_setgid`
- Invoked from: `anylinuxfs/src/main.rs` (AppRunner), orchestrated with settings from `anylinuxfs/src/settings.rs`
- Required entitlement: `com.apple.security.hypervisor`

## Network Helpers (Bundled Binaries)

**gvproxy** (`libexec/gvproxy`):
- Source: containers/gvproxy project (pre-built, bundled)
- Purpose: User-space network proxy providing NAT and port forwarding for VM NFS ports
- Forwarded ports: 2049 (nfsd), 32767 (mountd), 32765 (statd)
- Integration: Spawned as child process via `anylinuxfs/src/vm_network.rs` (`start_gvproxy`)
- Communication: Unix domain socket at `/tmp/network-<random>.sock`
- Config: `config.preferences.gvproxy_debug()` controls debug logging

**vmnet-helper** (`libexec/vmnet-helper`):
- Purpose: macOS vmnet-based networking (native macOS network virtualization)
- Integration: Spawned as child process via `anylinuxfs/src/vm_network.rs` (`start_vmnet_helper`)
- Auth: Requires `sudo` unless macOS Tahoe or later; runs with dropped privileges
- Config output: JSON with CIDR, MAC address, interface ID written to stdout; parsed via `VmnetConfigJson` in `anylinuxfs/src/vm_network.rs`
- Network selection: `anylinuxfs/src/netutil.rs` (`pick_available_network`) allocates from `172.27.1.0/12` pool (`DEFAULT_VMNET_POOL` in `anylinuxfs/src/settings.rs`)

**Network mode selection:**
- Controlled by `[network] helper = "vmnet"` in `etc/anylinuxfs.toml`
- Modes: `gvproxy` or `vmnet` — see `NetHelper` enum in `common-utils/src/lib.rs`

## macOS System Frameworks (FFI)

**DiskArbitration** (`/System/Library/Frameworks/DiskArbitration.framework`):
- Bindings: `objc2-disk-arbitration` 0.3.1 in `anylinuxfs/src/diskutil.rs`
- Purpose: Register callbacks for disk appear/disappear events, query disk properties
- API used: `DASession`, `DADisk`, `DARegisterDiskAppearedCallback`, `DARegisterDiskDisappearedCallback`

**CoreFoundation** (`/System/Library/Frameworks/CoreFoundation.framework`):
- Bindings: `objc2-core-foundation` 0.3.1
- Used in: `anylinuxfs/src/diskutil.rs`, `anylinuxfs/src/netutil.rs`, `anylinuxfs/src/utils.rs`
- Purpose: CFDictionary/CFString/CFRunLoop/CFURL manipulation for DiskArbitration and SystemConfiguration results

**SystemConfiguration** (`/System/Library/Frameworks/SystemConfiguration.framework`):
- Bindings: `objc2-system-configuration` 0.3.1 in `anylinuxfs/src/netutil.rs`
- Purpose: Read the active DNS server(s) configured on the host via `SCDynamicStore`
- Key queried: `State:/Network/Global/DNS` → `ServerAddresses`

**oncrpc** (`/System/Library/PrivateFrameworks/oncrpc.framework`):
- Bindings: Custom FFI in `anylinuxfs/src/rpcbind.rs` (`#[link(name = "oncrpc", kind = "framework")]`)
- Purpose: Register/unregister NFS-related RPC services with macOS portmapper
- API used: `rpcb_set`, `rpcb_unset`, `clnt_create_timeout`, `getrpcbynumber`
- Link path: `/System/Library/PrivateFrameworks` (added in `anylinuxfs/build.rs`)

**libblkid** (Homebrew `util-linux`):
- Bindings: `libblkid-rs` 0.4.1 in `anylinuxfs/src/devinfo.rs`
- Purpose: Probe block devices and disk images for UUID, label, filesystem type, partition type
- Requires: `PKG_CONFIG_PATH=/opt/homebrew/opt/util-linux/lib/pkgconfig` at build time

## NFS Server (Inside Guest VM)

**Linux NFS daemon** (Alpine Linux package `nfs-utils`):
- Components: `rpc.nfsd`, `mountd`, `statd` — started inside the microVM by `vmproxy`
- Exports written to `/etc/exports` inside the VM by `vmproxy/src/main.rs`
- NFS version: 3 (default; options in `anylinuxfs/src/fsutil.rs` `NfsOptions`)
- Mounted on host via `mount -t nfs 192.168.127.2:/mnt/disk /Volumes/...`
- Service registration: host-side via `anylinuxfs/src/rpcbind.rs` (macOS rpcbind)

**FreeBSD NFS daemon** (FreeBSD base system):
- Files required in guest rootfs: `/etc/rc.d/mountd`, `/etc/rc.d/nfsd`, `/etc/rc.d/statd`
- Used when `zfs_os = "FreeBSD"` in config (see `etc/anylinuxfs.toml`)

## OCI Container Registry (Alpine Linux Bootstrapping)

**Docker Hub / OCI registries:**
- Client: `go.podman.io/image/v5` — used in `init-rootfs/main.go`
- Image pulled: `alpine:latest` (configurable via `images.alpine-latest.docker_ref` in `etc/anylinuxfs.toml`)
- Auth: None (public image); DNS fallback: `1.1.1.1` (hardcoded in `init-rootfs/main.go`)
- OCI layout unpacking: `github.com/opencontainers/umoci` in `init-rootfs/main.go`

## GitHub Releases (Kernel & Guest Image Downloads)

**libkrunfw kernel images:**
- URL pattern: `https://github.com/nohajc/libkrunfw/releases/download/v6.12.62-rev1/linux-aarch64-Images-v6.12.62-anylinuxfs.tar.gz`
- URL pattern: `https://github.com/nohajc/libkrunfw/releases/download/v6.12.62-rev1/modules.squashfs`
- Configured in `etc/anylinuxfs.toml` under `[images.alpine-latest]`
- Fetched by: `anylinuxfs/src/vm_image.rs` (`fetch` function)

**FreeBSD kernel bundle:**
- URL: `https://github.com/nohajc/freebsd/releases/download/alfs%2F15.0/kernel.txz`
- Configured in `etc/anylinuxfs.toml` under `[images."freebsd-15.0"]`
- Used when building FreeBSD guest image with `anylinuxfs image install`

**NFS entrypoint script:**
- URL: `https://raw.githubusercontent.com/nohajc/docker-nfs-server/refs/heads/freebsd/entrypoint.sh`
- Fetched during rootfs initialization (`anylinuxfs/src/vm_image.rs`)

## FreeBSD Official Distribution Servers

**FreeBSD ISO and OCI images:**
- ISO URL: `https://download.freebsd.org/releases/ISO-IMAGES/15.0/FreeBSD-15.0-RELEASE-arm64-aarch64-bootonly.iso`
- OCI URL: `https://download.freebsd.org/releases/OCI-IMAGES/15.0-RELEASE/aarch64/Latest/FreeBSD-15.0-RELEASE-arm64-aarch64-container-image-runtime.txz`
- Configured in `etc/anylinuxfs.toml` under `[images."freebsd-15.0"]`
- Fetched by: `freebsd-bootstrap/main.go`

**FreeBSD sysroot (build-time cross-compilation):**
- URL: `http://ftp.cz.freebsd.org/pub/FreeBSD/releases/arm64/14.3-RELEASE/base.txz`
- Fetched by: `build-app.sh` during vmproxy FreeBSD cross-compilation if `vmproxy/freebsd-sysroot/` is absent

## Host↔Guest IPC (Control Socket)

**Protocol:**
- Format: `[4-byte u32 BE length][RON-serialized payload]`, max 1 MiB per message
- Serialization: `ron` 0.12.0 — implemented in `common-utils/src/ipc.rs`
- Messages: `Request::Quit`, `Request::SubscribeEvents` → `Response::Ack`, `Response::ReportEvent`
- Defined in: `common-utils/src/vmctrl.rs`

**Transport (host side → guest):**
- macOS/FreeBSD host: TCP on `192.168.127.2:7350` (`VM_CTRL_PORT`, `VM_IP` constants in `common-utils/src/lib.rs`)
- Linux guest internal: vsock `CID_ANY:12700` via `vsock` crate (Linux only)

## API Server (anylinuxfs Unix Socket)

**Purpose:** Expose runtime mount state to callers (e.g., `anylinuxfs status`)
- Socket path: `/tmp/anylinuxfs-<id>.sock`
- Implemented in: `anylinuxfs/src/api.rs`
- Ownership: Changed to invoker UID/GID after creation (captured from `SUDO_UID`/`SUDO_GID`)
- Protocol: Same RON-framed IPC as control socket (`Request::GetConfig` → `Response::Config(RuntimeInfo)`)
- Struct: `RuntimeInfo` — mount config, device info, PIDs, mount point, VM IP

## DNS Service Discovery (Bonjour/mDNS)

**dns-sd (patched fork):**
- Crate: `dns-sd` 0.1.3 from `https://github.com/nohajc/rust-dns-sd.git`
- Purpose: mDNS/Bonjour integration for service announcement or discovery on local network
- Used in: `anylinuxfs/src/` (imported via `Cargo.toml`)

## macOS System Commands (Runtime Integration)

The following macOS system tools are invoked as child processes at runtime:

| Tool | Location | Purpose |
|------|----------|---------|
| `diskutil` | macOS built-in | Disk/partition enumeration (plist output), called from `anylinuxfs/src/diskutil.rs` |
| `mount` / `umount` | macOS built-in | NFS mount/unmount on host, called from `anylinuxfs/src/fsutil.rs` |
| `codesign` | Xcode CLI tools | Ad-hoc binary signing in `build-app.sh` |

## Guest VM System Commands

The following Linux/FreeBSD tools are invoked inside the microVM by `vmproxy/src/main.rs`:

| Tool | OS | Purpose |
|------|----|---------| 
| `cryptsetup` | Linux | LUKS volume decryption |
| `mdadm` | Linux | Software RAID assembly |
| `lvm`/`vgscan`/`vgchange`/`lvdisplay` | Linux | LVM logical volume management |
| `zpool import` / `zfs list` | Linux+FreeBSD | ZFS pool import and dataset listing (`vmproxy/src/zfs.rs`) |
| `mount`/`umount` | Linux | Filesystem mounting (via `sys-mount` crate and shell scripts) |
| `rpcbind` | Linux | RPC service registration inside guest |
| `apk` | Linux (Alpine) | Alpine package management |
| `zfs`/`zpool` | FreeBSD | ZFS native support on FreeBSD guest |
| `freebsd-update` equivalents | FreeBSD | Base system bootstrapping |

## File Storage

**Databases:** None
**File storage:** Local filesystem only
- Guest rootfs: `~/.anylinuxfs/alpine/rootfs/` (Alpine) or `~/.anylinuxfs/freebsd-*/` (FreeBSD)
- Kernel images: `~/.anylinuxfs/alpine/` (downloaded from GitHub Releases)
- Logs: `~/.anylinuxfs/*.log`

## Authentication & Identity

**No auth providers.** Auth model:
- System-level: `sudo` for `/dev/disk*` access on macOS
- Privilege dropping: `krun_setuid`/`krun_setgid` in `anylinuxfs/src/bindings.rs`; vmnet-helper runs with dropped privileges
- Passphrases: `ALFS_PASSPHRASE` env var or TTY prompt via `rpassword` 7.4.0 — never logged

## Monitoring & Observability

**Logging:** Custom prefixed logging via macros in `common-utils/src/log.rs` (`host_println!`, `host_eprintln!`, `prefix_println!`); log file at `~/.anylinuxfs/anylinuxfs.log`
**Error tracking:** None (no Sentry, Datadog, etc.)
**CI/CD:** None detected in repository

## Webhooks & Callbacks

**Incoming:** None (no web server)
**Outgoing:** None (no webhooks)

---

*Integration audit: 2026-04-10*
