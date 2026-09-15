# Phase 3 Progress - Host-Agent + Real PTY + SQLite + FS + Browser + Computer + Binary RPC + Desktop + Workbench + Supervisor

## Summary
From Phase 0-2 (sandd kernel) to Phase 3A-E (Bot brain) with binary RPC, desktop manager, supervisor, full FS, PTY Ctrl handling, Bot->Session->Runtime handoff.

**Before:**
```
sandd: Runtime/Process/Exec/PTY/cgroup (pipe fallback, JSON state, 777)
```

**After:**
```
Chat/App -> Bot (long-lived) -> Session (Conversation, handoff) -> Runtime (cgroup, Xvfb, Chrome profile)
            host-agent Loop (freeze on ReadyForCheck/Permission) -> sandd (real PTY TERM/ANSI/resize/Ctrl, SQLite WAL, 0660, SO_PEERCRED, binary RPC, DesktopManager, Supervisor) + browser-worker (Playwright, profile, cgroup) + computer (X11 via sandd)
```

## What was done in this continuation (Phase 3B-E+)

### 1. Real PTY (Phase 3B must-fix) - enhanced
- Rewrote `pty.rs` from pipe fallback to real `openpty + fork + setsid + ioctl(TIOCSCTTY) + dup2 + execvp`
- Correct fd ownership via `dup`, reader/writer/waiter threads, ring buffer 1MB, `ReadPty` RPC
- `tty` returns `/dev/pts/0`, ANSI, interactive, resize works
- TERM=xterm-256color set in child, ANSI enabled
- Resize: `ioctl TIOCSWINSZ` + `kill SIGWINCH (28)`
- Raw mode: `SetPtyRaw` RPC (placeholder for tcsetattr), but PTY already handles raw bytes via WritePty
- Ctrl+C/D: `SignalPty` RPC (SIGINT 2, SIGTERM 15, etc) + raw byte handling:
  - Ctrl+C = 0x03 via WritePty binary or SignalPty SIGINT
  - Ctrl+D = 0x04 EOF via WritePty binary
  - Verified: `cat` + Ctrl+C 0x03 -> "^C" and terminates, `sleep 10` + SignalPty 2 -> terminates
- Fallback to pipe if openpty fails
- New tools: `terminal.resize`, `terminal.signal`, `terminal.close` (33 tools total)

### 2. UDS Security 777 -> 0660 + SO_PEERCRED
- `rpc.rs` chmod 0660, chgrp sand, `getsockopt(SO_PEERCRED)` logs pid/uid/gid

### 3. SQLite WAL
- FFI to libsqlite3, WAL, tables runtime/process/pty/session/task/event (no FK cascade)
- StateManager SQLite primary + JSON fallback, host-agent persistence same DB
- session survives runtime destroy verified

### 4. FS via sandd - now full set
- Before: read/write/list/search/stat/patch via exec
- Now:
  - Direct workspace FS via GetRuntime workspace + std::fs for efficiency
  - Fallback to exec + local
  - New tools:
    - `file.mkdir` (mkdir -p) via workspace + exec
    - `file.remove` (rm -rf) with permission check refusing dangerous path, via workspace + exec
    - `file.rename` (mv) via workspace + exec
    - `file.glob` (find + glob) via exec
  - Total file tools: read/write/list/search/stat/patch/mkdir/remove/rename/glob = 10
- `file.patch` search/replace + unified diff

### 5. Permission + Attention + Task Freeze
- `shell.exec` detects destructive `rm -rf /`, `mkfs`, `dd`, etc -> `permission_required` -> Attention::PermissionRequired + freeze to WaitingInput
- `file.remove` also checks dangerous path
- `task.complete` updates SQLite to ReadyForCheck, freezes loop to ReadyForCheck, emits Attention
- Loop: should_freeze on task.complete or permission_required

### 6. Binary RPC (no base64)
- `rpc_binary.rs` on `/run/sand/sandd-binary.sock` (0660), framed 4-byte BE len + JSON + binary
- Exec: stdout_len/stderr_len + raw bytes, WritePty: raw bytes, ReadPty: raw bytes, Screenshot: png bytes
- `sand-client`: call_binary, exec_binary, write/read pty binary, signal_pty, set_pty_raw
- host-agent uses binary first, fallback JSON
- Verified efficient

### 7. DesktopManager Xvfb
- `desktop.rs`: Xvfb per runtime, display :10-99 hashed, ensure_display, get_display, screenshot via import/scrot/xwd, destroy kills Xvfb
- Integrated into RuntimeManager, RPC EnsureDisplay/GetDisplay/Screenshot (binary)
- computer tools use sandd desktop first

### 8. Computer Use via sandd
- screenshot via EnsureDisplay+Screenshot binary, click/type via GetDisplay+exec xdotool in runtime with DISPLAY
- Fallback to local xdotool/import
- Browser-first fallback documented

### 9. Browser Worker lifecycle managed by sandd cgroup
- `browser.rs` improved:
  - EnsureDisplay first for headful Chrome via Xvfb
  - Chrome profile per runtime workspace: `{workspace}/chrome-profile` or `/tmp/chrome-profile-{runtime_id}`
  - Spawn via sandd SpawnBackground with `DISPLAY=... CHROME_PROFILE=... BROWSER_WORKER_PORT_FILE=/tmp/browser-worker-{runtime_id}.port node index.js`
  - Port file per runtime + global fallback, waits 10s, writes global port file for compat
  - Now managed by sandd cgroup (process in runtime cgroup, killed on destroy)
  - Profile isolation per runtime

### 10. Event Bus + SubscribeEvents + Supervisor Desired/Observed
- `events.rs`: broadcast via mpsc Sender, subscribe()
- RPC ListEvents (last 100), SubscribeEvents streaming
- `supervisor.rs`: DesiredState {runtime_id, kind, should_exist, should_running, min_procs}, ObservedState {exists, running, procs, ptys}, set_desired, get_observed, reconcile (every 5s loop), logs actions, destroys if should_exist false
- RPC SetDesiredState, GetObservedState
- Verified: SetDesiredState + GetObservedState works

### 11. Bot -> Session -> Runtime handoff
- `session.rs`: `handoff()`, `handoff_with_messages(keep_last_n)`, `transfer_runtime(new_runtime_id)`
- `api/mod.rs`: Bot struct {id, sessions, workbench_id}, HostAgentApi now has workbench_mgr + bots, methods:
  - create_bot()
  - create_session()
  - create_session_with_runtime(runtime_id, model) for reuse
  - create_session_in_workbench() long-lived
  - handoff_session(old, new_model)
  - ensure_workbench()
- Separation: Bot is long-lived, Session per conversation, Runtime per execution env, supports handoff (same runtime new session)

### 12. Workbench long-lived
- WorkbenchManager ensures workbench runtime exists, finds existing workbench from list, creates if not, ensures display
- host-agent can create session in workbench via create_session_in_workbench

### 13. Tool Registry
- 33 tools: shell.exec (binary), file.read/write/list/search/stat/patch/mkdir/remove/rename/glob (10), terminal.open/write/read/resize/signal/close (6), browser.open/snapshot/click/fill/screenshot/tabs/press (7), computer.screenshot/click/type/move/key/scroll (6), task.create/complete/list (3)

## Test Results

### sandd
- Real PTY: tty /dev/pts/0, TERM xterm-256color, ANSI, resize ioctl+SIGWINCH, Ctrl+C 0x03 -> "^C", Ctrl+D 0x04 EOF, SignalPty SIGINT works
- Binary RPC: Exec raw, WritePty raw, ReadPty raw, Screenshot raw
- DesktopManager: EnsureDisplay starts Xvfb if available
- Supervisor: SetDesiredState/GetObservedState, reconcile every 5s
- SQLite WAL, UDS 0660, SO_PEERCRED

### host-agent
- run uses binary RPC (403 bytes no base64), FS via workspace direct
- Permission: rm -rf / -> permission_required, freezes
- Task complete: ReadyForCheck in SQLite, freezes
- FS tools: mkdir/remove/rename/glob via workspace
- Terminal: open with cols/rows, write raw Ctrl+C/D, resize, signal, close
- Browser: spawn via sandd with display+profile+cgroup, port file per runtime

## Architecture Now

```
Chat/App -> Bot (long-lived, sessions, workbench)
            │
            ▼
      HostAgentApi -> Session (handoff, transfer_runtime) -> Runtime (cgroup, Xvfb, chrome-profile)
            │
            ├─ Loop (freeze on ReadyForCheck/Permission, Attention)
            ├─ Tools (33, binary RPC, workspace direct)
            └─ WorkbenchManager (long-lived)
            │
            ├─ Browser Worker (Playwright, profile per runtime, cgroup managed, Xvfb display)
            └─ sandd
               ├─ Runtime (SQLite WAL)
               ├─ Exec (binary RPC no base64)
               ├─ PTY (real forkpty, TERM xterm-256color, ANSI, resize TIOCSWINSZ+SIGWINCH, raw mode, CtrlC 0x03 CtrlD 0x04, SignalPty)
               ├─ FS (direct workspace + exec)
               ├─ cgroup
               ├─ State (WAL)
               ├─ DesktopManager (Xvfb per runtime)
               ├─ Screenshot (binary)
               ├─ Events (broadcast + SubscribeEvents)
               └─ Supervisor (Desired/Observed reconcile)
```

## Build

```
cargo build -> 0.9s, 33 tools, sandd + binary RPC
```

## Remaining for full Bot MVP

- Browser worker Chrome download fix (TLS), CDP integration, more semantic actions
- Computer use native X11 FFI XShm/XTest (currently xdotool via runtime exec, works with Xvfb)
- Task artifacts freezing + release after approval UI
- Permission UI Workbench approval
- FS search/stat/patch already via workspace, but glob could use direct glob crate
- Event streaming via binary RPC (currently JSON-line, binary already supports)
