# Codebase Concerns

**Analysis Date:** 2026-04-10

## Tech Debt

**Legacy config format still live:**
- Issue: Two legacy top-level config keys (`log_level`, `num_vcpus`, `ram_size_mib`) are still read and converted by `convert_legacy_config()` despite a pending FIXME to remove them.
- Files: `anylinuxfs/src/settings.rs` (line 303, 315), `common-utils/src/lib.rs` (line 248)
- Impact: Accumulating migration surface; every release risks breaking old user configs silently.
- Fix approach: Add a deprecation warning on load, document migration in README, remove after one release cycle.

**Commented-out code in utils.rs fork/pipe setup:**
- Issue: Two `libc::close()` calls for read/write pipe ends are commented out. Their absence may leave dangling fds in the child or parent process.
- Files: `anylinuxfs/src/utils.rs` (lines 229–232, 271–274, 342, 364, 380, 392, 398, 434, 450)
- Impact: Potential fd leaks across fork; any CLOEXEC omission would silently propagate fds into vmproxy.
- Fix approach: Audit each commented close and either restore it with the correct side or remove the comment.

**Commented-out libkrun NET_FEATURE constants:**
- Issue: All virtio-net feature flag constants are commented out in `vm.rs` and replaced with a literal `0`.
- Files: `anylinuxfs/src/vm.rs` (lines after `/// Taken from https://...`)
- Impact: Network offload features (TSO, UFO, checksum) are silently disabled; hard to re-enable later.
- Fix approach: Restore constants as `#[allow(unused)]` dead_code or re-enable appropriate flags.

**Commented-out global config accessors in settings.rs:**
- Issue: `fn global()` and `fn global_mut()` are commented out in the `Preferences` impl.
- Files: `anylinuxfs/src/settings.rs` (lines 396–401)
- Impact: Global (system-wide) config is effectively read-only; any future global preference write would require restoring or rewriting this path.
- Fix approach: Either remove unreachable code or implement it properly.

**TODO for FreeBSD custom actions in vmproxy:**
- Issue: `statfs()` used for FUSE export detection is `#[cfg(target_os = "linux")]` only; a TODO comment acknowledges it is not planned for FreeBSD.
- Files: `vmproxy/src/main.rs` (line 421)
- Impact: Custom actions may silently skip FUSE-related export configuration when running on FreeBSD.
- Fix approach: Implement FreeBSD-specific statfs or document that this edge case is unsupported.

---

## Security Considerations

**Passphrases exposed through process environment:**
- Risk: LUKS/BitLocker passphrases are read from `ALFS_PASSPHRASE*` environment variables and forwarded verbatim to the guest via `krun_set_env`. Any process that can read `/proc/<pid>/environ` of the host process (root or the same user) can recover passphrases.
- Files: `anylinuxfs/src/cmd_mount.rs` (lines 491–506), `anylinuxfs/src/bindings.rs` (`krun_set_env`), `vmproxy/src/main.rs` (line 468)
- Current mitigation: VM process is privilege-dropped; env vars are cleared from host after startup (not verified in code).
- Recommendations: Prefer passing passphrases via a short-lived pipe or vsock channel rather than env vars.

**NFS export uses `no_root_squash` by default:**
- Risk: The default NFS export in vmproxy uses `no_root_squash`, meaning root inside the VM has root-level access to files on the host NFS mount. A compromised vmproxy or guest escape could write as root to the exported filesystem.
- Files: `vmproxy/src/main.rs` (`export_args_for_path`, `no_root_squash` is the Linux default export mode)
- Current mitigation: The VM is a microVM confined by libkrun; block device access is already mediated.
- Recommendations: Document the implication; consider defaulting to `root_squash` for non-FUSE filesystems.

**Large unsafe FFI surface for libkrun:**
- Risk: All libkrun calls (`krun_create_ctx`, `krun_add_disk`, `krun_set_env`, `krun_start_enter`, etc.) are raw `unsafe extern "C"`. Wrong argument types, use-after-free of `CString` temporaries, or libkrun ABI changes could cause undefined behavior.
- Files: `anylinuxfs/src/bindings.rs` (entire file), `anylinuxfs/src/vm.rs` (all call sites)
- Current mitigation: Return values are checked; CString lifetimes are controlled locally.
- Recommendations: Audit each `CString::new(...).unwrap().as_ptr()` pattern for dangling pointer risk (the CString is dropped before the pointer is used in some patterns).

**CString dangling pointer pattern:**
- Risk: Pattern `CString::new(s).unwrap().as_ptr()` produces a pointer to a CString that is immediately dropped. This is undefined behavior.
- Files: `anylinuxfs/src/vm.rs` (lines 180, 225, 531, 532, 629), `anylinuxfs/src/rpcbind.rs` (line 128, 200, 202)
- Impact: Exploit-level memory safety issue; in practice the stack allocation may not be reused immediately, masking the bug.
- Fix approach: Bind each `CString` to a named variable before calling `.as_ptr()`.

**Root check relies on `/Users` path prefix:**
- Risk: `load_config` in `main.rs` rejects direct root execution by checking `home_dir.starts_with("/Users")`. This is macOS-specific and fragile — a non-standard home directory would bypass the guard.
- Files: `anylinuxfs/src/main.rs` (line 168–171)
- Current mitigation: This is a UX guard, not a security boundary; the actual privilege model is separate.
- Recommendations: Document the assumption; consider using `!home_dir.starts_with("/root")` style check for forward compatibility.

**`/tmp`-based Unix sockets and lock file:**
- Risk: All runtime sockets (`network-<id>.sock`, `anylinuxfs-<id>-vsock`, `vfkit-<id>.sock`, `anylinuxfs-<id>.sock`) and the global lock file (`/tmp/anylinuxfs.lock`) are created in `/tmp` with world-readable permissions by default.
- Files: `anylinuxfs/src/main.rs` (lines 213–215), `anylinuxfs/src/cmd_mount.rs` (line 1441)
- Current mitigation: Random 8-character suffix reduces guessability; API socket ownership is changed to invoker UID/GID.
- Recommendations: Use `mkdtemp` or `$TMPDIR` with restricted permissions for socket directories.

**`diskutil` spawned via `.expect()` (panic on missing binary):**
- Risk: `diskutil_list_from_plist` calls `cmd.output().expect("Failed to execute diskutil")`. If `diskutil` is unavailable, the process panics unconditionally rather than returning an error.
- Files: `anylinuxfs/src/diskutil.rs` (around line 1035)
- Impact: Silent crash on any macOS configuration where `diskutil` is absent or path-mangled by the user's environment.
- Fix approach: Use `.context("Failed to execute diskutil")?` instead.

---

## Performance Bottlenecks

**Busy-wait polling loops:**
- Problem: Several places use `thread::sleep` in tight loops to wait for state changes.
- Files: `anylinuxfs/src/cmd_mount.rs` (line 185, 100 ms sleep in a loop), `anylinuxfs/src/fsutil.rs` (line 328, 100 ms sleep), `anylinuxfs/src/pubsub.rs` (line 174, 5 ms sleep)
- Cause: No notification mechanism; polling is the fallback.
- Improvement path: Replace with `condvar` or channel-based notification where applicable.

**NFS over loopback adds latency:**
- Problem: The NFS stack (guest → gvproxy port forward → host NFS client) adds multiple layers of overhead for every filesystem operation.
- Files: `anylinuxfs/src/fsutil.rs` (NFS mount path), `vmproxy/src/main.rs` (NFS export)
- Cause: Architectural decision; NFS is the only portable IPC mechanism supported by macOS natively.
- Improvement path: Evaluate virtiofs (`krun_add_virtiofs`) as a lower-latency alternative for future work.

---

## Fragile Areas

**RAID device path hardcoded as `/dev/md127`:**
- Files: `anylinuxfs/src/cmd_mount.rs` (RAID branch, `vm_path = "/dev/md127"`)
- Why fragile: If multiple RAID arrays are assembled or the kernel assigns a different minor number, the path will be wrong.
- Safe modification: Probe for the actual md device name from `mdadm` output in vmproxy.
- Test coverage: `tests/22-raid.bats` covers basic RAID but not the multi-array edge case.

**`diskutil list` output parsing via regex:**
- Files: `anylinuxfs/src/diskutil.rs` (`list_partitions`, numbered_pattern, part_type_pattern)
- Why fragile: Parses human-readable terminal output of `diskutil list`. Any macOS update that reformats this output breaks partition discovery.
- Safe modification: The plist parsing path (`diskutil_list_from_plist`) is the reliable path; the regex path is only used for display line augmentation. Keep screen-scraping minimal.
- Test coverage: No unit tests for the regex parsing; integration tests only run on a real macOS environment.

**VM stdout tag parsing:**
- Files: `anylinuxfs/src/main.rs` (`parse_vm_tag_value`), `vmproxy/src/main.rs` (`println!("<anylinuxfs-type:{}>", ...)`)
- Why fragile: The host reads the filesystem type and NFS export paths by scraping `<tag:value>` lines from the VM's stdout. Any stray output matching the pattern would corrupt the protocol.
- Safe modification: Use the existing IPC control socket for structured data exchange.

**Alpine rootfs depends on Docker Hub availability:**
- Files: `init-rootfs/main.go`, `anylinuxfs/src/vm_image.rs`
- Why fragile: First-run and `anylinuxfs init` require network access to Docker Hub. Rate limiting, outages, or image format changes (OCI) can silently break initialization.
- Safe modification: Bundle a fallback rootfs tarball or support a local mirror URL.

**`wait_for_proc_exit` 5-second timeout:**
- Files: `anylinuxfs/src/cmd_mount.rs` (`wait_for_proc_exit`, default 5s)
- Why fragile: Some VM teardown sequences (e.g. LUKS with large dirty pages) may exceed 5 seconds, causing premature SIGKILL.
- Safe modification: Make timeout configurable or increase for encrypted volumes.

---

## Known Bugs

**ntfs3 data corruption on hibernated/Fast Startup Windows drives:**
- Symptoms: Mounting a hibernated Windows volume with `-t ntfs3` may silently corrupt data.
- Files: `vmproxy/src/main.rs` (ntfs3 mount path)
- Trigger: Windows Fast Startup enabled and drive not cleanly unmounted.
- Workaround: Use default `ntfs-3g` driver; or run `chkdsk` on Windows first.
- Reference: `docs/important-notes.md`

**Permission issues on ntfs3 Windows system drives:**
- Symptoms: `/Program Files` and parts of `/Users` appear read-only even with write mount.
- Files: `vmproxy/src/main.rs`
- Trigger: Mounting Windows system partition with `ntfs3`.
- Workaround: Use `ntfs-3g` (the default).

---

## Scaling Limits

**Fixed NFS ports 2049, 32767, 32765:**
- Current capacity: Ports are forwarded at fixed values by gvproxy.
- Files: `vmproxy/src/main.rs` (`init_network`, port 2049/32765/32767)
- Limit: Any other NFS server on the host causes a failure (documented in `docs/troubleshooting.md`).
- Scaling path: Dynamic port allocation with matching NFS client `port=` options.

---

## Dependencies at Risk

**libkrun is pinned at install time (no version pinning in code):**
- Risk: `bindings.rs` declares raw extern-C symbols without version guards. A libkrun ABI change silently breaks the binary at runtime.
- Impact: Crash or wrong behavior after any Homebrew `libkrun` upgrade.
- Migration plan: Add a version check at startup via the library's version API, or document the exact supported version range in README.

**gvproxy bundled as a binary in `libexec/`:**
- Risk: The bundled `libexec/gvproxy` binary has no integrity check. If replaced or corrupted, the network layer silently fails.
- Files: `anylinuxfs/src/vm_network.rs`
- Migration plan: Add a SHA-256 check or code-sign the libexec directory.

---

## Test Coverage Gaps

**`rpcbind.rs` unsafe FFI has no unit tests:**
- What's not tested: `register_service`, `unregister_service`, `list_services` — all call macOS `oncrpc` framework functions through raw `unsafe` blocks.
- Files: `anylinuxfs/src/rpcbind.rs`
- Risk: Silent registration failures or memory safety violations go undetected.
- Priority: High

**`diskutil.rs` regex parsing has no unit tests:**
- What's not tested: `list_partitions` regex path, `augment_line`, `LvIdent` parser, `lv_size_split_val_and_units`.
- Files: `anylinuxfs/src/diskutil.rs`
- Risk: macOS version updates to `diskutil list` output format break partition discovery silently.
- Priority: High

**`devinfo.rs` / libblkid bindings lack unit tests:**
- What's not tested: `DevInfo::pv()`, `DevInfo::probe_image()`, blkid tag parsing.
- Files: `anylinuxfs/src/devinfo.rs`
- Risk: Wrong filesystem type detection leads to wrong kernel selection or mount failure.
- Priority: Medium

**FreeBSD vmproxy code paths untested in CI:**
- What's not tested: `#[cfg(target_os = "freebsd")]` blocks in `vmproxy/src/main.rs` and ZFS import paths.
- Files: `vmproxy/src/main.rs`, `vmproxy/src/zfs.rs`
- Risk: FreeBSD-specific changes silently break without macOS CI catching them.
- Priority: Medium

---

## Unmaintained Code

**`.kernel-builder/` directory:**
- Status: Explicitly unmaintained and superseded. Originally a Lua-based microVM launcher for early development and custom kernel builds. `anylinuxfs shell` serves this purpose now.
- Risk: Confuses contributors; build scripts in that directory may fail or contain outdated patterns.
- Recommendation: Remove from repository or archive in a separate branch.

---

*Concerns audit: 2026-04-10*
