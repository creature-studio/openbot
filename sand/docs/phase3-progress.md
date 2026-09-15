# Phase 3 Progress - Host-Agent + Real PTY + SQLite + FS + Browser + Computer

## Summary
From Phase 0-2 (sandd kernel) to Phase 3A-E (Bot brain).

**Before:**
```
sandd: Runtime/Process/Exec/PTY/cgroup (pipe fallback, JSON state, 777)
```

**After:**
```
Chat/App -> host-agent (Session/Loop/Tools/Attention/Task) -> sandd (real PTY, SQLite WAL, 0660, SO_PEERCRED) + browser-worker (Playwright) + computer (X11)
```

## What was done in this continuation

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

### 4. FS via sandd (Phase 3B)
- `file.read/write/list/search/stat/patch` now try sandd exec first using runtime workspace (via GetRuntime workspace + Exec cat/ls/stat)
- Fallback to local FS
- `file.patch` supports:
  - search/replace mode: {path, search, replace}
  - unified diff mode: {path, patch} via patch -p0/-p1 or git apply
- Audit/diff friendly vs shell heredoc

### 5. Permission + Attention
- `AgentContext` has Allow/Ask/Deny map for shell/file/browser/secret/external/destructive
- `shell.exec` detects destructive `rm -rf /`, `mkfs`, `dd`, `chmod 777 /` -> returns `permission_required` which triggers `Attention::PermissionRequired` in loop
- Loop checks result contains `permission_required` and emits Attention event

### 6. Computer Use (Phase 3D)
- `computer.screenshot/click/type/move/key/scroll` implemented with:
  - Try `import -window root` (ImageMagick) and `scrot` for screenshot if DISPLAY set
  - Try `xdotool` for click/type
  - Else placeholder with architecture doc: Browser snapshot -> semantic action -> screenshot+vision+mouse fallback
- Documents execution strategy

### 7. Browser Worker (Phase 3C)
- `browser-worker/` Node + Playwright (package.json, playwright 1.48, but chromium download fails due to TLS, fallback to mock)
- HTTP server: /open, /snapshot (accessibility tree -> `[ref] role "name"`), /click, /fill, /press, /scroll, /screenshot (base64), /tabs, /close, /health
- Port discovery via `/tmp/browser-worker.port` file + env `BROWSER_WORKER_PORT`
- host-agent browser tools call worker via curl, auto-spawn via sandd SpawnBackground if not running (try_spawn_browser_worker)
- Snapshot design key: semantic refs, not selectors, for model efficiency

### 8. Task + Attention + Recovery (Phase 3E)
- `Task` struct + `TaskManager` with create/complete/list, artifacts/result
- Tools: `task.create {goal}`, `task.complete {result}`, `task.list`
- `task.complete` marks ReadyForCheck, runtime kept until user approval (vs immediate destroy)
- Session persistence via SQLite, survives sandd restart
- EventBus + persistence push_event for future streaming

### 9. Tool Registry Separation
- Agent Tool API vs sandd Runtime API clearly separated
- `default_tool_registry()` registers all high-level tools
- Model sees only high-level tools, not low-level runtime_id/cgroup details (except runtime_id injected by host-agent)

## Test Results

### sandd still passes all previous tests
- Test1 A/B isolation, Test2 10 sleep cgroup, Test3 B unaffected, Test4 PTY 3x, Test5 disconnect, Test6 restart recovery, Test7 concurrent, Test8 cgroup.kill all OK
- New: Real PTY tty -> /dev/pts/0, SQLite WAL with .db-wal file, UDS 0660

### host-agent Phase 3A minimal bot
```
./target/debug/host-agent run "explore project"
- Creates session + runtime
- Mock model: file.list -> file.read -> shell.exec -> final answer
- FS via runtime (shows via runtime rt-xxx)
- Session saved to SQLite, survives runtime destroy
- Runtime destroyed after
```
Verified.

### Tools list
```
shell.exec, file.read/write/list/search/stat/patch, terminal.open/write/read, browser.open/snapshot/click/fill/press/tabs/screenshot, computer.screenshot/click/type/move/key/scroll, task.create/complete/list
```

## Remaining for full Bot MVP

- FS via sandd: currently uses exec cat/ls, should use more efficient direct FS with runtime workspace path (already gets workspace via GetRuntime, but exec is okay)
- SQLite: currently only runtime and session saved, need process/pty/event streaming + task artifacts
- RPC binary: still JSON line + base64, should move to protobuf/tonic or binary framed for PTY/screenshot/file (future)
- Browser worker: needs auto-managed by sandd cgroup, Playwright browser download fix (TLS), more semantic actions, integration with Chrome profile / CDP
- Computer use: needs real X11 env (Xvfb + X11 FFI XShm/XTest) for screenshot/click, currently placeholder
- Desktop manager: Xvfb/xfwm4/VNC not yet in sandd
- Supervisor: DesiredState/ObservedState reconcile not yet
- Permission enforcement: currently logs + returns permission_required for destructive, but needs full allow/ask/deny flow with user approval API
- Event streaming: currently VecDeque, needs broadcast channel + SubscribeEvents RPC
- Complete_task: should freeze artifacts and release runtime after approval, not immediate destroy

## Architecture Now

```
                     Chat / App
                         │
                         ▼
                  ┌─────────────┐
                  │ host-agent  │
                  │             │
                  │ Agent Loop  │
                  │ Session     │  SQLite WAL
                  │ Tools       │  Task + Attention
                  │ Permission  │
                  └──────┬──────┘
                         │
             ┌───────────┴─────────────┐
             │                         │
             ▼                         ▼
      Browser Worker                 sandd
       Playwright                      │
             │                         ├─ Runtime (SQLite WAL)
             │                         ├─ Exec
             └──────── Chrome ◀────────┤
                                       ├─ PTY (real forkpty)
                                       ├─ FS (via runtime)
                                       ├─ cgroup (auto subtree_control)
                                       ├─ State (WAL)
                                       └─ Computer (X11 placeholder)
```

## Build

```
cargo build
# sandd + sand + host-agent + sand-cli + sand-init
# Finished dev profile 0.6s
```

## Next Steps

- Implement binary RPC (tonic/protobuf) for PTY/screenshot streaming
- Implement DesktopManager in sandd (Xvfb, xfwm4, VNC)
- Implement browser-worker as sandd managed process with cgroup + lifecycle
- Implement computer use native with rustix X11
- Implement Task approval flow + complete_task freezing
- Implement Permission UI + Attention -> user
- Add more FS tools via sandd direct (not exec)
