---
phase: "02"
status: findings
findings_count: 4
---

# Phase 02: Code Review — vmproxy-event-emission

**Reviewed:** 2025-07-17  
**Files Reviewed:** `vmproxy/src/main.rs`, `common-utils/src/vmctrl.rs`  
**Depth:** standard (per-file + concurrency-focused cross-analysis)

---

## Summary

The Phase 2 changes correctly replace the fragile stdout tag-scraping protocol with
typed `VmEvent` IPC events. The `EventState` / `EventSink` design is sound: buffering
before a subscriber connects, atomic flush-then-live transition, and LIFO cleanup via
`Deferred` are all correct in the happy path.

Four issues were found:

| # | Severity | Title |
|---|----------|-------|
| 1 | **high** | `entrypoint.sh` spawn failure silently treated as success — no `ExitCode(1)` emitted |
| 2 | **medium** | Race: `events_subscribed` set inside spawned thread, read from outer thread |
| 3 | **medium** | `send_report()` deadlocks on `done_rx.recv()` if called before subscriber connects and no Quit received |
| 4 | **low** | Unquoted `disk_path` interpolated into shell — broken on paths with spaces |

---

## Findings

### Finding 1 — HIGH: `entrypoint.sh` spawn failure silently treated as success

**File:** `vmproxy/src/main.rs:1311-1325`

**Issue:**  
When `Command::new("/usr/local/bin/entrypoint.sh").spawn()` returns `Err`, execution
falls through to `run_success.store(true, Ordering::Relaxed)` and then `Ok(())`. As a
result:

1. `run_success = true` → the deferred closure's guard does **not** emit
   `VmEvent::ExitCode(1)`, so the host never receives a structured error signal.
2. `run()` returns `Ok(())` → `main()` exits with `ExitCode::SUCCESS`.
3. `wait_for_quit_cmd()` is never called — NFS was presumably never started since
   entrypoint.sh is what launches it — but the host receives no indication of failure
   and will wait until its own mount timeout.

```rust
// current (buggy)
match Command::new("/usr/local/bin/entrypoint.sh").spawn() {
    Ok(mut hnd) => {
        ctrl_server.wait_for_quit_cmd();
        // ...
    }
    Err(e) => {
        eprintln!("Failed to start entrypoint.sh: {:#}", e);
        // falls through — no return Err(...)
    }
}
run_success.store(true, Ordering::Relaxed); // always reached
Ok(())
```

**Fix:**  
Return `Err(...)` from the `Err` arm so the error propagates correctly, causes
`run_success` to remain `false`, and lets the deferred cleanup emit `ExitCode(1)`:

```rust
match Command::new("/usr/local/bin/entrypoint.sh").spawn() {
    Ok(mut hnd) => {
        ctrl_server.wait_for_quit_cmd();
        println!("Exiting...");
        if let Err(e) = terminate_child(&mut hnd, "entrypoint.sh") {
            eprintln!("{:#}", e);
        }
    }
    Err(e) => {
        return Err(anyhow!("Failed to start entrypoint.sh: {:#}", e));
    }
}
run_success.store(true, Ordering::Relaxed);
Ok(())
```

Note: with this fix, finding 3 below also applies when entrypoint.sh fails to spawn
before any subscriber has connected — see that finding for the combined remedy.

---

### Finding 2 — MEDIUM: Race on `events_subscribed` between Quit and spawned thread start

**File:** `vmproxy/src/main.rs:278, 304–308, 314–315`

**Issue:**  
`events_subscribed.store(true, Ordering::Relaxed)` is the first line **inside** the
closure passed to `s.spawn(...)`. Between `s.spawn()` returning (in the accept loop
thread) and the spawned thread actually executing its first instruction, the OS
scheduler can suspend the spawned thread. If a `Quit` request arrives in that window:

```
accept loop:  s.spawn(move || {      <- spawned, but thread hasn't run yet
accept loop:  [reads next request]
accept loop:  Quit -> events_subscribed.load() == false
              -> done_tx.lock().take().unwrap().send(())   // fires prematurely!
spawned thread: (finally starts) events_subscribed.store(true)
               ...writes buffered events, live events...
               report_rx.recv() -> Ok(report)
               write_response(ReportEvent) ...
               done_tx.lock().take() -> None  // already consumed above
               // done_tx NOT signalled from here
```

Consequence: `send_report()` on the main thread calls `done_rx.recv()`, which returns
immediately (the Quit handler already fired it). `send_report()` returns and the main
thread proceeds to `run()` → `main()` returns → **process exits**. The subscriber
thread is inside `thread::scope`, but `thread::scope` is inside a detached
`thread::spawn`. When `std::process::exit` is called the detached thread (and its
scope children) are terminated mid-write. The kernel-log `ReportEvent` message may be
partially written or lost.

**Fix:**  
Set `events_subscribed` to `true` in the **outer** accept-loop thread *before* calling
`s.spawn()`, not inside the spawned closure:

```rust
vmctrl::Request::SubscribeEvents => {
    let event_state = Arc::clone(&event_state_server);

    // Mark subscribed BEFORE the thread starts so a concurrent Quit
    // cannot fire done_tx and return send_report() prematurely.
    events_subscribed.store(true, Ordering::Release);

    s.spawn(move || {
        // (no longer needed here)
        // Create live channel and drain the buffer atomically.
        let (live_tx, live_rx) = mpsc::sync_channel(64);
        // ...rest unchanged...
    });
}
```

Change the `Quit` handler's load to `Ordering::Acquire` to pair with the `Release`
store.

---

### Finding 3 — MEDIUM: `send_report()` deadlocks when called before any subscriber connects and without a Quit command

**File:** `vmproxy/src/main.rs:404–415` (the `send_report` method), `1154–1158` (deferred call site)

**Issue:**  
`send_report()` blocks on `done_rx.recv()` indefinitely:

```rust
fn send_report(&self, report: vmctrl::Report) -> anyhow::Result<()> {
    self.event_sink.close();
    self.report_tx.send(report)...?;
    _ = self.done_rx.recv();   // <-- blocks forever in the scenario below
    Ok(())
}
```

`done_rx` is only ever unblocked by one of two paths:
- The **subscriber thread** (after writing the kernel log report), or
- The **Quit handler** (when `events_subscribed == false` at Quit time).

If `run()` exits early due to an error (e.g., `init_network()` failure, the
entrypoint.sh bug from Finding 1, or any `?`-propagated error) **before**
`wait_for_quit_cmd()` is reached, AND the host has not yet sent `SubscribeEvents`, then:

- No subscriber thread exists to signal `done_rx`.
- No `Quit` command arrives (network may not even be up).
- `send_report()` (called from the deferred closure when `run()` returns) blocks
  forever, hanging the vmproxy process permanently.

This matters for all early-exit error paths, not just the entrypoint.sh case.

**Fix:**  
Use `recv_timeout` so cleanup can never hang indefinitely:

```rust
fn send_report(&self, report: vmctrl::Report) -> anyhow::Result<()> {
    self.event_sink.close();
    self.report_tx
        .send(report)
        .context("Failed to send report to ctrl socket")?;
    // Wait up to 30 s for the subscriber to finish; on early abort there may
    // be no subscriber or Quit, so we must not block forever.
    let _ = self.done_rx.recv_timeout(Duration::from_secs(30));
    Ok(())
}
```

A 30-second timeout is generous enough for the subscriber to write even a large kernel
log while still guaranteeing process exit after early fatal errors.

---

### Finding 4 — LOW: Unquoted `disk_path` interpolated into shell — broken on paths with spaces, injection vector

**File:** `vmproxy/src/main.rs:1264–1270`

**Issue:**  
`dsk.disk_path` is placed unquoted into a shell pipeline:

```rust
let opts = script_output(&format!(
    "mount | grep {} | awk -F'(' '{{ print $2 }}' | tr -d ')'",
    &dsk.disk_path       // ← no quoting
))
```

A disk path containing a space (e.g., `/dev/disk 1`) would split across shell words,
causing `grep` to receive multiple arguments and return wrong or empty output. This
silently produces an incorrect `effective_mount_options`, which in turn causes an
incorrect `export_mode` (`ro` vs `rw`) written to `/tmp/exports`.

Shell metacharacters in the path (`; & |` etc.) could also execute arbitrary
commands, though in practice disk paths come from the trusted host CLI.

**Fix:**  
Single-quote the variable, and use `grep -F --` to prevent regex/word-splitting issues:

```rust
let opts = script_output(&format!(
    "mount | grep -F -- '{}' | awk -F'(' '{{ print $2 }}' | tr -d ')'",
    &dsk.disk_path.replace('\'', "'\\''")   // escape embedded single-quotes
))
```

Or, if `disk_path` is guaranteed to be a `/dev/...` path without single-quotes (the
common case), a simple single-quoting of the interpolation is sufficient:

```rust
"mount | grep -F -- '{}' | awk -F'(' '{{ print $2 }}' | tr -d ')'",
```

---

## What Looks Correct

- **EventSink buffer→live transition** is atomic (lock held across `mem::replace` and
  `EventState::Live(live_tx)` install). No events can be lost between buffer drain and
  live channel start.
- **`ExitCode(1)` gating on `run_success`**: correct for the success path (Finding 1
  addresses the failure path gap).
- **`Ordering::Relaxed` on `run_success`**: safe — both the `store` and the `load`
  happen on the same thread (sequential in `run()` then in the dropped `Deferred`).
- **`report_rx` wrapped in `Arc<Mutex<Option<...>>>`**: correctly prevents a second
  `SubscribeEvents` from stealing the report receiver; the "unexpected state" early
  return leaves `done_tx` responsibility to the first subscriber.
- **`call_now(force_output_off)`** on mount success: correct. On mount failure the
  same closure remains in `Deferred` and fires during cleanup (reverse order confirmed
  from `Deferred::drop`), so `ForceOutputOff` is always sent.
- **`test_event_sink()` helper**: clean and sufficient for the unit tests.

---

_Reviewer: gsd-code-reviewer_  
_Depth: standard_
