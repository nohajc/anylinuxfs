# Coding Conventions

**Analysis Date:** 2026-04-10

## Error Handling

**Primary pattern:** `anyhow::Result<T>` throughout the entire codebase — both host-side (`anylinuxfs/src/`) and guest-side (`vmproxy/src/`), as well as the shared library (`common-utils/src/`).

**Context chaining rule:** Every `?` operator **must** be preceded by `.context(...)` or `.with_context(|| ...)` to preserve a human-readable error chain. String literals use `.context(...)`. Dynamic messages (format strings) use `.with_context(|| format!(...))`:

```rust
// Correct — static message
let home_dir = homedir::my_home()
    .context("Failed to get home directory")?
    .context("Home directory not found")?;

// Correct — dynamic message
let content = fs::canonicalize(&path)
    .with_context(|| format!("Failed to resolve path {}", &path))?;

// Wrong — context dropped, caller has no breadcrumb
let content = fs::read_to_string(&path)?;
```

**`StatusError`** (`anylinuxfs/src/utils.rs`): Used when child-process failures need to carry the raw exit status integer up through anyhow. Create with `StatusError::new(msg, status)`.  In `main.rs`, the top-level `run()` downcasts to `StatusError` and converts to a Unix exit code via `to_exit_code(status)`.

**No `unwrap` / `expect` in production paths.** Reserve them for:
- Inline test assertions inside `#[cfg(test)]` modules.
- Provably-unreachable branches (e.g. iterating over a just-constructed Vec where `next()` can never return `None`).
- Mutex lock results on infallible in-process mutexes (`.lock().unwrap()` when poison is impossible).

Examples of legitimate non-test `.unwrap()` occurrences: `raw_os_error().unwrap()` after confirming the error is an OS error, `last().unwrap()` after confirming an iterator is non-empty (`anylinuxfs/src/cmd_mount.rs` line 931).

## Concurrency Model

The codebase is **purely synchronous**. There is no `async/await`, no `tokio`, no `async_std`, no `futures` executor.

**Concurrency primitives used:**
- `std::thread::spawn` for background work (event loops, output readers, control socket listeners).
- `std::sync::mpsc` channels for one-direction message passing between threads.
- `std::sync::{Arc, Mutex}` for shared mutable state.
- `signal_hook` for OS signal handling (in `anylinuxfs/src/utils.rs`).

Example from `anylinuxfs/src/cmd_mount.rs`:
```rust
let (nfs_ready_tx, nfs_notify_rx) = mpsc::channel::<NfsStatus>();
_ = thread::spawn(move || { /* PtyReader loop */ });
let nfs_ready = nfs_notify_rx.recv()?; // blocks on main thread
```

## String Types

**`bstr::BString` and `ByteSlice`** (from the `bstr` crate) are used for data that is usually text but is not guaranteed to be valid UTF-8 — environment variables, filesystem labels, disk paths, command output, NFS export paths. This avoids the `String` vs `Vec<u8>` false dichotomy.

Imports always include the trait `bstr::ByteSlice` to unlock string-like operations (`.starts_with()`, `.find()`, `.as_bstr()`, etc.) on byte slices:
```rust
use bstr::{BStr, BString, ByteSlice, ByteVec};
```

Used in `anylinuxfs/src/main.rs`, `anylinuxfs/src/cmd_mount.rs`, `anylinuxfs/src/settings.rs`, `vmproxy/src/main.rs`, `common-utils/src/lib.rs`.

Plain `String` / `&str` is used only when UTF-8 validity is known (e.g. clap argument values, TOML config values, log messages).

## RAII Cleanup — `Deferred`

**`Deferred`** (`common-utils/src/lib.rs`) is the project-wide RAII cleanup pattern. It registers closures that run in **reverse registration order** when `Deferred` drops (similar to Go's `defer`). Use this instead of manual cleanup in `Drop` impls or scattered error branches.

```rust
// Typical usage — always use this for scope-exit cleanup
let mut deferred = Deferred::new();
let api_socket_path = format!("/tmp/anylinuxfs-{}.sock", id);
_ = deferred.add(|| { let _ = fs::remove_file(&api_socket_path); });

// Call-now variant — run and remove a registered action early
let action_id = deferred.add(move || { ... });
deferred.call_now(action_id);
```

Return value of `deferred.add(...)` is an `ActionID`. Assign with `_ = ...` when early cancellation isn't needed. Assign to a named variable (`let vm_wait_action = deferred.add(...)`) when you need `call_now` or `remove` later.

`Deferred` is used in `anylinuxfs/src/cmd_mount.rs`, `anylinuxfs/src/vm.rs`, `anylinuxfs/src/vm_image.rs`, and `vmproxy/src/main.rs`.

## Type-State for File Descriptors — `ForkOutput<O, I, C>`

`ForkOutput<O, I, C>` (`anylinuxfs/src/utils.rs`) tracks fd capabilities at compile time via generic parameters:
- `O` — output fd type: `PtyFd` (PTY master) or `PipeOutFds` (stdout/stderr pipes) or `()`
- `I` — input fd type: `PipeInFd` or `()`
- `C` — control fd type: `CommFd` or `()`

Each fd type implements a corresponding trait (`HasPtyFd`, `HasPipeOutFds`, `HasPipeInFd`, `HasCommFd`). `ForkOutput<O, I, C>` conditionally implements those traits when the type parameters are non-`()`.

When writing new fork-based code: use `fork_with_pty_output()` or `fork_with_pipe_output()` to obtain typed `ForkOutput`, then access fds via trait methods (`fork_out.master_fd()`, `fork_out.out_fd()`, `fork_out.comm_fd()`), **not** raw integers.

## Privilege Handling

At startup in `anylinuxfs/src/main.rs` (`load_config` function):
1. Read `SUDO_UID` / `SUDO_GID` env vars to determine the **invoker** (the user who ran `sudo anylinuxfs`).
2. Reject direct root invocation (uid == 0 without `SUDO_UID`).
3. Store `invoker_uid` / `invoker_gid` in `Config` for socket ownership and privilege dropping.

Unix socket ownership set to `(invoker_uid, invoker_gid)` after creation (`anylinuxfs/src/api.rs`).

Privilege drop functions `drop_privileges()` / `drop_effective_privileges()` / `elevate_effective_privileges()` are defined in `anylinuxfs/src/main.rs`.

Passphrases are never logged or printed. They are supplied via `ALFS_PASSPHRASE` (or `ALFS_PASSPHRASE1` etc. for multi-disk), collected in `prepare_vm_environment()` (`anylinuxfs/src/cmd_mount.rs`) and forwarded into the VM environment.

## Feature Gates

Optional FreeBSD support is gated behind the `freebsd` Cargo feature:
```rust
#[cfg(feature = "freebsd")]
pub fn fs_preferred_os(&self, fs_type: &str) -> OSType { ... }
```

Platform-specific code uses OS cfg guards:
```rust
#[cfg(target_os = "linux")]          // vmproxy vsock / procfs paths
#[cfg(target_os = "macos")]          // DiskArbitration / CoreFoundation
#[cfg(any(target_os = "freebsd", target_os = "macos"))]  // BSD shared paths
```

The `freebsd` feature flag must be passed to every `cargo build` / `cargo test` invocation that exercises FreeBSD paths (see Building section).

## Naming Patterns

**Files:** `snake_case.rs`. Module names match file names exactly.

**Functions / methods:** `snake_case`. Public API functions in `common-utils` are prefixed meaningfully (`wait_for_child`, `terminate_child`, `path_safe_label_name`).

**Types / structs / enums:** `CamelCase`. Error types end in `Error` (`StatusError`). Config structs end in `Config` (`MountConfig`, `KernelConfig`). Trait names are adjective phrases (`HasPtyFd`, `HasCommFd`).

**Constants:** `SCREAMING_SNAKE_CASE` (`LOCK_FILE`, `VM_CTRL_PORT`, `MAX_MSG_SIZE`).

**Enum variants:** `CamelCase`. Clap/serde rename attributes used where wire names differ (`#[clap(name = "linux")]`, `#[serde(rename = "gvproxy")]`).

**Private helpers vs public API:** helpers that are used only within a module are `fn` (not `pub`). Cross-module helpers are `pub(crate)` or `pub` based on whether they belong to a library crate (`common-utils`).

## Import Organization

```rust
// 1. std library
use std::fs::{self, File};
use std::path::{Path, PathBuf};

// 2. External crates (alphabetical within group)
use anyhow::{Context, anyhow};
use bstr::{BString, ByteSlice};

// 3. Internal crates / modules
use common_utils::{Deferred, ipc, log};
use crate::settings::Config;
```

Trait imports (`use bstr::ByteSlice`, `use std::io::{Read, Write}`) appear in the external-crate or std group as appropriate.

## Logging

Structured via macros from `common-utils/src/log.rs`:
- `host_println!(...)` — prefixed output to the macOS host console.
- `host_eprintln!(...)` — prefixed error output.
- `prefix_println!(prefix, ...)` — output with an explicit `log::Prefix`.

Never use bare `println!` / `eprintln!` in `anylinuxfs/` except inside `log.rs` itself or commented-out debug probes. `vmproxy/src/main.rs` uses `println!` for structured VM output tags (`<anylinuxfs-label:...>`).

## Code Formatting

`cargo fmt` (rustfmt with default settings) is **required** before every commit. Do not commit unformatted Rust code. The `build-app.sh` script does not auto-format — run `cargo fmt` manually in each affected crate directory before committing.

## Module Visibility

Internal implementation details should be `pub(crate)` rather than `pub` unless they are part of a library crate's external API. The `anylinuxfs` binary crate's modules (`api`, `cmd_mount`, `vm`, etc.) freely use `pub(crate)` for cross-module items.

## Comments

Inline comments explain *why*, not *what*. Non-obvious platform constraints are documented inline:
```rust
// Subtract 16 blocks (64 KiB) from the device-reported size so the
// superblock block count matches the smaller device mount mode exposes.
```

`TODO:` comments mark known limitations or deferred work (e.g. in `tests/12-luks.bats`, `anylinuxfs/src/cmd_mount.rs`).

---

*Convention analysis: 2026-04-10*
