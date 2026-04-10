---
phase: 02-vmproxy-event-emission
plan: 01
subsystem: ipc
tags: [vmproxy, rust, vmevent, ipc, eventsink, eventsource, vmctrl]

# Dependency graph
requires:
  - phase: 01-vmctrl-protocol-types
    provides: VmEvent enum and Response::VmEvent in common-utils/src/vmctrl.rs
provides:
  - EventState enum (Buffering/Live/Closed) in vmproxy/src/main.rs
  - EventSink cloneable handle for emitting VmEvent from any thread
  - CtrlSocketServer with event buffer — flushes on SubscribeEvents, streams live events, sends kernel-log report
  - All 20 stdout tag println!/eprintln! calls replaced with typed VmEvent IPC events
  - run_success flag emits ExitCode(1) via IPC on error path
affects: [anylinuxfs, cmd_mount, event subscription, tag scraping removal]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - EventState machine (Buffering→Live→Closed) via Arc<Mutex<>> shared across threads
    - EventSink as Clone wrapper over Arc<Mutex<EventState>> — cheaply passed to structs
    - Deferred closure cloning event_sink for passphrase prompt boundary signals

key-files:
  created: []
  modified:
    - vmproxy/src/main.rs

key-decisions:
  - "EventSink uses Arc<Mutex<EventState>> for safe sharing across threads without async"
  - "Bounded mpsc::sync_channel(64) for live events — prevents unbounded memory growth if subscriber stalls"
  - "EventSink::close() called in send_report() to unblock live_rx loop before sending report"
  - "run_success AtomicBool flag deferred to emit ExitCode(1) only on actual failure (not just quit)"

patterns-established:
  - "EventSink: pass .clone() to structs that need to emit events — cheap Arc clone"
  - "Deferred passphrase events: clone event_sink before deferred.add() closure capture"

requirements-completed: [VMPROXY-01, VMPROXY-02, VMPROXY-03, VMPROXY-04, VMPROXY-05, VMPROXY-06, VMPROXY-07, VMPROXY-08, VMPROXY-09]

# Metrics
duration: 30min
completed: 2026-04-11
---

# Phase 02-01: vmproxy Event Emission Summary

**EventSink/EventState IPC layer in vmproxy — all 20 stdout tags replaced with typed VmEvent messages buffered before SubscribeEvents and streamed live after**

## Performance

- **Duration:** ~30 min
- **Started:** 2026-04-11
- **Completed:** 2026-04-11
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments
- Added `EventState` enum (Buffering/Live/Closed) and `EventSink` cheaply-cloneable handle
- Reworked `CtrlSocketServer::new()`: events buffer before subscription, flushed atomically on `SubscribeEvents`, then streamed live until channel closes, then kernel-log report sent
- Wired `event_sink` into `VmDiskContext`, `CustomActionRunner`, and the `run()` function body
- Replaced all 20 `<anylinuxfs-tag:value>` stdout calls with typed `vmctrl::VmEvent` IPC events
- Added `run_success` AtomicBool flag — `ExitCode(1)` emitted via IPC deferred on error paths
- Removed `eprintln!("<anylinuxfs-exit-code:1>")` from `main()`
- Added `test_event_sink()` helper and updated all 8 unit test call sites

## Task Commits

1. **Task 1 & 2: EventSink/EventState + all 20 tags replaced** - `7526eda` (feat)

## Files Created/Modified
- `vmproxy/src/main.rs` — EventState enum, EventSink struct, reworked CtrlSocketServer, event_sink in VmDiskContext and CustomActionRunner, all tags replaced

## Decisions Made
- Used bounded `sync_channel(64)` to prevent unbounded memory if the subscriber stalls
- `EventSink::close()` is called before sending the report to unblock `live_rx` iteration
- Deferred closures that emit passphrase boundary events clone the `EventSink` before capture

## Deviations from Plan

None — plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None — no external service configuration required.

## Next Phase Readiness
- Guest-side IPC migration is complete: vmproxy never emits structured data on stdout (except `<anylinuxfs-vmproxy-ready>` which bootstraps the connection)
- Host side (`anylinuxfs`) can now subscribe to `Response::VmEvent` messages and remove its stdout tag-scraping logic
- All downstream cargo checks pass: vmproxy (musl + freebsd), anylinuxfs, 8 unit tests

---
*Phase: 02-vmproxy-event-emission*
*Completed: 2026-04-11*
