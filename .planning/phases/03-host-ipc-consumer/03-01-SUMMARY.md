---
phase: 03-host-ipc-consumer
plan: 01
subsystem: ipc
tags: [anylinuxfs, rust, vmevent, ipc, cmd_mount, pty_reader, tag_scraping]

# Dependency graph
requires:
  - phase: 01-protocol-types
    provides: VmEvent enum and Response::VmEvent in common-utils/src/vmctrl.rs
  - phase: 02-vmproxy-event-emission
    provides: vmproxy emits all VM state as VmEvent IPC messages
provides:
  - start_ipc_event_reader: loops over Response::VmEvent until ReportEvent, returns mpsc::Receiver<VmEvent>
  - process_vm_events: non-blocking drain helper called before/after every PTY read
  - PtyReader::spawn rewritten to consume VmEvents via try_recv instead of stdout tag parsing
  - All 9 stdout tag branches removed from PtyReader (only vmproxy-ready remains)
  - parse_vm_tag_value deleted from anylinuxfs/src/main.rs
affects: [phase 4 integration verification, anylinuxfs host binary]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - IPC event channel: start_ipc_event_reader returns Receiver<VmEvent>; PtyReader drains with try_recv
    - Triple drain: before PTY read, after PTY read, final drain at READY AND WAITING — ensures no events lost
    - process_vm_events: extracted helper with allow(clippy::too_many_arguments) for all 9 VmEvent arms

key-files:
  created: []
  modified:
    - anylinuxfs/src/cmd_mount.rs
    - anylinuxfs/src/main.rs

key-decisions:
  - "try_recv() is correct here: all VmEvents arrive before nfsd (READY AND WAITING), so the triple drain catches them all"
  - "process_vm_events extracted to helper to avoid code duplication (called 3x per loop iteration)"
  - "start_ipc_event_reader keeps config/vm_native_ip params — PtyReader fields unchanged"
  - "vmproxy-ready stdout check is the IPC bootstrap signal and intentionally preserved"

patterns-established:
  - "IPC event channel pattern: produce in spawned thread (start_ipc_event_reader), consume non-blocking (try_recv) in PTY thread"

requirements-completed: [HOST-01, HOST-02, HOST-03, HOST-04]

# Metrics
duration: 20min
completed: 2026-04-11
---

# Phase 03-01: Host IPC Consumer Summary

**All stdout tag scraping removed from cmd_mount.rs — VM state now flows exclusively through VmEvent IPC messages consumed via try_recv in PtyReader**

## Performance

- **Duration:** ~20 min
- **Completed:** 2026-04-11
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- `subscribe_to_vm_events` replaced by `start_ipc_event_reader` that loops over `Response::VmEvent` until `Response::ReportEvent`, returning `mpsc::Receiver<vmctrl::VmEvent>`
- `process_vm_events` helper added: drains the receiver with `try_recv()`, matching all 9 `VmEvent` variants
- `PtyReader::spawn()` rewritten: `vm_event_rx` populated on `vmproxy-ready`, drained before/after every PTY read and once more at `READY AND WAITING`
- All 9 stdout tag branches removed (`exit-code`, `label`, `type`, `mount:changed-to-ro`, `nfs-export`, `passphrase-prompt:start/end`, `force-output:on/off`)
- `parse_vm_tag_value` deleted from `anylinuxfs/src/main.rs` and its import removed from `cmd_mount.rs`

## Task Commits

1. **Tasks 1 & 2: IPC event reader + PtyReader rewrite + parse_vm_tag_value removal** - `2d29e2d` (feat)

## Files Created/Modified
- `anylinuxfs/src/cmd_mount.rs` — `start_ipc_event_reader`, `process_vm_events`, rewritten `PtyReader::spawn`, removed 9 tag branches and `parse_vm_tag_value` import
- `anylinuxfs/src/main.rs` — `parse_vm_tag_value` function deleted

## Decisions Made
- Used `try_recv()` (non-blocking) in PtyReader's existing thread loop rather than a separate consumer thread — avoids synchronization complexity
- Triple drain (before/after PTY read + final drain at READY AND WAITING) ensures no VmEvents are dropped
- Kept `PtyReader` struct fields unchanged (`config`, `vm_native_ip`) — passed to `start_ipc_event_reader` when vmproxy-ready arrives

## Deviations from Plan

None — plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None — no external service configuration required.

## Next Phase Readiness
- Host and guest IPC migration is complete end-to-end
- No `<anylinuxfs-*>` tag parsing remains in anylinuxfs (only `vmproxy-ready` bootstrap signal)
- `parse_vm_tag_value` fully deleted
- All verification checks pass: `cargo check -F freebsd`, 0 tag branches, 0 `parse_vm_tag_value` references, 8/8 unit tests
- Phase 4 (Integration Verification) can begin: BATS tests should pass with the new IPC flow

---
*Phase: 03-host-ipc-consumer*
*Completed: 2026-04-11*
