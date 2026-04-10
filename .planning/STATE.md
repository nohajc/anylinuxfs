# State: anylinuxfs IPC Migration

## Project Reference

**Core Value:** The host and vmproxy exchange structured data reliably — no stray VM output can corrupt the protocol.
**Current Focus:** Phase 1 — Protocol Types (define `VmEvent` in `common-utils`)
**Milestone:** 1

---

## Current Position

**Active Phase:** 1 — Protocol Types
**Active Plan:** None (not started)
**Status:** Planning

**Progress:**
```
Phase 1 [          ] 0%   Protocol Types
Phase 2 [          ] 0%   vmproxy Event Emission
Phase 3 [          ] 0%   Host IPC Consumer
Phase 4 [          ] 0%   Integration Verification
```

**Overall:** 0/4 phases complete (0%)

---

## Performance Metrics

| Metric | Value |
|--------|-------|
| Phases total | 4 |
| Phases complete | 0 |
| Plans written | 0 |
| Plans complete | 0 |
| Requirements mapped | 19/19 |
| Tests passing | unknown |

---

## Accumulated Context

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| Keep `vmproxy-ready` as stdout tag | Bootstraps the IPC connection — no chicken-and-egg problem |
| Buffer-and-flush for pre-subscription events | Events before IPC subscription must not be lost |
| Extend `vmctrl.rs` with `VmEvent` enum | Clean separation from `Report` kernel-log stream |
| Extend `Response` with `VmEvent` variant | Keeps events and kernel-log reports orthogonal |

### Architecture Notes

- `common-utils/src/vmctrl.rs` — shared protocol types; must change first
- `common-utils/src/ipc.rs` — RON-serialized length-prefixed framing; no changes needed
- `vmproxy/src/` — guest agent; cross-compiled for `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd`
- `anylinuxfs/src/cmd_mount.rs` — primary host consumer of VM state; main Phase 3 target
- `anylinuxfs/src/main.rs` — contains `parse_vm_tag_value`; must be removed in Phase 3

### Phase Dependencies

```
Phase 1 (protocol types)
  └── Phase 2 (vmproxy sends events)
        └── Phase 3 (host consumes events)
              └── Phase 4 (verify end-to-end)
```

### Active Todos

- [ ] Phase 1: Define `VmEvent` variants in `common-utils/src/vmctrl.rs`
- [ ] Phase 1: Add `Response::VmEvent(VmEvent)` variant

### Blockers

None.

---

## Session Continuity

**Last session:** 2026-04-10 — Roadmap created
**Handoff note:** No work started yet. Begin with Phase 1, plan the `common-utils` protocol changes first.

---

*State initialized: 2026-04-10*
*Last updated: 2026-04-10 after roadmap creation*
