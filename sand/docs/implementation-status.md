# sandd + host-agent Implementation Status - Phase0+1+2+3A

## Overview
- **sandd**: Rust Runtime Kernel as Assistant Computer native Runtime Kernel. Single daemon managing multiple Runtimes, Processes, cgroups, Exec, PTY, State, Events.
- **host-agent**: Bot Brain Runtime, Agent Session + Loop + Tool Registry + Model abstraction.

Binary locations:
- `target/debug/sandd` - daemon
- `target/debug/sand` - CLI
- `target/debug/host-agent` - bot brain
- `browser-worker/src/index.js` - Node + Playwright browser worker

Socket: `/run/sand/sandd.sock` (fallback `/tmp/sandd/sandd.sock`), mode 0660 (fixed from 777, SO_PEERCRED logging)
State: `/run/sand/state.json` (fallback `/tmp/sandd/state.json`) - JSON lines (SQLite WAL planned)
Cgroup root: `/sys/fs/cgroup/sand` (fallback `/tmp/sand-cgroups`)

## Architecture Implemented

### RuntimeManager (runtime.rs)
- HashMap<RuntimeId, Runtime>
- Create: generates `rt-<hex>` id, workspace `/tmp/sand-runtime-<id>` (or `/workspace/...` if exists), cgroup creation, state save
- Get/List: returns id, kind, state, procs, ptys
- Destroy: PTY cleanup, cgroup kill via cgroup.kill + procs kill -9, rmdir via sudo, workspace removal, state save
- Recover: loads state.json, checks cgroup existence and live pids, marks Running if cgroup has procs else Stopped
- State machine: Creating -> Starting -> Running -> Stopping -> Stopped, Failed/Killed/Oom/Crashed branches reserved

### CgroupManager (cgroup.rs) - Fixed
- Root detection: tries `/sys/fs/cgroup/sand`, auto-creates via sudo if missing
- Auto-enable controllers: on new(), ensures `cgroup.subtree_control` contains `+cpu +memory +pids +io` via direct write or sudo sh -c
- Probes if real cgroup writable, else fallback to `/tmp/sand-cgroups`
- Methods:
  - `runtime_cgroup_path(id)` returns real or fallback path (no mkdir side-effect)
  - `create_cgroup(id)`: mkdir or sudo mkdir, chmod 755, ensures subtree_control
  - `destroy_cgroup(id)`: kill_cgroup first, sleep 200ms, then `fs::remove_dir` (rmdir, not rm -rf, because cgroup interface files cannot be unlinked), fallback sudo rmdir, retry kill+rmdir
  - `kill_cgroup(id)`: tries `cgroup.kill` write 1 (direct or sudo), sleep 200ms, fallback kills all pids in `cgroup.procs` via kill -9 (direct read or sudo cat), also fallback pids file
  - `add_pid(id, pid)`: writes pid to cgroup.procs (direct or sudo), also tracks in fallback pids file
  - `list_pids(id)`: reads cgroup.procs (direct or sudo cat), merges fallback pids, filters alive via /proc/<pid> exists
  - `read_stats(id)`: cpu.stat usage_usec, memory.current, memory.peak, pids.current, memory.events oom_kill, with sudo fallback
- Fixes applied:
  - Previously used `remove_dir_all` (rm -rf) which fails on cgroup (Operation not permitted for interface files). Changed to `remove_dir` (rmdir)
  - Previously didn't auto-enable subtree_control, causing child cgroups without cpu.stat/memory.* etc. Now auto-enables in new()
  - Uses sudo for operations where direct write fails (cgroup ownership root)

### ProcessManager (process.rs)
- `exec_robust`: spawns process with pre_exec hook writing pid to cgroup.procs (via cgroup_manager.add_pid), captures stdout/stderr via threads, timeout via poll, kill on timeout, returns ExecResult {pid, exit_code, stdout, stderr, duration_ms}
- `spawn_background`: similar but detaches, thread waits for child, stores pid via cgroup, returns pid immediately
- Both use Command with current_dir = runtime workspace

### PtyManager (pty.rs) - Real PTY fixed
- **Fixed from pipe fallback to real PTY**: Uses `openpty` + `fork` + `setsid` + `ioctl TIOCSCTTY` + `dup2` + `execvp`
- Handles fd ownership correctly: `dup` master fd for reader/writer threads, `File::from_raw_fd` only once per dup, original master kept for ioctl/close
- Reader thread: reads master fd, appends to ring buffer (1MB), supports `ReadPty` RPC
- Writer thread: mpsc channel -> master fd
- Waiter thread: `waitpid` to get exit_code
- Resize: `ioctl TIOCSWINSZ` + SIGWINCH
- Verified: `tty` returns `/dev/pts/0`, ANSI prompt, interactive shell, resize 120x40 works
- Fallback: if openpty fails, falls back to pipe (for restricted envs)
- Strategy: PTY continues alive after client disconnect, must be explicitly closed or runtime destroyed

### StateManager (state.rs)
- JSON lines file, not yet SQLite WAL (planned)
- Save: writes one JSON per line {id, kind, state, workspace, cgroup, created, started, caps, procs, ptys}
- Load: naive parsing, extracts fields via string search
- Recover: used by RuntimeManager to rebuild HashMap

### EventBus (events.rs)
- VecDeque with cap 1000, push/list/list_for
- Events: RuntimeCreated, Started, ProcessStarted/Exited, PtyOpened/Closed, BrowserStarted/Exited, Failed, Destroyed, Oom

### RPC (rpc.rs) - UDS JSON lines, now 0660 + SO_PEERCRED
- UnixListener bind to sock_path, chmod 0660 (fixed from 777), chgrp sand if exists, per-client thread
- SO_PEERCRED: `getsockopt SO_PEERCRED` logs pid/uid/gid, future enforcement
- Protocol: one JSON per line, method field dispatch
- Methods:
  - CreateRuntime {kind, workspace} -> {ok, id, kind, workspace}
  - GetRuntime {id} -> {ok, id, kind, state, workspace, procs, ptys}
  - ListRuntimes -> {ok, runtimes: [{id, kind, state, procs, ptys}]}
  - DestroyRuntime {id} -> {ok}
  - Exec {id, command: [array or string], cwd, timeout_ms} -> {ok, pid, exit_code, stdout_b64, stderr_b64, duration_ms}
  - SpawnBackground {id, command: array} -> {ok, pid}
  - OpenPty {id, pty_id, cols, rows, shell} -> {ok, runtime_id, pty_id, pid}
  - WritePty {id, pty_id, data_b64} -> {ok}
  - ReadPty {id, pty_id, clear} -> {ok, data_b64, len} (NEW, supports real PTY output polling)
  - ResizePty {id, pty_id, cols, rows} -> {ok}
  - ClosePty {id, pty_id} -> {ok}
  - ListPtys {id} -> {ok, ptys: [{pty_id, pid, cols, rows}]}
  - Status -> {ok, runtime_count, version}
- Base64: custom encode/decode without external crate, fixed padding bug
- Command parsing: handles both string and array forms

### Client (sand-client)
- UDS client, connects to /run/sand/sandd.sock or /tmp/sandd/sandd.sock
- Methods: create_runtime, get_runtime, list_runtimes, destroy_runtime, exec, spawn_background, open_pty, etc., all via JSON line + socat-like

### CLI (sand-cli)
- Commands: status, runtime list/create/inspect/destroy/exec/ps/kill, pty list/open/close/resize
- Exec prints raw JSON plus decoded stdout/stderr (fixed base64 padding bug)

### Init (sand-init)
- Ensures /sys/fs/cgroup/sand, /run/sand, /tmp/sandd exist via sudo mkdir -p

## Test Results - All Passed

### Test1: Runtime A/B independent
- A=rt-65c1e2a29994 B=rt-d31ae2b962c1
- `sand runtime exec A pwd` returns `/tmp/sand-runtime-rt-65c1e2a29994`, B returns its own workspace
- `echo hello` in A, `echo world` in B isolated
- Result: OK

### Test2: spawn 10 sleep 100 in A
- Spawned via SpawnBackground RPC: 10x sleep 100
- cgroup `/sys/fs/cgroup/sand/runtime-<A>/cgroup.procs` contains 10 pids
- `pids.current` = 10
- `ps aux | grep sleep 100` = 10
- Result: OK

### Test3: Runtime B not affected by A destroy
- Before destroy: list shows A (10 procs) and B (0 procs)
- After `destroy A`: list only B, A gone
- cgroup A dir gone (verified via rmdir, not rm -rf)
- ps sleep 100 = 0 (killed via cgroup.kill + kill -9)
- B cgroup still exists, pids.current 0
- Result: OK

### Test4: PTY open 3 in one runtime
- `pty open B term-1/2/3` -> 3 sessions, pids 3559,3566,3573
- `pty list B` shows 3
- Resize term-1 to 120x40 -> list shows 120
- WritePty base64 "echo hello from pty" -> ok
- Result: OK

### Test5: PTY client disconnect behavior
- Strategy: PTY continues alive after client disconnect, must be explicitly closed or runtime destroyed
- Verification: each `sand pty open` uses separate UDS connection (CLI opens, sends request, disconnects). After disconnect, `pty list` still shows PTY alive
- Result: Documented, OK

### Test6: sandd restart, runtime state re-read
- Create C=rt-57ce8588aa2b, spawn sleep 200, ps shows 1, cgroup.procs contains pid
- Kill sandd, restart sandd, list shows C still present (recovered from state.json)
- sleep 200 survived restart (orphan handling via cgroup still holds pid)
- cgroup still exists, procs contains pid
- Destroy C and B, sleep cleaned, no runtime cgroups left
- Result: OK (state recovery works, cgroup orphan handling works)

### Test7: UDS RPC concurrent
- Spawn 20 parallel `sand status` clients
- All 20 returned `{"ok":true,"runtime_count":0,"version":"0.1.0"}`
- No deadlock, per-client thread model works
- Result: OK

### Test8: kill_all via cgroup.kill
- Create D, spawn 5x sleep 100, cgroup.procs 5 pids
- `sudo sh -c "echo 1 > /sys/fs/cgroup/sand/runtime-D/cgroup.kill"` -> kills all
- After kill, cgroup.procs empty, ps sleep 100 = 0
- Destroy D cleans cgroup
- Result: OK

### Additional fixes verified
- Base64 padding: `pwd` exec previously decoded with trailing \0\0, now correct `/tmp/sand-runtime-...`
- Subtree_control auto-enable: CgroupManager::new now writes `+cpu +memory +pids +io` to `/sys/fs/cgroup/sand/cgroup.subtree_control` via sudo, ensuring child cgroups have cpu.stat, memory.current, pids.current, etc.
- Destroy via rmdir: previously used rm -rf which fails on cgroup (Operation not permitted for interface files), now uses rmdir via fs::remove_dir and sudo rmdir

## Known Limitations / TODO for next phases

- State is JSON lines, not SQLite WAL (planned, needed for Session/Task/Events recovery)
- PTY now real forkpty ✅, but still needs more edge tests: vim/top/codex/claude/python REPL
- No pidfd yet (uses /proc/<pid> exists check, PID reuse risk)
- No protobuf/tonic yet (uses JSON lines over UDS, base64 for binary - should be binary framed)
- No HTTP 1337/1338 compatibility layer yet (Phase3 compatibility)
- Desktop/Computer/Browser: BrowserWorker Node+Playwright implemented as HTTP server, host-agent tools call it via curl, but not yet managed by sandd cgroup auto-spawn
- No Supervisor DesiredState/ObservedState reconcile yet (Phase7)
- Capability: permission map exists, SO_PEERCRED logging added, but not enforced yet (0660 fixed)
- No structured tracing metrics yet (uses eprintln)
- No event streaming via broadcast (VecDeque only)

## New: host-agent Phase 3A Implemented

### AgentSession, Status, Attention, Task
- AgentSession id/runtime_id/model/messages/tool_state/cwd/status/goal
- AgentStatus Idle/Running/WaitingTool/WaitingInput/ReadyForCheck/Failed/Completed
- Attention WaitingInput/ReadyForCheck/ExecutionError/PermissionRequired/Completed
- Task id/goal/session_id/runtime_id/status/artifacts/result, TaskManager

### Tool Protocol Separation
- Agent Tool API: shell.exec, file.read/write/list/search/stat/patch, terminal.open/write/read, browser.open/snapshot/click/fill/press/tabs/screenshot, computer.screenshot/click/type
- sandd Runtime API: CreateRuntime/Exec/OpenPty etc.
- host-agent converts high-level to low-level

### Model Abstraction
- MockModel for testing, OpenAICompatibleModel via curl (OPENAI_API_KEY)

### Loop
- user message -> LLM -> tool_calls -> exec -> tool_results -> LLM -> final
- Event callback for observability

### Browser Worker
- Node + Playwright, HTTP server with /open, /snapshot (returns [ref] role "name"), /click, /fill, /press, /screenshot, /tabs
- Snapshot design key for Bot efficiency: semantic refs, not raw selectors

### Verification
- `host-agent run "explore project"` works with mock model: file.list, file.read, shell.exec, final answer
- Real PTY verified: tty -> /dev/pts/0
- UDS 0660 + SO_PEERCRED logging
- All previous sandd tests still pass

## Build

```
cargo build
# Finished dev profile 1.30s, 16 warnings, binaries target/debug/sandd and sand
```

## Running

```
sudo mkdir -p /sys/fs/cgroup/sand /run/sand /tmp/sandd
sudo chmod 777 /run/sand /sys/fs/cgroup/sand
sudo sh -c 'echo "+cpu +memory +pids +io" > /sys/fs/cgroup/sand/cgroup.subtree_control'
./target/debug/sandd > /tmp/sandd.log 2>&1 &
./target/debug/sand status
./target/debug/sand runtime create assistant
./target/debug/sand runtime exec <id> pwd
./target/debug/sand runtime destroy <id>
```

## Conclusion
Phase0+1+2 acceptance criteria all passed. Core runtime isolation via cgroup v2, process management, exec, PTY multiplexing, UDS RPC, state recovery, orphan cleanup via cgroup.kill all working.
