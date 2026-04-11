---
phase: "03"
slug: host-ipc-consumer
status: verified
threats_open: 0
asvs_level: 1
created: 2026-04-11
---

# Phase 03 — Security: host-ipc-consumer

> Per-phase security contract: threat register, accepted risks, and audit trail.

---

## Trust Boundaries

| Boundary | Description | Data Crossing |
|----------|-------------|---------------|
| IPC socket → host | VmEvent data originates from vmproxy inside the VM — trusted (our own code) | Typed VmEvent structs (fs label, fs type, export paths, exit code) |
| PTY → host | Stdout output forwarded from VM — no longer used for structured data after this phase | Plain log text; only `vmproxy-ready` bootstrap signal retained |

---

## Threat Register

| Threat ID | Category | Component | Disposition | Mitigation | Status |
|-----------|----------|-----------|-------------|------------|--------|
| T-03-01 | Spoofing | vmproxy-ready on stdout | accept | The vmproxy-ready check bootstraps the IPC connection; it acts only on the *presence* of the line, not the payload — no data is trusted from this signal | closed |
| T-03-02 | Denial | mpsc::try_recv starvation | accept | try_recv is non-blocking; called before and after each PTY read, plus final drain at `READY AND WAITING` — sufficient coverage to drain all events under normal operation | closed |
| T-03-03 | Tampering | VmEvent channel in PtyReader | accept | Channel is mpsc (single producer); the sole producer is our own `start_ipc_event_reader` thread started by PtyReader itself — no external write path exists | closed |

*Status: open · closed*
*Disposition: mitigate (implementation required) · accept (documented risk) · transfer (third-party)*

---

## Accepted Risks Log

| Risk ID | Threat Ref | Rationale | Accepted By | Date |
|---------|------------|-----------|-------------|------|
| AR-03-01 | T-03-01 | vmproxy-ready signal is a bootstrap-only line presence check with no payload trust. Spoofing the line in VM stdout only triggers an IPC connection attempt — connection failure is now logged (fix F-01 in 03-REVIEW.md). | gsd-security-auditor | 2026-04-11 |
| AR-03-02 | T-03-02 | Non-blocking try_recv with triple-drain (before/after PTY read + at READY AND WAITING) ensures event delivery before NFS snapshot. Starvation requires artificially high event volume not produceable by vmproxy design. | gsd-security-auditor | 2026-04-11 |
| AR-03-03 | T-03-03 | mpsc channel has exactly one producer (start_ipc_event_reader thread). No external or untrusted write path exists. VmEvent values are deserialized from the vmctrl IPC stream which connects only to the trusted VM-side vmproxy process. | gsd-security-auditor | 2026-04-11 |

---

## Security Audit Trail

| Audit Date | Threats Total | Closed | Open | Run By |
|------------|---------------|--------|------|--------|
| 2026-04-11 | 3 | 3 | 0 | gsd-security-auditor (automated) |

---

## Sign-Off

- [x] All threats have a disposition (mitigate / accept / transfer)
- [x] Accepted risks documented in Accepted Risks Log
- [x] `threats_open: 0` confirmed
- [x] `status: verified` set in frontmatter

**Approval:** verified 2026-04-11
