# anylinuxfs — IPC Migration (Milestone 1)

## What This Is

`anylinuxfs` is a macOS CLI utility that mounts any Linux-supported filesystem (ext4, btrfs, xfs, NTFS, exFAT, ZFS, etc.) with full read/write support. It runs a lightweight `libkrun` microVM that mounts the filesystem and exports it to the host via NFS. This milestone removes the fragile stdout tag-scraping protocol and replaces it with structured IPC events over the existing control socket.

## Core Value

The host and vmproxy exchange structured data reliably — no stray VM output can corrupt the protocol.

## Requirements

### Validated

- ✓ microVM lifecycle management (start, stop, status) — existing
- ✓ NFS-based filesystem exposure to macOS host — existing
- ✓ IPC control socket with `SubscribeEvents`/`ReportEvent` pattern — existing
- ✓ RON-serialized length-prefixed framing in `common-utils/src/ipc.rs` — existing
- ✓ Passphrase handling via env vars and interactive TTY prompt — existing
- ✓ LUKS, LVM, RAID, ZFS, NTFS, exFAT, ext4, btrfs, xfs, UFS support — existing

### Active

- [ ] `VmEvent` enum in `common-utils/src/vmctrl.rs` covers all current stdout tag types
- [ ] `<anylinuxfs-vmproxy-ready>` is the sole remaining stdout tag (bootstrap signal)
- [ ] vmproxy buffers events that occur before IPC subscription is active, then flushes them after the host subscribes
- [ ] vmproxy sends fs type, fs label, NFS export path(s), mount:changed-to-ro, passphrase-prompt start/end, force-output on/off, and exit code via IPC events
- [ ] `cmd_mount.rs` consumes all VM state from IPC events instead of parsing stdout lines
- [ ] `parse_vm_tag_value` and all `<anylinuxfs-*>` tag parsing code is removed from the host
- [ ] All existing integration tests pass after migration

### Out of Scope

- Changing the NFS transport or moving to virtiofs — separate architectural concern
- Passphrase security improvement (pipe vs env var) — separate security concern (see CONCERNS.md)
- Dynamic NFS port allocation — scaling concern, separate milestone
- FreeBSD-specific vmproxy improvements — separate track

## Context

**Current protocol (stdout tag-scraping):**

The host reads vmproxy's PTY stdout line by line. Lines matching `<anylinuxfs-tag:value>` are control signals; the rest are log output. Currently used tags:

| Tag | Direction | Purpose |
|-----|-----------|---------|
| `<anylinuxfs-vmproxy-ready>` | vmproxy → host | IPC socket is now accepting connections |
| `<anylinuxfs-type:FS>` | vmproxy → host | Detected filesystem type |
| `<anylinuxfs-label:LBL>` | vmproxy → host | Detected filesystem label |
| `<anylinuxfs-nfs-export:PATH>` | vmproxy → host | NFS export path ready (can repeat) |
| `<anylinuxfs-mount:changed-to-ro>` | vmproxy → host | Mount degraded to read-only |
| `<anylinuxfs-passphrase-prompt:start>` | vmproxy → host | About to request passphrase on TTY |
| `<anylinuxfs-passphrase-prompt:end>` | vmproxy → host | Passphrase entry complete |
| `<anylinuxfs-force-output:on/off>` | vmproxy → host | Toggle verbose log display |
| `<anylinuxfs-exit-code:N>` | vmproxy → host | vmproxy error exit code |

**Target protocol:**

All tags except `vmproxy-ready` become typed `VmEvent` variants delivered via the existing `SubscribeEvents` → `ReportEvent` IPC stream. Events that occur before the IPC subscription is established are buffered in vmproxy and flushed after the first `SubscribeEvents` request.

**Sequencing insight:**

Most events (fs type, label, nfs-export) are emitted AFTER vmproxy-ready in the current code, so they naturally arrive post-subscription. The buffer/flush mechanism handles events that precede the subscription (passphrase-prompt, force-output, early exit-code on error paths).

## Constraints

- **Sync-only**: No `async/await` or tokio — codebase is purely synchronous; use threads + channels
- **RON serialization**: All IPC messages use RON format (already established)
- **Backward compat**: `common-utils` is shared; protocol changes must be coordinated across `anylinuxfs` and `vmproxy`
- **Cross-compiled target**: vmproxy compiles for `aarch64-unknown-linux-musl` and `aarch64-unknown-freebsd`

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Keep `vmproxy-ready` as stdout tag | Bootstraps the IPC connection — no chicken-and-egg problem | — Pending |
| Buffer-and-flush for pre-subscription events | Events before IPC subscription must not be lost | — Pending |
| Extend `vmctrl.rs` with `VmEvent` enum | Clean separation from `Report` kernel-log stream | — Pending |
| Extend `Response` with new `VmEvent` variant | Keeps events and kernel-log reports orthogonal | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd-complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-04-10 after initialization*
