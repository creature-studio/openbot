# Runtime 目标架构 (sandd)

## 1. 一句话目标

```
host-main / host-agent 负责表达“我要做什么”

sandd 负责 怎么启动、执行、管理进程、PTY、Runtime、隔离、回收、观察、驱动桌面、管理资源
```

## 2. 整体结构

```
                   host-main / host-agent
                           │
                           │ RPC (UDS + protobuf, 兼容 HTTP 1337)
                           ▼
                /run/sand/sandd.sock
                           │
                           ▼
┌─────────────────────────────────────────┐
│                 sandd                   │
│                                         │
│ Runtime Manager                         │
│ Process / cgroup Manager                │
│ Exec                                    │
│ PTY Broker                              │
│ Filesystem                              │
│ Desktop Manager                         │
│ Computer Use                            │
│ Browser Provider                        │
│ State / Event                           │
│ Network / Egress                        │
└─────────────────────────────────────────┘
          │
          ├─ Runtime A (cgroup /sys/fs/cgroup/sand/runtime-A)
          │    ├─ shell processes
          │    ├─ PTY sessions
          │    ├─ Chrome (optional, via provider)
          │    └─ browser-worker
          ├─ Runtime B
          ├─ Runtime C
          │
          ├─ Shared Desktop (Xvfb :1) or per-Runtime Desktop
          ├─ Chrome (shared or per-runtime)
          ├─ shell
          ├─ PTY broker (single, multiplexed)
          └─ browser-worker (Playwright sidecar)
```

核心原则: Agent 数量增长，不应导致 exec-daemon 数量线性增长. 最终 14000+, 136xx 端口消失.

## 3. Runtime 一等公民

```rust
struct Runtime {
    id: RuntimeId, // uuid, e.g. "rt-abc123"
    kind: RuntimeKind,
    state: RuntimeState,
    processes: ProcessGroup, // cgroup + pidfd set
    workspace: Workspace, // /workspace/runtime-<id> or /home/box/sand-data/runtime-<id>
    ptys: HashMap<PtyId, PtySession>,
    desktop: Option<DesktopHandle>,
    browser: Option<BrowserHandle>,
    capabilities: CapabilitySet,
    created_at: SystemTime,
    started_at: Option<SystemTime>,
}

enum RuntimeKind {
    Assistant, // 对应现有 Agent
    Task,      // 一次性任务
    Workbench, // 长期工作区
    Eval,      // 评测
}

enum RuntimeState {
    Creating,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed { reason: String },
    Killed,
    Oom,
    Crashed { exit_code: Option<i32>, signal: Option<i32> },
}

struct CapabilitySet {
    // e.g. runtime:A:exec, runtime:A:pty, runtime:A:computer, runtime:A:browser
    // 使用 HashSet<String> 或 bitflags
}
```

Runtime ≠ Agent. Runtime 是本机提供给某个 Agent/Task/Workbench 的可隔离执行环境.

## 4. Runtime 生命周期状态机

```
Creating
   ↓ (allocate cgroup, workspace, state DB)
Starting
   ↓ (start init process, desktop if needed)
Running
   ↓ (正常运行)
Stopping
   ↓ (SIGTERM -> cgroup.kill -> cleanup)
Stopped

异常分支:
Running -> Failed (exec 失败, 内部错误)
Running -> Killed (收到 Kill)
Running -> Oom (memory.events oom_kill)
Running -> Crashed (init 进程退出非0)

任何状态 -> Destroying -> Destroyed (资源释放)
```

API:

```
CreateRuntime { kind, workspace, capabilities, desktop: Option<DesktopSpec> } -> RuntimeId
GetRuntime { id } -> Runtime
ListRuntimes { filter } -> Vec<Runtime>
StartRuntime { id }
StopRuntime { id, timeout }
DestroyRuntime { id } // 必须清理 shell, child, PTY, Chrome, browser-worker, desktop child, cgroup.procs=0
```

Destroy 验收: cgroup.procs 为空, PTY 0, Chrome 0, browser worker 0, 临时资源释放.

## 5. Process Manager: cgroup v2 + pidfd

每个 Runtime 独立 cgroup:

```
/sys/fs/cgroup/sand/
    runtime-<id>/
        cgroup.procs
        cgroup.threads
        cgroup.kill (新内核)
        cpu.stat, cpu.max, cpu.weight
        memory.current, memory.peak, memory.events, memory.max
        pids.current, pids.max
        io.stat
```

实现要点:

- 使用 rustix 处理 Linux syscall, 避免大量 shell command
- 创建 cgroup: mkdir /sys/fs/cgroup/sand/runtime-<id>, 若无权限则 fallback 到 /tmp/sand-cgroups/runtime-<id> (用于测试) 或使用 systemd-run? 但优先真实 cgroup
- 所有 Runtime 子进程必须通过 `echo <pid> > cgroup.procs` 加入对应 cgroup, 在 fork 后 exec 前执行
- 读取资源: CPU usage (cpu.stat usage_usec), Memory usage (memory.current), peak (memory.peak), PIDs (pids.current), IO (io.stat), OOM (memory.events oom_kill)
- 销毁: 优先 cgroup.kill (echo 1 > cgroup.kill) 完成整组清理, 若内核不支持则遍历 cgroup.procs kill -9
- pidfd: 使用 pidfd_open (rustix) 获取 pidfd, 避免 PID reuse, 并可 poll 进程退出
- 防止: PID reuse, 父死子活, Chrome orphan, shell orphan
- 集成: 监听 /proc/<pid>/cgroup 确认归属, 定期扫描 /proc 修复 ObservedState

## 6. Exec

```rust
async fn exec(runtime_id: RuntimeId, req: ExecRequest) -> ExecStream

struct ExecRequest {
    command: Vec<String>, // e.g. ["bash", "-lc", "pwd"]
    cwd: Option<PathBuf>,
    env: HashMap<String, String>,
    timeout: Option<Duration>,
    stdin: Option<Vec<u8>>,
}

enum ExecEvent {
    Started { pid: i32, pidfd: PidFd },
    Stdout { data: Vec<u8> },
    Stderr { data: Vec<u8> },
    Exit { code: Option<i32>, signal: Option<i32> },
}
```

- 所有 exec 进程自动进入 Runtime cgroup
- 支持 streaming output via tokio broadcast or mpsc
- 支持 cwd, env, timeout, stdin, stdout, stderr, exit code, signal
- 错误 typed: ExecError::RuntimeNotFound, CgroupFailed, SpawnFailed, Timeout

## 7. PTY Broker (multiplexed)

目标: 一个 sandd, 一个 PTY broker, N runtime, M PTY, 不再每个 Agent 一个 WS 端口

```
runtime_id + pty_id 逻辑标识, 例如 runtime=A, pty=terminal-1
```

协议:

```
OPEN { runtime_id, pty_id, cols, rows, cwd, shell, env }
DATA { runtime_id, pty_id, data: bytes }
RESIZE { runtime_id, pty_id, cols, rows }
SIGNAL { runtime_id, pty_id, signal }
CLOSE { runtime_id, pty_id }
EXIT { runtime_id, pty_id, code, signal }
```

实现:

- 使用 portable-pty 或 nix + rustix 手动创建 PTY (openpty)
- 每个 PTY 一个 tokio task 管理 shell 进程, 通过 mpsc 传递 input/output
- 存储: HashMap<(RuntimeId, PtyId), PtySession>
- 客户端通过 UDS RPC OpenPty(stream PtyMessage) returns (stream PtyMessage) 订阅
- 断开策略: 明确定义, 例如 PTY 继续存活, 客户端可重连, 除非显式 Close 或 Runtime Destroy
- 测试: 同一 Runtime 打开 PTY 1/2/3, 正常 IO, resize, Ctrl+C, disconnect/reconnect

## 8. RPC

本机内部优先 Unix Domain Socket:

```
/run/sand/sandd.sock
```

避免 localhost TCP 端口大量使用

协议优先 protobuf + tonic

```proto
syntax = "proto3";
package sand;

service SandRuntime {
    rpc CreateRuntime(CreateRuntimeRequest) returns (CreateRuntimeResponse);
    rpc GetRuntime(GetRuntimeRequest) returns (GetRuntimeResponse);
    rpc ListRuntimes(ListRuntimesRequest) returns (ListRuntimesResponse);
    rpc DestroyRuntime(DestroyRuntimeRequest) returns (DestroyRuntimeResponse);
    rpc StartRuntime(StartRuntimeRequest) returns (StartRuntimeResponse);
    rpc StopRuntime(StopRuntimeRequest) returns (StopRuntimeResponse);

    rpc Exec(ExecRequest) returns (stream ExecEvent);

    rpc OpenPty(stream PtyMessage) returns (stream PtyMessage);

    rpc Screenshot(ScreenshotRequest) returns (ScreenshotResponse);
    rpc ComputerAction(ComputerActionRequest) returns (ComputerActionResponse);

    rpc SubscribeEvents(SubscribeEventsRequest) returns (stream RuntimeEvent);
}

message RuntimeId { string id = 1; }
enum RuntimeKind { ASSISTANT = 0; TASK = 1; WORKBENCH = 2; EVAL = 3; }
enum RuntimeState { CREATING = 0; STARTING = 1; RUNNING = 2; STOPPING = 3; STOPPED = 4; FAILED = 5; KILLED = 6; OOM = 7; CRASHED = 8; }

message CreateRuntimeRequest { RuntimeKind kind = 1; string workspace = 2; }
message CreateRuntimeResponse { RuntimeId runtime = 1; }

message ExecRequest {
    RuntimeId runtime_id = 1;
    repeated string command = 2;
    string cwd = 3;
    map<string, string> env = 4;
    uint64 timeout_ms = 5;
}

message ExecEvent {
    oneof event {
        ExecStarted started = 1;
        bytes stdout = 2;
        bytes stderr = 3;
        ExecExit exit = 4;
    }
}

message PtyMessage {
    RuntimeId runtime_id = 1;
    string pty_id = 2;
    oneof msg {
        PtyOpen open = 3;
        bytes data = 4;
        PtyResize resize = 5;
        PtySignal signal = 6;
        PtyClose close = 7;
        PtyExit exit = 8;
    }
}

message RuntimeEvent {
    RuntimeId runtime_id = 1;
    oneof event {
        RuntimeCreated created = 2;
        RuntimeStarted started = 3;
        ProcessStarted proc_started = 4;
        ProcessExited proc_exited = 5;
        PtyOpened pty_opened = 6;
        PtyClosed pty_closed = 7;
        BrowserStarted browser_started = 8;
        BrowserExited browser_exited = 9;
        RuntimeFailed failed = 10;
        RuntimeDestroyed destroyed = 11;
        OomEvent oom = 12;
    }
}
```

Control Plane 用 protobuf RPC, 大量二进制 (screenshot) 用 bytes, 避免 base64->JSON

兼容层: 第一阶段 sandd 继续监听 1337/1338, 模拟现有 exec-daemon HTTP API, 让 host-main 无感迁移. 兼容层内部调用新的 Runtime Kernel.

## 9. Supervisor 逐步进入 sandd

最终 sandd 自己作为 supervisor

```rust
struct DesiredState {
    host: HostSpec,
    desktop: DesktopSpec,
    runtimes: HashMap<RuntimeId, RuntimeSpec>,
}

struct ObservedState {
    host: ProcessState,
    desktop: DesktopState,
    runtimes: HashMap<RuntimeId, RuntimeState>,
}
```

reconcile: desired -> reconcile -> observed

负责 host process, desktop, browser worker, runtime 健康与生命周期

第一阶段先不删除现有 sand-supervisor, 先让 sandd 作为 exec-daemon replacement 跑通, 第二阶段再接管 supervisor.

## 10. Desktop Manager

独立模块:

```
DesktopManager
  ├─ Xvfb
  ├─ window manager (xfwm4)
  ├─ VNC/noVNC
  ├─ DISPLAY
  ├─ Chrome desktop binding
  ├─ health check
  └─ restart
```

Runtime 可以: 无桌面, 共享桌面, 独立桌面. 不要默认每个 Runtime 都创建完整 desktop.

实现: 复用现有 /tmp/sand-desktop 机制, 但由 sandd 统一管理, 而非分散脚本.

## 11. Computer Use

两层:

```
Computer
├── Native Computer
│    ├─ screenshot (X11 XShm, 避免 scrot shell)
│    ├─ mouse move/click (XTest)
│    ├─ keyboard input (XTest)
│    ├─ scroll
│    ├─ window list/focus (X11)
│    └─ 降低 roundtrip 和进程创建开销
└── Browser Computer
     ├─ Playwright / DOM / accessibility (优先)
     ├─ CDP
     └─ screenshot + vision + native mouse (fallback)
```

执行优先级: 1. Playwright/DOM/accessibility, 2. CDP, 3. screenshot+vision+native mouse (fallback)

## 12. Browser Provider

抽象:

```rust
trait BrowserProvider {
    async fn launch(&self, runtime_id: RuntimeId, opts: BrowserOpts) -> Result<BrowserHandle, BrowserError>;
    async fn close(&self, handle: BrowserHandle) -> Result<(), BrowserError>;
}
```

第一版: Node + Playwright sidecar, sandd 负责启动、生命周期、cgroup、RPC、Chrome process、profile, Playwright 负责 DOM/selector/accessibility/click/fill/navigation/evaluate

未来可替换 CDP, chromiumoxide, WebDriver BiDi, 但 Runtime Kernel 不改.

## 13. State: SQLite WAL

不再散落 JSON, 使用 SQLite WAL 存 metadata:

```
runtime (id, kind, state, workspace, created_at, started_at, desktop, browser, capabilities)
process (id, runtime_id, pid, pidfd, command, cwd, state, exit_code, signal, started_at, exited_at)
pty (id, runtime_id, pid, cols, rows, cwd, shell, state)
browser_session (id, runtime_id, profile, cdp_port, state)
desktop_session (id, display, state)
capability
event (id, runtime_id, kind, payload, at)
```

日志、截图、transcript 大文件仍存 filesystem, SQLite 存 metadata

支持 crash 恢复: 重启后扫描 SQLite, cgroup, /proc, 重建 ObservedState, 区分 Running/Dead/Orphaned/Unknown, 然后 reconcile

## 14. Event Bus

统一事件:

```rust
enum RuntimeEvent {
    RuntimeCreated { id },
    RuntimeStarted { id },
    ProcessStarted { runtime_id, pid },
    ProcessExited { runtime_id, pid, code, signal },
    PtyOpened { runtime_id, pty_id },
    PtyClosed { runtime_id, pty_id },
    BrowserStarted { runtime_id },
    BrowserExited { runtime_id },
    Oom { runtime_id },
    RuntimeFailed { id, reason },
    RuntimeDestroyed { id },
}
```

通过 tokio broadcast 分发, 外部可订阅 Runtime event stream

## 15. Capability / Security

引入 CapabilitySet:

```
runtime:A:exec
runtime:A:pty
runtime:A:computer
runtime:A:browser
```

Runtime A 不应直接控制 Runtime B

UDS 可调研 SO_PEERCRED 识别本机调用进程

第一阶段不需要复杂权限, 但数据结构和接口必须为 capability 留位置

不再只依赖 auth-token=local

## 16. Observability

- structured logging via tracing, 包含 runtime_id, process_id, pty_id, request_id, agent_id
- metrics: runtime count, process count, pty count, cpu, memory, oom, exec latency, pty IO
- CLI:

```bash
sand status
sand runtime list
sand runtime inspect <id>
sand runtime ps <id>
sand runtime kill <id>
sand pty list <id>
sand logs <id>
```

- 错误 typed, 不只是 "exec failed", 而是包含 runtime, operation, error

## 17. 重点解决 orphan

测试场景:

- Agent 突然被 kill
- host-main crash
- sandd crash/restart
- Chrome crash
- PTY client disconnect
- shell fork child, double fork
- OOM, SIGKILL

验收: Runtime Destroy 后 cgroup.procs=0, PTY=0, Chrome=0, browser worker=0, 临时资源释放

实现: cgroup.kill + pidfd + 定期 /proc 扫描 + 明确 ownership

## 18. 崩溃恢复

sandd 重启后:

- 扫描 SQLite
- 扫描 /sys/fs/cgroup/sand/runtime-*
- 扫描 /proc

重建 ObservedState, 区分 Running/Dead/Orphaned/Unknown, reconcile

不要 daemon 重启 = 所有 Runtime 信息丢失

## 19. 最终设计原则

> sandd 是本机 Assistant Computer 的 Runtime Kernel

上层只表达:

```
CreateRuntime
Exec
OpenPty
StartBrowser
ComputerAction
DestroyRuntime
```

剩余全部交给 sandd
