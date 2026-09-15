# 迁移计划

## 原则

```
incremental
compatible
observable
rollbackable
```

禁止:

1. 不要直接全部重写 host-main (25MB 打包, 含大量 orchestration, tool schema, browser coordination, provider logic, conversation state, feature flag, MCP, experiments)
2. 不要为全 Rust 把 Playwright 强行重写
3. 不要先删现有 exec-daemon 再开发
4. 不要继续为每 Runtime 分配 daemon HTTP 端口
5. 不要继续为每 PTY 分配独立监听端口
6. 不要让 Runtime 生命周期依赖 host-main 内存状态
7. 不要通过大量 shell command 实现 Process Manager
8. 不要把 SQLite 当高频日志数据库
9. 不要为抽象而抽象
10. 不要一次性大爆炸迁移

## Phase 0: 调研 (已完成)

输出:

- docs/runtime-current.md: 真实调用链, 启动顺序, 父子关系, HTTP/PTY API, fork 流程, Agent->exec-daemon 绑定, Chrome profile生命周期, Computer Use, Desktop生命周期, cgroup初始化, state位置, shutdown/restart/upgrade, orphan风险
- docs/runtime-target.md: 目标架构, Runtime一等公民, 状态机, cgroup+pidfd, Exec, PTY Broker, RPC, Supervisor, Desktop, Computer Use, Browser Provider, State SQLite, Event Bus, Capability, Observability
- docs/migration-plan.md: 本文

## Phase 1: Rust 基础

建立 Rust workspace:

```
sand/
├── Cargo.toml (workspace)
├── crates/
│   ├── sand-protocol (proto 定义, 或 rust struct)
│   ├── sand-client (UDS client)
│   ├── sandd (核心 kernel)
│   │   ├── runtime/
│   │   ├── process/
│   │   ├── exec/
│   │   ├── pty/
│   │   ├── fs/
│   │   ├── desktop/
│   │   ├── computer/
│   │   ├── browser/
│   │   ├── network/
│   │   ├── state/
│   │   ├── supervisor/
│   │   ├── rpc/
│   │   └── events/
│   ├── sand-init (初始化 cgroup, /run/sand)
│   └── sand-cli (CLI)
```

优先实现:

- RuntimeManager: Create/List/Inspect/Destroy, 状态机 Creating->Starting->Running->Stopping->Stopped, 异常 Failed/Killed/Oom/Crashed
- ProcessManager: cgroup v2 创建 /sys/fs/cgroup/sand/runtime-<id>, 加入进程, 读取 cpu/memory/pids/io/oom, cgroup.kill 清理, pidfd 防止 reuse
- Exec: Exec(runtime_id, command) 支持 cwd/env/timeout/stdin/stdout/stderr/exit/signal, streaming via channel, 自动进 cgroup
- State: SQLite WAL (rusqlite) 存 runtime/process/pty/browser/desktop/capability/event metadata, 日志等大文件仍 filesystem
- Events: tokio broadcast 或类似, RuntimeEvent 枚举, 外部可订阅
- Observability: tracing, structured logging 含 runtime_id/process_id/pty_id/request_id

推荐依赖 (优先 rustix 处理 syscall, 避免大量 bash/ps):

```
tokio (full)
serde, serde_json
tracing, tracing-subscriber
rustix, nix
rusqlite / sqlx (WAL)
uuid, dashmap, parking_lot
thiserror, anyhow
```

验收:

- cargo fmt, clippy, test 通过
- 能创建 Runtime A/B, 分别执行 pwd/echo hello/sleep, 完全独立
- Runtime A 启动 10 child, DestroyRuntime(A) 后 cgroup.procs 为空, 所有 child 清理
- Runtime B 不受 A destroy 影响
- SQLite state 持久化, sandd 重启后能重建 ObservedState

## Phase 2: PTY

实现 multiplexed PTY:

- 一个 sandd, 一个 PTY broker, N runtime, M PTY
- 逻辑标识 runtime_id + pty_id
- 协议: OPEN, DATA, RESIZE, SIGNAL, CLOSE, EXIT
- 实现: portable-pty 或 nix/rustix openpty, 每个 PTY 一个 task 管理 shell
- 支持: shell, resize, Ctrl+C, streaming, exit, disconnect/reconnect 策略清晰 (例如 PTY 继续存活, 可重连)
- 测试:
  - 一个 Runtime 同时打开 PTY 1/2/3 正常 IO 与 resize
  - PTY client disconnect 后验证是继续存活还是销毁, 行为必须清晰, 不允许随机

## Phase 3: Compatibility API

兼容现有 host-main 依赖的 HTTP :1337 和 PTY WS :1338

- sandd 继续监听 1337/1338, 模拟现有 exec-daemon API
- 内部: host-main (旧 API) -> sandd compatibility API -> 新 Rust Runtime Kernel
- 先确保 host-main 无感迁移, 等兼容层稳定再逐步让 host-main 改用 UDS/protobuf
- 不要一开始同时改两边

实现:

- axum 或类似 HTTP server 监听 1337, 实现 exec-daemon 的关键端点 (从打包字符串推断: /exec, /shell, /pty, /computer-use, /mcp 等)
- WS server 监听 1338, 兼容现有 PTY WebSocket 协议 (ptyId, cols, rows, data, exit)
- 兼容层内部将请求路由到 RuntimeManager, 例如主 exec-daemon 对应 Runtime "default" 或 "assistant-main"
- 逐步将 fork exec-daemon (14000+) 的请求也路由到 sandd 的 Runtime 隔离, 而非独立进程

验收: 现有 host-main 不改或少改即可接入 sandd, 基本 Shell/PTY/FS 功能正常

## Phase 4: 替换 fork exec-daemon

把 N x exec-daemon 改成 1 x sandd + N x logical Runtime

- 当前: Agent A -> exec-daemon 14002, Agent B -> 14003, etc.
- 新: sandd 统一管理, runtime A/B/C 区分靠 runtime_id 而非端口
- 逐步让 14000+, 136xx 端口消失, 但第一阶段兼容期可保留, 新内部设计不得依赖端口机制
- host-main 中创建 fork exec-daemon 的逻辑改为 CreateRuntime

验收:

- 多 Agent 场景下只有一个 sandd 进程
- 每个 Agent 对应一个 Runtime, 隔离 via cgroup
- 端口不再线性增长

## Phase 5: Desktop / Computer

加入 DesktopManager, ComputerManager

- DesktopManager 负责 Xvfb, xfwm4, VNC/noVNC, DISPLAY, Chrome desktop binding, health check, restart
- Runtime 可以无桌面、共享桌面、独立桌面, 不要默认每个 Runtime 都创建完整 desktop
- Computer Use 分两层 Native (screenshot via XShm, mouse via XTest, keyboard, window list/focus) 和 Browser (Playwright/DOM优先, CDP其次, screenshot+vision+mouse fallback)
- 降低 mouse/keyboard/screenshot 的 roundtrip 和进程创建开销, 尽量不用 xdotool/scrot shell 间接调用

## Phase 6: Browser Provider

让 Playwright 成为 sidecar/provider

- 定义 BrowserProvider trait: launch, close
- 第一版 Node+Playwright sidecar, sandd 负责生命周期、cgroup、RPC、Chrome process、profile, Playwright 负责 DOM/selector/accessibility/click/fill/navigation/evaluate
- 未来可替换 CDP, chromiumoxide, WebDriver BiDi, 但 Runtime Kernel 不改

## Phase 7: Supervisor 接管

sandd 接管 host, desktop, runtime 生命周期, 实现 DesiredState / ObservedState reconcile

- DesiredState { host, desktop, runtimes }
- ObservedState { host, desktop, runtimes }
- 负责健康检查与重启
- 此时再逐步移除 sand-supervisor, supervise shell scripts

此时 sandd 真正成为本机 Runtime Kernel

## 第一轮交付 (本轮)

完成 Phase0 + Phase1 + Phase2:

1. 调研文档: runtime-current.md, runtime-target.md, migration-plan.md
2. Rust sandd: CreateRuntime, ListRuntime, InspectRuntime, DestroyRuntime, Exec, cgroup lifecycle, SQLite state, event stream
3. PTY: OpenPty, WritePty, ResizePty, SignalPty, ClosePty, 一个 sandd 管理多个 Runtime 多个 PTY
4. CLI: sand runtime create/list/inspect/exec/destroy, sand pty open

验收场景 (自动测试):

- Test1: Runtime A/B 分别执行 pwd/echo hello/sleep, 完全独立
- Test2: Runtime A 启动 10 child, DestroyRuntime(A) 后所有 child 清理, cgroup.procs 为空
- Test3: Runtime B 不受 A destroy 影响
- Test4: 一个 Runtime 同时打开 PTY 1/2/3 正常 IO 与 resize
- Test5: PTY client disconnect 后策略清晰 (继续存活 vs 销毁)
- Test6: sandd 重启后 Runtime state 能重读, 识别现存进程
- Test7: OOM/SIGKILL 场景状态正确进入 Failed/Killed/Oom 之一

## 风险与回滚

- 每个 Phase 都保持兼容层, 可快速回滚到旧 exec-daemon
- 使用 feature flag 控制是否启用 sandd
- 监控 orphan, cgroup procs, 端口占用, 日志
- 保留旧 supervise 脚本直到 Phase7 完成

## 下一步

- 本轮完成后输出: 当前架构实测结论, 修改后目录树, Runtime状态机, RPC/proto定义, cgroup设计, PTY架构, 已实现代码, 测试结果, 仍未迁移能力, 下一阶段计划
- 然后进入 Phase3 兼容层, 让 host-main 无感迁移
