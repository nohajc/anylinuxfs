# Requirements: anylinuxfs IPC Migration

**Defined:** 2026-04-10
**Core Value:** The host and vmproxy exchange structured data reliably — no stray VM output can corrupt the protocol.

## v1 Requirements

### Protocol

- [ ] **PROTO-01**: `VmEvent` enum is defined in `common-utils/src/vmctrl.rs` with variants for all current stdout tag types: `FsType`, `FsLabel`, `NfsExport`, `MountChangedToRo`, `PassphrasePromptStart`, `PassphrasePromptEnd`, `ForceOutputOn`, `ForceOutputOff`, `ExitCode`
- [ ] **PROTO-02**: `Response` in `vmctrl.rs` gains a `VmEvent(VmEvent)` variant alongside existing `Ack` and `ReportEvent`
- [ ] **PROTO-03**: `<anylinuxfs-vmproxy-ready>` remains as the sole stdout tag; the host uses it only as the bootstrap signal to connect to the IPC socket

### vmproxy (Guest)

- [ ] **VMPROXY-01**: vmproxy starts an IPC event buffer at startup; events emitted before a client subscribes are buffered (not dropped)
- [ ] **VMPROXY-02**: vmproxy replaces all `println!("<anylinuxfs-type:...>")` calls with `VmEvent::FsType` IPC events
- [ ] **VMPROXY-03**: vmproxy replaces all `println!("<anylinuxfs-label:...>")` calls with `VmEvent::FsLabel` IPC events
- [ ] **VMPROXY-04**: vmproxy replaces all `println!("<anylinuxfs-nfs-export:...>")` calls with `VmEvent::NfsExport` IPC events
- [ ] **VMPROXY-05**: vmproxy replaces all `println!("<anylinuxfs-mount:changed-to-ro>")` calls with `VmEvent::MountChangedToRo` IPC events
- [ ] **VMPROXY-06**: vmproxy replaces all `println!("<anylinuxfs-passphrase-prompt:start/end>")` calls with `VmEvent::PassphrasePromptStart` / `PassphrasePromptEnd` IPC events
- [ ] **VMPROXY-07**: vmproxy replaces all `println!("<anylinuxfs-force-output:on/off>")` calls with `VmEvent::ForceOutputOn` / `ForceOutputOff` IPC events
- [ ] **VMPROXY-08**: vmproxy replaces all `eprintln!("<anylinuxfs-exit-code:N>")` calls with `VmEvent::ExitCode(N)` IPC events
- [ ] **VMPROXY-09**: When a client subscribes via `SubscribeEvents`, vmproxy flushes buffered events to the client in order before continuing to stream live events

### Host (anylinuxfs)

- [ ] **HOST-01**: `cmd_mount.rs` subscribes to IPC events after receiving `vmproxy-ready` from stdout, and reads `VmEvent` variants instead of parsing tagged stdout lines
- [ ] **HOST-02**: `parse_vm_tag_value` function is removed from `anylinuxfs/src/main.rs`
- [ ] **HOST-03**: All `<anylinuxfs-*>` tag parsing branches in `cmd_mount.rs` are removed (except the `vmproxy-ready` check)
- [ ] **HOST-04**: All VM state variables (`fstype`, `fslabel`, `export_paths`, `passphrase_prompt`, `force_output`, `exit_code`, `mount_changed_to_ro`) are populated exclusively from IPC events

### Quality

- [ ] **QUAL-01**: All existing BATS integration tests pass after migration (ext4, btrfs, NTFS, LUKS, LVM, RAID, ZFS, UFS, etc.)
- [x] **QUAL-02**: `cargo check -F freebsd` passes for both `anylinuxfs` and `vmproxy` targets
- [x] **QUAL-03**: No `<anylinuxfs-*>` tagged lines appear in host stdout or log output after migration (except `vmproxy-ready`)

## v2 Requirements

### Future improvements enabled by this migration

- Remove kernel_log streaming via IPC now that all data exchange is structured (simplify `Report`)
- Add typed error events (structured error reporting instead of exit code)
- Bidirectional IPC: host sends passphrase via IPC instead of environment variables

## Out of Scope

| Feature | Reason |
|---------|--------|
| Moving passphrase delivery to IPC | Separate security concern; env var mechanism unchanged in this milestone |
| Changing NFS transport to virtiofs | Architectural change, separate workstream |
| Dynamic NFS port allocation | Scaling concern, separate milestone |
| FreeBSD-specific vmproxy FUSE statfs | Separate TODO, not related to stdout migration |
| Removing kernel_log from Report | Simplification opportunity; defer to avoid scope creep |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| PROTO-01 | Phase 1 | Pending |
| PROTO-02 | Phase 1 | Pending |
| PROTO-03 | Phase 1 | Pending |
| VMPROXY-01 | Phase 2 | Pending |
| VMPROXY-02 | Phase 2 | Pending |
| VMPROXY-03 | Phase 2 | Pending |
| VMPROXY-04 | Phase 2 | Pending |
| VMPROXY-05 | Phase 2 | Pending |
| VMPROXY-06 | Phase 2 | Pending |
| VMPROXY-07 | Phase 2 | Pending |
| VMPROXY-08 | Phase 2 | Pending |
| VMPROXY-09 | Phase 2 | Pending |
| HOST-01 | Phase 3 | Pending |
| HOST-02 | Phase 3 | Pending |
| HOST-03 | Phase 3 | Pending |
| HOST-04 | Phase 3 | Pending |
| QUAL-01 | Phase 4 | Pending |
| QUAL-02 | Phase 4 | Complete |
| QUAL-03 | Phase 4 | Complete |

**Coverage:**
- v1 requirements: 19 total
- Mapped to phases: 19
- Unmapped: 0 ✓

---
*Requirements defined: 2026-04-10*
*Last updated: 2026-04-10 after initialization*
