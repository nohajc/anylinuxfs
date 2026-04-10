# Codebase Structure

**Analysis Date:** 2026-04-10

## Directory Layout

```
anylinuxfs/                     # macOS host CLI (Rust)
├── src/
│   ├── main.rs                 # CLI entrypoint, AppRunner, lifecycle orchestration
│   ├── api.rs                  # Unix socket RPC server (RuntimeInfo)
│   ├── bindings.rs             # FFI bindings to libkrun C API
│   ├── cli.rs                  # clap CLI definitions (all subcommand structs)
│   ├── cmd_mount.rs            # Mount/unmount orchestration (NFS flow)
│   ├── devinfo.rs              # Device probing via libblkid (DevInfo)
│   ├── diskutil.rs             # macOS DiskArbitration bindings, disk list
│   ├── fsutil.rs               # Mount table queries, NfsOptions builder
│   ├── netutil.rs              # IP allocation, DNS resolution
│   ├── pubsub.rs               # PubSub<T> event hub
│   ├── rpcbind.rs              # FFI bindings to macOS oncrpc framework
│   ├── settings.rs             # Config, MountConfig, Preferences, KernelConfig
│   ├── utils.rs                # Process management, PTY, signals, ForkOutput, StatusError
│   ├── vm.rs                   # VMContext, VMOpts, setup_vm(), start_vm() via libkrun
│   ├── vm_image.rs             # Alpine and FreeBSD rootfs initialization
│   └── vm_network.rs           # gvproxy / vmnet-helper startup, port forwarding
├── build.rs                    # Build script (links oncrpc, sets rpath)
├── Cargo.toml                  # Rust manifest (anylinuxfs, features = ["freebsd"])
├── tests/                      # Rust unit tests for anylinuxfs
└── target/                     # Build output (gitignored)

vmproxy/                        # MicroVM guest agent (Rust, cross-compiled)
├── src/
│   ├── main.rs                 # Agent entrypoint: network init, disk stack, NFS, ctrl socket
│   ├── kernel_cfg.rs           # Kernel module loading helpers (Linux)
│   ├── utils.rs                # Shell script helpers (script(), script_output())
│   └── zfs.rs                  # ZFS-specific mount helpers
├── bsd-build.sh                # FreeBSD cross-compilation helper script
├── Cargo.toml                  # Rust manifest (vmproxy, linux+freebsd targets)
├── freebsd-sysroot/            # FreeBSD sysroot for cross-compilation
└── target/                     # Build output (gitignored)

common-utils/                   # Shared library (Rust, used by both components)
├── src/
│   ├── lib.rs                  # Deferred, OSType, NetHelper, VM_IP/GATEWAY constants, helpers
│   ├── ipc.rs                  # Wire framing: [u32 BE len][RON payload], Handler, Client
│   ├── vmctrl.rs               # Control protocol: Request, Response, Report message enums
│   └── log.rs                  # Dual-sink logging (console + file), log macros, Prefix enum
├── Cargo.toml                  # Shared library manifest
└── target/                     # Build output (gitignored)

init-rootfs/                    # Alpine rootfs bootstrapper (Go, runs on macOS host)
├── main.go                     # OCI pull, unpack, apk install, vmproxy copy
├── go.mod                      # Go module (anylinuxfs/init-rootfs)
├── default-alpine-packages.txt # Packages installed into rootfs by default
└── vmrunner/                   # Subpackage: lightweight VM runner used during rootfs setup

freebsd-bootstrap/              # FreeBSD image bootstrapper (Go, runs via bootstrap VM)
├── main.go                     # FreeBSD image setup inside FreeBSD bootstrap VM
├── main_test.go                # Go tests for bootstrap logic
├── go.mod                      # Go module
├── Makefile                    # Build rules
├── config.json                 # Bootstrap config
└── txz2iso.sh                  # Utility: convert FreeBSD txz archives to ISO

tests/                          # BATS integration tests
├── run-tests.sh                # Test runner (runs all .bats files)
├── 01-ext4.bats                # ext4 mount/unmount test
├── 02-btrfs.bats               # btrfs test
├── 03-exfat.bats               # exFAT test
├── 04-f2fs.bats                # F2FS test
├── 05-ntfs.bats                # NTFS test
├── 06-zfs.bats                 # ZFS test
├── 07-ufs.bats                 # UFS (FreeBSD) test
├── 10-partitioned-disk.bats    # Partitioned disk test
├── 11-lvm.bats                 # LVM logical volume test
├── 12-luks.bats                # LUKS encryption test
├── 13-hdiutil-attach.bats      # hdiutil attach workflow
├── 14-multi-disk-btrfs.bats    # Multi-disk btrfs test
├── 15-multi-instance.bats      # Multiple concurrent mounts
├── 16-freebsd-zfs-multi.bats   # FreeBSD ZFS test
├── 17-keyfile.bats             # Key file decryption test
├── 18-image-partition.bats     # Partition image test
├── 20-subcommands.bats         # CLI subcommand tests
├── 21-mount-options.bats       # Mount option tests
├── 22-raid.bats                # RAID (mdadm) test
├── artifacts/                  # Pre-built test images and fixtures
└── test_helper/                # BATS helper libraries

libexec/                        # Bundled helper binaries (shipped with anylinuxfs)
├── gvproxy                     # Network bridge (user-space virtio-net)
├── vmnet-helper                # macOS vmnet network bridge
├── vmproxy                     # Guest agent binary (Linux/aarch64-musl)
├── vmproxy-bsd                 # Guest agent binary (FreeBSD/aarch64)
├── init-rootfs                 # Alpine rootfs bootstrapper binary
├── freebsd-bootstrap           # FreeBSD bootstrapper binary
├── Image                       # Linux kernel image for microVM
└── Image-4K                    # Linux kernel image (4K page size variant)

share/                          # Non-binary runtime data
├── alpine/
│   └── rootfs.ver              # Expected rootfs version string (hash/tag)
└── freebsd/                    # FreeBSD image metadata

bin/                            # Installed host binary
└── anylinuxfs                  # Main CLI binary (built by build-app.sh)

etc/                            # Default configuration
└── anylinuxfs.toml             # Default config (Homebrew prefix: /opt/homebrew/etc/)

docs/                           # User-facing documentation
├── building.md
├── custom-actions.md
├── examples.md
├── important-notes.md
├── luks-lvm.md
└── troubleshooting.md

.github/
└── instructions/
    └── copilot-instructions.md # Project conventions and build instructions
```

## Directory Purposes

**`anylinuxfs/src/`:**
- Purpose: All macOS host logic
- Key entry: `main.rs` → `AppRunner::run()` dispatches CLI subcommands
- Settings loaded once: `settings::load_config()` → `Config` struct passed by reference

**`vmproxy/src/`:**
- Purpose: Entire guest agent implementation in a single `main.rs` plus helpers
- `zfs.rs`: ZFS pool import and dataset listing
- `kernel_cfg.rs`: Loading kernel modules (e.g. `overlay` for FreeBSD unionfs)
- `utils.rs`: `script()` / `script_output()` wrappers for running shell commands

**`common-utils/src/`:**
- Purpose: Zero-duplication between host and guest code
- `ipc.rs` defines the single framing implementation used by both the control socket and the API socket
- `vmctrl.rs` is the canonical source of truth for all control messages
- Add new shared types here, not in either anylinuxfs or vmproxy

**`init-rootfs/`:**
- Purpose: One-shot Alpine rootfs bootstrap; not a VM component
- Outputs: `~/.anylinuxfs/alpine/rootfs/` populated with NFS tools, vmproxy binary, and Alpine packages
- `vmrunner/` subpackage runs a minimal VM to execute setup scripts inside Alpine

**`tests/`:**
- Purpose: End-to-end BATS test suite
- Naming pattern: `NN-<feature>.bats` (two-digit prefix for ordering)
- `test_helper/` contains shared BATS helper functions (e.g. `hdiutil_attach`, `hdiutil_detach`)
- Run individual tests: `bats tests/<file>.bats`; full suite: `./tests/run-tests.sh`

**`libexec/`:**
- Purpose: All helper binaries shipped and consumed by anylinuxfs at runtime
- Paths resolved relative to the `anylinuxfs` binary: `exec_dir/../libexec/`
- vmproxy binaries are placed here by `build-app.sh` after cross-compilation

## Key File Locations

**Entry Points:**
- `anylinuxfs/src/main.rs`: Host CLI main function and `AppRunner`
- `vmproxy/src/main.rs`: Guest agent main function
- `init-rootfs/main.go`: Alpine rootfs bootstrapper
- `freebsd-bootstrap/main.go`: FreeBSD image bootstrapper

**Configuration:**
- `etc/anylinuxfs.toml`: Global config template (installed to Homebrew prefix)
- `~/.anylinuxfs/config.toml`: Per-user config (loaded at runtime, not in repo)
- `anylinuxfs/src/settings.rs`: `Config`, `MountConfig`, `Preferences` types

**IPC Protocol:**
- `common-utils/src/ipc.rs`: Wire framing — the only place that reads/writes length-prefixed RON messages
- `common-utils/src/vmctrl.rs`: Control message types (`Request`, `Response`, `Report`)
- `anylinuxfs/src/api.rs`: Host-side API socket server and `RuntimeInfo` type

**VM Launch:**
- `anylinuxfs/src/vm.rs`: `setup_vm()` + `start_vm()` — all libkrun FFI calls
- `anylinuxfs/src/bindings.rs`: Raw `extern "C"` declarations linking to `libkrun`
- `anylinuxfs/src/vm_network.rs`: `start_gvproxy()` / `start_vmnet_helper()` + `connect_to_vm_ctrl_socket()`

**Testing:**
- `tests/`: BATS integration tests
- `run-rust-tests.sh`: Runs `cargo test` for all Rust crates on macOS (targeting native, not cross-compiled targets)

## Naming Conventions

**Rust Files:**
- `snake_case.rs` for all modules
- Module name matches its primary responsibility: `vm_network.rs` (networking), `vm_image.rs` (image init), `cmd_mount.rs` (mount command impl)

**Rust Types:**
- `PascalCase` for structs/enums: `VMContext`, `MountConfig`, `DevInfo`, `RuntimeInfo`
- `snake_case` for functions: `setup_vm()`, `init_rootfs()`, `serve_info()`
- Constants: `SCREAMING_SNAKE_CASE`: `VM_CTRL_PORT`, `VM_IP`, `VM_GATEWAY_IP`

**Test Files:**
- `NN-<feature>.bats` — numeric prefix controls execution order
- Helpers in `tests/test_helper/` use `load_helper` BATS convention

**Binary Outputs:**
- `bin/anylinuxfs`: Main host binary
- `libexec/vmproxy`: Linux guest binary
- `libexec/vmproxy-bsd`: FreeBSD guest binary
- `libexec/init-rootfs`, `libexec/freebsd-bootstrap`: Bootstrapper binaries

## Where to Add New Code

**New CLI subcommand:**
- Add variant to `anylinuxfs/src/cli.rs` (clap `Commands` enum)
- Implement handler in `anylinuxfs/src/main.rs` (AppRunner match arm) or a dedicated `cmd_<name>.rs` module
- Register the module in `anylinuxfs/src/main.rs` with `mod cmd_<name>;`

**New control message (host → guest):**
- Add variant to `vmctrl::Request` in `common-utils/src/vmctrl.rs`
- Add handler branch in `vmproxy/src/main.rs` `CtrlSocketServer` match
- Add send logic in `anylinuxfs/src/cmd_mount.rs` (follow `send_quit_cmd()` pattern)

**New shared utility:**
- Add to `common-utils/src/lib.rs` (small helpers) or a new file under `common-utils/src/`
- Import via `use common_utils::<name>` in both `anylinuxfs` and `vmproxy`

**New filesystem type support:**
- Guest-side mounting logic: `vmproxy/src/main.rs`
- ZFS-specific code: `vmproxy/src/zfs.rs`
- FreeBSD-specific paths: guard with `#[cfg(any(target_os = "freebsd", target_os = "macos"))]`
- Add BATS integration test: `tests/NN-<fs>.bats` following existing test patterns

**New BATS test:**
- Create `tests/NN-<feature>.bats`
- Use helpers from `tests/test_helper/` (e.g. `hdiutil_attach`, `hdiutil_detach`)
- Run with: `bats tests/NN-<feature>.bats`

## Special Directories

**`target/` (multiple):**
- Purpose: Rust build output for each crate
- Generated: Yes
- Committed: No (gitignored)

**`libexec/`:**
- Purpose: Runtime helper binaries consumed by anylinuxfs
- Generated: Yes (by `build-app.sh`)
- Committed: Yes (pre-built binaries for distribution)

**`share/alpine/`:**
- Purpose: Contains `rootfs.ver` — the expected version string for the Alpine rootfs
- `vm_image.rs` compares this against the installed rootfs to decide whether to reinitialize

**`freebsd-bootstrap/freebsd-sysroot/` and `vmproxy/freebsd-sysroot/`:**
- Purpose: FreeBSD cross-compilation sysroot headers and libs
- Generated: Yes (by `build-app.sh` or `bsd-build.sh`)
- Committed: No

---

*Structure analysis: 2026-04-10*
