# anylinuxfs Project Context

`anylinuxfs` is a macOS CLI utility designed to mount any Linux-supported filesystem (ext4, btrfs, xfs, NTFS, exFAT, etc.) with full write support. It achieves this by running a lightweight `libkrun` microVM and exposing the mounted filesystem to the host via NFS.

## Project Architecture

The project consists of several components working together:

*   **anylinuxfs (Rust/macOS):** The main CLI tool that runs on macOS. It manages the microVM lifecycle, handles disk arbitration, sets up networking (gvproxy), and mounts the NFS share.
*   **vmproxy (Rust/Linux/FreeBSD):** A proxy agent that runs inside the microVM. It is responsible for mounting the actual filesystems and communicating with the host. It is compiled for both `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd`.
*   **init-rootfs (Go/Linux):** A bootstrapper tool used to initialize the Alpine Linux root filesystem for the microVM. It downloads an Alpine OCI image, unpacks it, and sets up the environment. **Note:** This is NOT the VM's init process; `libkrun` has its own bundled init.
*   **freebsd-bootstrap (Go/FreeBSD):** A bootstrapper tool for initializing FreeBSD images, similar to `init-rootfs`.
*   **common-utils (Rust):** A shared library containing common logic used by both `anylinuxfs` and `vmproxy`.

## Building and Running

### Prerequisites
*   Rust toolchain (with `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd` targets).
*   Go toolchain.
*   Homebrew dependencies: `util-linux`, `libkrun`, `lld`, `llvm`, `pkgconf`.

### Build Commands
The project uses a central build script:
```bash
./build-app.sh            # Debug build
./build-app.sh --release  # Release build
```
This script compiles all components, handles cross-compilation for the microVM agents, and places the binaries in `bin/` and `libexec/`.

## Testing

### Rust Unit Tests
Unit tests for `anylinuxfs`, `vmproxy`, and `common-utils` can be run using the provided script:
```bash
./run-rust-tests.sh
```
This script ensures that `vmproxy` tests are run for the host architecture (macOS) to verify shared logic, while `anylinuxfs` and `common-utils` tests are run normally.

### Integration Tests
Integration tests are written using [BATS](https://github.com/bats-core/bats-core) and cover end-to-end scenarios (mounting various filesystems, LUKS, LVM, etc.):
```bash
./tests/run-tests.sh
```
**Prerequisites for integration tests:**
*   `bats-core` installed (`brew install bats-core`).
*   Project built (`./build-app.sh`).
*   Alpine rootfs initialized (`anylinuxfs init`).
*   The script will automatically install necessary Alpine packages (e.g., `e2fsprogs`, `btrfs-progs`) into the microVM rootfs on the first run.

### Key Commands
*   `anylinuxfs list`: Lists available filesystems and disk identifiers.
*   `anylinuxfs mount <DISK_IDENT>`: Mounts a filesystem (default command).
*   `anylinuxfs unmount`: Safely unmounts and terminates the VM.
*   `anylinuxfs status`: Shows the current mount status.
*   `anylinuxfs init`: Reinitializes the microVM root filesystem.

## Development Conventions

*   **Sudo Requirement:** Most `anylinuxfs` commands require `sudo` to access `/dev/disk*` nodes, but the microVM itself runs with dropped privileges.
*   **Filesystem Support:** Leverages standard Linux kernel drivers (via `libkrunfw`) and FUSE drivers (like `ntfs-3g`).
*   **NFS Integration:** Uses NFS for high-performance file access on macOS, bypassing the need for kernel extensions.

## Key Files and Directories

*   `anylinuxfs/src/`: Core macOS CLI implementation.
*   `vmproxy/src/`: MicroVM agent implementation.
*   `init-rootfs/`: Alpine rootfs bootstrapping logic (Go).
*   `common-utils/`: Shared Rust utilities (IPC, logging, etc.).
*   `tests/`: BATS-based integration tests.
*   `GEMINI.md`: This file, providing project context.
