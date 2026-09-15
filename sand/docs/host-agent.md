# host-agent - Bot Brain Runtime

## 目标
实现 Bot 的大脑运行时，连接 Conversation -> Agent Session -> Model -> Tool Call -> sandd -> Tool Result -> Model 循环。

```
Chat / App
    │
    ▼
┌─────────────┐
│ host-agent  │
│             │
│ Agent Loop  │
│ Session     │
│ Tools       │
│ Attention   │
└──────┬──────┘
       │
┌──────┴──────────────┐
│                     │
▼                     ▼
Browser Worker      sandd
 Playwright            │
       │               ├─ Runtime
       │               ├─ Exec
       └──── Chrome ◀──┤
                       ├─ PTY (real forkpty)
                       ├─ FS
                       ├─ cgroup
                       └─ Computer (planned)
```

## 已实现 - Phase 3A

### AgentSession
```rust
AgentSession {
    id: sess-xxx,
    runtime_id: rt-xxx,
    model: String,
    messages: Vec<Message>,
    tool_state: HashMap,
    cwd: String,
    status: AgentStatus,
    created_at, updated_at,
    goal: Option<String>,
}
```
关系: Bot -> AgentSession -> Runtime, 而非 Runtime=Agent. 支持同一个 Runtime 多 Session handoff.

### AgentStatus & Attention
```rust
enum AgentStatus {
    Idle, Running, WaitingTool, WaitingInput, ReadyForCheck, Failed, Completed
}
enum Attention {
    WaitingInput, ReadyForCheck, ExecutionError, PermissionRequired, Completed
}
```
用于长时间运行 Bot 何时回来找用户.

### Task
```rust
Task {
    id, goal, session_id, runtime_id,
    status: Pending/Running/WaitingApproval/Completed/Failed,
    artifacts, result
}
```
区分 Task (有结束边界) vs Workbench (长期).

### Tool Protocol - 高层工具与底层 Runtime API 分离

**Agent Tool API** (模型看到的):
- shell.exec {command}
- file.read {path}, file.write {path, content}, file.list {path}, file.search {pattern, path}, file.stat {path}, file.patch {path, patch}
- terminal.open {pty_id, shell}, terminal.write {pty_id, data}, terminal.read {pty_id, clear}
- browser.open {url}, browser.snapshot {}, browser.click {ref}, browser.fill {ref, value}, browser.press {key}, browser.tabs {}, browser.screenshot {}
- computer.screenshot, computer.click {x,y}, computer.type {text}

**sandd Runtime API** (底层):
- CreateRuntime, GetRuntime, ListRuntimes, DestroyRuntime
- Exec, SpawnBackground
- OpenPty, WritePty, ReadPty, ResizePty, ClosePty, ListPtys
- Status

host-agent 负责转换: `shell.exec` -> `runtime_id + sandd.Exec`.

### Model 抽象
```rust
trait Model {
    fn chat(&self, messages: &[Message], tools: &ToolRegistry) -> Result<ModelResponse>
}
```
- MockModel: 用于无 LLM 测试，按预设 responses 返回
- OpenAICompatibleModel: 通过 curl 调用 OpenAI compatible API (OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL), 避免依赖 reqwest, 自行解析 JSON

### ToolRegistry
- 注册所有工具，生成 JSON schema 给 LLM
- execute(name, args, runtime_id) 分发

已实现工具:
- shell.exec: via sand-client exec (bash -lc)
- file.*: via std::fs (后续可改为 via sandd)
- terminal.*: via sandd PTY RPC (real PTY, not pipe)
- browser.*: via browser-worker HTTP (Node+Playwright), fallback 提示
- computer.*: placeholder Phase 3D

### AgentLoop
```text
user message -> LLM -> tool_calls -> serial exec -> tool_results -> LLM -> until final
```
支持 max_iterations, event callback for observability.

### RuntimeManager
包装 sand-client, 创建/销毁 Runtime.

### CLI
```
host-agent run [goal]          # 最小 Bot 循环
host-agent session create
host-agent session list
host-agent tools
```

示例:
```
$ ./target/debug/host-agent run "explore the project structure"
[host-agent] created session sess-xxx runtime rt-xxx
[loop] tool call: file.list {"path":"."}
[loop] tool call: file.read {"path":"Cargo.toml"}
[loop] tool call: shell.exec {"command":"ls -la crates"}
[loop] completed: This project is ...
Final Answer: ...
runtime destroyed
```

## 已修复的 sandd 坑

### 1. Real PTY (previously pipe fallback)
重写 `crates/sandd/src/pty.rs`:
- 使用 `openpty` + `fork` + `setsid` + `ioctl TIOCSCTTY` + `dup2` 实现真实 PTY
- master_fd 通过 `dup` 复制给 reader/writer 线程，避免 double-close (File::from_raw_fd 仅一次)
- Reader 线程读取 master fd，写入 ring buffer (1MB)，支持 `ReadPty` RPC
- Writer 线程通过 mpsc channel 写入 master fd
- Waiter 线程 waitpid 获取 exit_code
- Resize via `ioctl TIOCSWINSZ` + SIGWINCH
- 测试: `tty` 返回 `/dev/pts/0`, ANSI prompt 正常, resize 120x40 正常

### 2. UDS 777 -> 0660
`rpc.rs` 将 socket 权限从 0o777 改为 0o660，尝试 chgrp sand，日志提示 SO_PEERCRED planned.
`get_peer_cred` via `getsockopt SO_PEERCRED` 获取 pid/uid/gid，打印日志，未来可强制 group 检查.

### 3. ReadPty RPC
新增 `ReadPty {id, pty_id, clear}` 返回 `data_b64` 和 len，支持轮询 PTY 输出.

### 4. Base64 已修复
之前 padding 导致 `\0\0`，已修复.

## 待做 - Phase 3B/C/D/E

### Phase 3B: Real PTY + FS + patch
- [x] Real PTY 已完成
- [x] FS tools 已实现 (stat/list/read/write/search/patch)
- [ ] FS via sandd (目前 local FS, 需改为 runtime workspace 隔离)
- [ ] Patch 更健壮 (目前 via patch/git apply)

### Phase 3C: Browser Worker
- 已创建 `browser-worker/` Node + Playwright 项目
- 实现了 HTTP server: /open, /snapshot, /click, /fill, /press, /scroll, /screenshot, /tabs, /close, /health
- snapshot 返回 `[ref] role "name"` 格式，支持 ref 操作
- host-agent browser tools 已改为调用 browser-worker via curl
- 待做: 集成到 sandd cgroup 管理，自动启动，Playwright 安装，更多 semantic actions

### Phase 3D: Computer Use
- Placeholder, 架构已定义: Browser snapshot 优先, 失败 fallback screenshot+vision+mouse
- 待实现 X11 XShm screenshot, XTest mouse/keyboard

### Phase 3E: Task, Attention, Approval, Recovery
- Task struct 已定义，TaskManager 已实现 create/complete/list
- Attention 已定义，LoopEvent 已包含 Attention
- 待做: SQLite WAL 持久化 Session/Task/Runtime/Events, recovery, approval flow, complete_task 冻结结果

## 下一步
- 完善 FS via sandd (让 file.read/write 走 runtime workspace)
- 实现 SQLite WAL 取代 JSON state (rusqlite)
- 将 RPC 从 JSON line 换为 protobuf/tonic 或 binary framed (避免 base64)
- 实现 browser-worker 自动由 sandd 启动并加入 cgroup
- 实现 computer use native
- 实现 Task 完整生命周期 + Attention + 用户交互

## 测试
```
cargo build
./target/debug/sandd &
./target/debug/host-agent run "explore the project"
./target/debug/host-agent tools
```
