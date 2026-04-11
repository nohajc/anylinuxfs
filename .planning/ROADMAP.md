# Roadmap: anylinuxfs IPC Migration

**Project:** anylinuxfs IPC Migration — Replace stdout tag-scraping with structured IPC events
**Milestone:** 1
**Core Value:** The host and vmproxy exchange structured data reliably — no stray VM output can corrupt the protocol.
**Granularity:** Standard
**Coverage:** 19/19 requirements mapped ✓

---

## Phases

- [x] **Phase 1: Protocol Types** - Define `VmEvent` enum and extend `Response` in `common-utils` so both crates share the new type definitions
- [x] **Phase 2: vmproxy Event Emission** - Replace all stdout tag `println!` calls in vmproxy with typed `VmEvent` IPC events, including buffer/flush for pre-subscription events
- [x] **Phase 3: Host IPC Consumer** - Rewrite `cmd_mount.rs` to subscribe to IPC events and remove all stdout tag parsing
- [ ] **Phase 4: Integration Verification** - Confirm end-to-end correctness: all integration tests pass, no tag remnants

---

## Phase Details

### Phase 1: Protocol Types
**Goal**: Define `VmEvent` and extend `Response` in `common-utils` so both `anylinuxfs` and `vmproxy` have the shared type definitions required for the migration.
**Depends on**: Nothing
**Requirements**: PROTO-01, PROTO-02, PROTO-03
**Success Criteria** (what must be TRUE):
  1. `VmEvent` enum compiles in `common-utils/src/vmctrl.rs` with all 9 variants: `FsType`, `FsLabel`, `NfsExport`, `MountChangedToRo`, `PassphrasePromptStart`, `PassphrasePromptEnd`, `ForceOutputOn`, `ForceOutputOff`, `ExitCode(i32)`
  2. `Response::VmEvent(VmEvent)` variant exists alongside `Ack` and `ReportEvent` in `vmctrl.rs`
  3. `cargo check -F freebsd` passes for `common-utils` without warnings on the new types
  4. `<anylinuxfs-vmproxy-ready>` is the only remaining stdout tag referenced in the IPC protocol documentation
**Plans**: TBD

### Phase 2: vmproxy Event Emission
**Goal**: Replace every stdout tag `println!`/`eprintln!` call in vmproxy with the corresponding `VmEvent` IPC emission, and add an event buffer that flushes to the subscriber on first `SubscribeEvents`.
**Depends on**: Phase 1
**Requirements**: VMPROXY-01, VMPROXY-02, VMPROXY-03, VMPROXY-04, VMPROXY-05, VMPROXY-06, VMPROXY-07, VMPROXY-08, VMPROXY-09
**Success Criteria** (what must be TRUE):
  1. vmproxy starts an in-memory event buffer at startup; events emitted before any client connects are stored, not dropped
  2. When a client sends `SubscribeEvents`, all buffered events are sent to the client in emission order before any subsequent live events
  3. No `<anylinuxfs-type:`, `<anylinuxfs-label:`, `<anylinuxfs-nfs-export:`, `<anylinuxfs-mount:`, `<anylinuxfs-passphrase-prompt:`, `<anylinuxfs-force-output:`, or `<anylinuxfs-exit-code:` strings remain in vmproxy source
  4. `cargo check --target aarch64-unknown-linux-musl -F freebsd` passes for `vmproxy`
**Plans**: TBD

### Phase 3: Host IPC Consumer
**Goal**: Rewrite `cmd_mount.rs` to drive all VM state from `VmEvent` IPC variants received after subscribing post `vmproxy-ready`, removing every stdout tag parsing branch.
**Depends on**: Phase 1, Phase 2
**Requirements**: HOST-01, HOST-02, HOST-03, HOST-04
**Success Criteria** (what must be TRUE):
  1. After receiving `<anylinuxfs-vmproxy-ready>` on stdout, `cmd_mount.rs` connects to the IPC socket and sends `Request::SubscribeEvents`
  2. All VM state variables (`fstype`, `fslabel`, `export_paths`, `passphrase_prompt`, `force_output`, `exit_code`, `mount_changed_to_ro`) are set exclusively by matching against received `VmEvent` variants
  3. `parse_vm_tag_value` no longer exists anywhere in the `anylinuxfs` codebase
  4. No `<anylinuxfs-*>` tag-matching code exists in `cmd_mount.rs` except the single `vmproxy-ready` check
  5. `cargo check -F freebsd` passes for `anylinuxfs`
**Plans**: TBD

### Phase 4: Integration Verification
**Goal**: Confirm the full IPC migration is correct end-to-end: all filesystem types mount successfully, both crates compile cleanly, and no tag remnants appear in runtime output.
**Depends on**: Phase 3
**Requirements**: QUAL-01, QUAL-02, QUAL-03
**Success Criteria** (what must be TRUE):
  1. All existing BATS integration tests pass: ext4, btrfs, exFAT, f2fs, NTFS, ZFS, UFS, partitioned disk, LVM, LUKS, hdiutil-attach, multi-disk btrfs, multi-instance, RAID, keyfile, image-partition, subcommands, mount-options
  2. `cargo check -F freebsd` passes for both `anylinuxfs` and `vmproxy` with zero errors
  3. Grepping `anylinuxfs` host log output for `<anylinuxfs-` during a live mount returns no matches (only `vmproxy-ready` is permitted)
**Plans**: TBD

---

## Progress

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Protocol Types | 1/1 | Complete | 2026-04-10 |
| 2. vmproxy Event Emission | 1/1 | Complete | 2026-04-11 |
| 3. Host IPC Consumer | 1/1 | Complete | 2026-04-11 |
| 4. Integration Verification | 0/? | Not started | - |

---

*Roadmap created: 2026-04-10*
*Last updated: 2026-04-11 after phase 3 completion*
