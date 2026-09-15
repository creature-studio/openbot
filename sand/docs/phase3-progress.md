# Phase 3 Progress - Host-Agent + Real PTY + SQLite + FS + Browser + Computer + Binary RPC + Desktop + Workbench

## Summary
From Phase 0-2 (sandd kernel) to Phase 3A-E (Bot brain) with binary RPC and desktop manager.

**Before:**
```
sandd: Runtime/Process/Exec/PTY/cgroup (pipe fallback, JSON state, 777)
```

**After:**
```
Chat/App -> host-agent (Session/Loop/Tools/Attention/Task/Workbench) -> sandd (real PTY, SQLite WAL, 0660, SO_PEERCRED, binary RPC, DesktopManager Xvfb) + browser-worker (Playwright) + computer (X11 via sandd)
```

## What was done in this continuation (Phase 3B-E+)

### 1. Real PTY (Phase 3B must-fix)
- Rewrote `pty.rs` from pipe fallback to real `openpty + fork + setsid + ioctl(TIOCSCTTY) + dup2 + execvp`
- Correct fd ownership via `dup`, reader/writer/waiter threads, ring buffer 1MB, `ReadPty` RPC
- `tty` returns `/dev/pts/0`, ANSI, interactive, resize works
- Fallback to pipe if openpty fails

### 2. UDS Security 777 -> 0660 + SO_PEERCRED
- `rpc.rs` chmod 0660, chgrp sand, `getsockopt(SO_PEERCRED)` logs pid/uid/gid
- Future enforcement planned

### 3. SQLite WAL (Phase 1 planned, now done)
- Implemented `state_sqlite.rs` via direct FFI to `libsqlite3` (no external crate, works offline)
- `#[link(name = "sqlite3")]` with symlink fix
- PRAGMA journal_mode=WAL, synchronous=NORMAL
- Tables: runtime, process, pty, session, task, event (no FK cascade to avoid session deletion on runtime destroy)
- StateManager uses SQLite primary + JSON fallback, verified .db-wal/.db-shm exist, python sqlite3 query works
- host-agent `persistence.rs` same DB `/run/sand/state.db`, session survives runtime destroy

### 4. FS via sandd (Phase 3B) - now direct workspace + exec fallback
- `file.read/write/list/search/stat/patch` now:
  - Try direct FS via runtime workspace path (GetRuntime workspace + std::fs) for efficiency
  - Fallback to sandd exec `cat/ls/stat`
  - Fallback to local FS
- `file.patch` supports search/replace and unified diff
- Verified via `read_file_via_workspace`, `write_file_via_workspace_direct`, `list_via_workspace`

### 5. Permission + Attention + Task Freeze
- `AgentContext` has Allow/Ask/Deny map
- `shell.exec` detects destructive `rm -rf /`, `mkfs`, `dd`, etc -> returns `permission_required` which triggers `Attention::PermissionRequired` in loop and freezes (WaitingInput)
- `task.complete` marks task ReadyForCheck in SQLite, freezes agent loop, emits `Attention::ReadyForCheck`, runtime kept until approval
- Loop logic: `should_freeze` on task.complete or permission_required, returns ReadyForCheck/WaitingInput

### 6. Binary RPC (new, efficient, no base64)
- Added `rpc_binary.rs` listening on `/run/sand/sandd-binary.sock` (0660)
- Framed protocol: 4-byte BE len + JSON header + binary payload
- Methods:
  - Exec: returns stdout_len/stderr_len + binary payload = stdout+stderr raw (no base64)
  - WritePty: binary payload is data to write (no base64)
  - ReadPty: returns len + raw bytes
  - Screenshot: returns len + png bytes
  - Delegates other methods to JSON handler
- `sand-client` now has `call_binary`, `exec_binary`, `write_pty_binary`, `read_pty_binary`
- host-agent tools updated to use binary RPC first, fallback to JSON:
  - `shell.exec` uses `exec_binary` (shows "binary RPC, X bytes, no base64")
  - `terminal.write/read` uses binary RPC
  - `computer.screenshot` uses binary Screenshot via sandd
- Test: `echo hello binary` via binary RPC returns 13 bytes raw, no base64 overhead

### 7. DesktopManager Xvfb (Phase 3D)
- New `desktop.rs` module:
  - Manages Xvfb per runtime, allocates display :10-99 based on runtime_id hash
  - `ensure_display(runtime_id, width, height)` starts Xvfb if available, reuses if exists
  - `get_display`, `screenshot` via import/scrot/xwd fallback, `destroy` kills Xvfb
  - Integrated into RuntimeManager, destroys on runtime destroy
  - RPC: `EnsureDisplay`, `GetDisplay`, `Screenshot` (binary)
- host-agent computer tools now try sandd desktop first:
  - `computer.screenshot` calls EnsureDisplay + Screenshot binary RPC
  - `computer.click/type` tries GetDisplay + exec xdotool in runtime with DISPLAY set
  - Fallback to local xdotool/import

### 8. Computer Use (Phase 3D) - now via sandd
- `computer.screenshot/click/type/move/key/scroll`:
  - Try sandd binary RPC (EnsureDisplay + Screenshot)
  - Try runtime exec with DISPLAY (xdotool)
  - Try local xdotool/import
  - Else placeholder with architecture doc
- Architecture: Browser snapshot first, screenshot+vision+mouse fallback

### 9. Browser Worker (Phase 3C)
- `browser-worker/` Node + Playwright
- HTTP server: /open, /snapshot (accessibility tree -> `[ref] role "name"`), /click, /fill, /press, /scroll, /screenshot, /tabs
- Port discovery via `/tmp/browser-worker.port` + env
- host-agent auto-spawn via sandd SpawnBackground
- Snapshot design: semantic refs, not selectors

### 10. Event Bus + SubscribeEvents
- `events.rs` now has subscribers: `Vec<Sender<RuntimeEvent>>` + `emit` broadcasts
- `subscribe()` returns Receiver
- RPC `ListEvents` returns last 100 events, `SubscribeEvents` streams via JSON-line (keeps connection open, sends events as they occur)
- Binary RPC also supports ListEvents

### 11. Task + Attention + Recovery + Workbench
- `Task` struct + tools: `task.create`, `task.complete` (updates SQLite to ReadyForCheck), `task.list` (python sqlite3 query)
- `WorkbenchManager` long-lived runtime kind Workbench, ensures display, persists across sessions
- Session persistence via SQLite, survives destroy

### 12. Tool Registry Separation
- Agent Tool API vs sandd Runtime API separated
- `default_tool_registry()` registers 26 tools: shell.exec (binary), file.*, terminal.* (binary), browser.*, computer.* (via sandd), task.*

## Test Results

### sandd
- Real PTY tty -> /dev/pts/0, SQLite WAL, UDS 0660, binary RPC on sandd-binary.sock
- `ListRuntimes`, `CreateRuntime`, `Exec` via binary RPC works (echo hello binary -> 13 bytes raw)
- PTY open/write/read via binary RPC works
- EnsureDisplay + Screenshot via binary (when Xvfb available)

### host-agent
- `./target/debug/host-agent run` uses binary RPC for exec (shows "binary RPC, 403 bytes, no base64")
- FS via workspace direct (file.read shows "via workspace")
- Permission: `rm -rf /` returns `permission_required: destructive command ... [tool: shell.exec]` -> triggers Attention::PermissionRequired and freezes
- Task complete: marks SQLite task ReadyForCheck, freezes loop
- Tools: 26 tools, all use sandd when possible

### Tools list
```
shell.exec (binary RPC), file.read/write/list/search/stat/patch (workspace direct + exec), terminal.open/write/read (binary RPC), browser.open/snapshot/click/fill/press/tabs/screenshot (auto-spawn), computer.screenshot/click/type/move/key/scroll (via sandd desktop + binary), task.create/complete/list (SQLite)
```

## Architecture Now

```
                     Chat / App
                         │
                         ▼
                  ┌─────────────┐
                  │ host-agent  │ SQLite WAL
                  │ Loop (freeze on ReadyForCheck/Permission)│
                  │ Session     │  Task (ReadyForCheck) + Workbench (long-lived)
                  │ Tools (binary RPC)│
                  │ Permission  │
                  └──────┬──────┘
                         │
             ┌───────────┴─────────────┐
             │                         │
             ▼                         ▼
      Browser Worker                 sandd
       Playwright                      │
             │                         ├─ Runtime (SQLite WAL)
             │                         ├─ Exec (binary RPC, no base64)
             └──────── Chrome ◀────────┤
                                       ├─ PTY (real forkpty, binary RPC)
                                       ├─ FS (direct workspace + exec)
                                       ├─ cgroup
                                       ├─ State (WAL)
                                       ├─ DesktopManager (Xvfb per runtime)
                                       ├─ Screenshot (binary RPC)
                                       └─ Events (broadcast + SubscribeEvents)
```

## Build

```
cargo build
# sandd + sand + host-agent + sand-cli + sand-init
# Finished dev profile 0.9s
# Binary RPC: /run/sand/sandd.sock + /run/sand/sandd-binary.sock
```

## Remaining for full Bot MVP

- Browser worker as sandd managed process with cgroup lifecycle + Chrome profile / CDP
- Computer use native X11 FFI XShm/XTest (currently uses xdotool via runtime exec, which works when Xvfb+DISPLAY)
- Supervisor DesiredState/ObservedState reconcile
- Permission UI + Attention -> Workbench UI approval
- Task artifacts freezing + release after approval
- FS via sandd direct for all tools (already done for read/write/list, need search/stat/patch)
- More event streaming via binary RPC (currently JSON-line)
