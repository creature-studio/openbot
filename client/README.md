# Spark GPUI Client

Native Rust AI agent workbench built on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) — Zed's GPU-accelerated UI framework.

```text
┌─────────────────────────────────────────────────────────────────┐
│ Spark                                      Connected ●    ⌘K   │
├──────────────┬──────────────────────────────┬───────────────────┤
│              │                              │                   │
│ BOT          │     修复登录页面              │   Browser         │
│ ● Coding Bot │                              │                   │
│              │ 用户：                       │  localhost:3000   │
│ TASKS        │ 帮我修复登录异常              │                   │
│              │                              │  [browser view]   │
│ ▶ Login Bug  │ ● 正在工作                   │                   │
│ ✓ API Fix    │                              ├───────────────────┤
│ ! Deploy     │ ▶ 搜索文件        16ms       │  Files            │
│              │   src/login...               │                   │
│ WORKBENCH    │                              │  Changes           │
│              │ ▶ 读取文件        3ms        │                   │
│ Project A    │                              │  Terminal          │
│              │ ▼ 修改文件        12ms       │                   │
│              │   +12 -3                     │                   │
│              │                              │                   │
│              │ ▶ cargo test                 │                   │
│              │                              │                   │
├──────────────┴──────────────────────────────┴───────────────────┤
│  Ask Spark...                                      Stop   Send  │
└─────────────────────────────────────────────────────────────────┘
```

## Architecture

```text
┌────────────────────────────────────────────┐
│               GPUI Client                  │
│                                            │
│  Task / Workbench / Bot UI                 │
│  Timeline                                  │
│  Terminal                                  │
│  Files / Diff                              │
│  Browser Preview                           │
│  Attention / Approval                      │
└─────────────────┬──────────────────────────┘
                  │
             HTTPS / WS
                  │
                  ▼
            host-agent
                  │
               UDS RPC
                  │
                  ▼
               sandd
```

## Crate Structure

```text
client/
├── Cargo.toml
└── crates/
    │
    ├── spark-client/        # Binary entry point + MainWindow
    │   ├── src/main.rs      # App startup, window creation
    │   └── src/app.rs       # Re-exports AppState
    │
    ├── spark-ui/            # GPUI views and stores (Entity<T>)
    │   ├── stores/
    │   │   ├── connection.rs  # ConnectionStore (transport status)
    │   │   ├── bot.rs         # BotStore (bot list, selection)
    │   │   ├── task.rs        # TaskStore + TaskEntity (timeline, browser, terminals)
    │   │   ├── workbench.rs   # WorkbenchStore
    │   │   ├── attention.rs   # AttentionStore (permissions, ready-for-check)
    │   │   └── settings.rs    # SettingsStore
    │   ├── sidebar/           # Left panel: bots, tasks, workbenches
    │   ├── timeline/          # Central panel: virtualized timeline
    │   ├── composer/          # Bottom input bar
    │   ├── inspector/         # Right panel: tabs (browser, files, terminal)
    │   ├── browser/           # Screenshot stream + interaction overlay
    │   ├── terminal/          # Terminal view (delegates to spark-terminal)
    │   ├── diff/              # File changes + unified diff
    │   ├── attention/         # Overlay modals (permissions, ready-for-check)
    │   └── app.rs             # AppState: root Entity<T> owning all stores
    │
    ├── spark-model/         # Core data types (transport-agnostic)
    │   ├── task.rs            # TaskId, TaskStatus, TaskSummary, TaskDetail
    │   ├── timeline.rs        # TimelineItem enum (the core data structure)
    │   ├── bot.rs             # BotId, Bot
    │   ├── workbench.rs       # Workbench
    │   ├── attention.rs       # Attention enum
    │   ├── connection.rs      # ConnectionStatus, ServerInfo
    │   └── settings.rs        # Settings, Theme
    │
    ├── spark-transport/     # GPUI-agnostic transport layer
    │   ├── api.rs             # HTTP API client (snapshot fetches)
    │   ├── event.rs           # TransportEvent (real-time event stream)
    │   ├── transport.rs       # Transport: WS connection, command/event channels
    │   └── reconnect.rs       # Reconnect with exponential backoff
    │
    └── spark-terminal/      # Terminal state via alacritty_terminal
        └── lib.rs             # TerminalModel, TerminalManager
```

## Entity<T> Ownership Model

Following GPUI's data-flow design — no `Arc<Mutex<...>>` in UI:

```text
AppState (held by App)
│
├── ConnectionStore     Entity<ConnectionStore>
│
├── BotStore            Entity<BotStore>
│
├── TaskStore           Entity<TaskStore>
│    ├── TaskEntity A
│    ├── TaskEntity B
│    └── TaskEntity C
│
├── WorkbenchStore      Entity<WorkbenchStore>
│
├── AttentionStore      Entity<AttentionStore>
│
└── SettingsStore       Entity<SettingsStore>
```

Transport events flow:

```text
host-agent
    │
    ▼
TransportEvent
    │
    ▼
AppState.handle_transport_event()
    │
    ├──► TaskStore.handle_event()     → updates TaskEntity
    ├──► AttentionStore.set()         → shows overlay
    └──► ConnectionStore.set_status() → updates header
         │
         ▼
    cx.notify()  →  GPUI re-renders affected views
```

## Timeline Design

The central Timeline is **not** a chat view. It's a virtualized list of typed items:

```rust
enum TimelineItem {
    UserMessage(UserMessage),
    AssistantMessage(AssistantMessage),
    Status(StatusItem),
    Tool(ToolItem),              // ← Most important
    Permission(PermissionItem),
    Artifact(ArtifactItem),
    Error(ErrorItem),
    ReadyForCheck(ReadyForCheckItem),
}
```

Tool stdout chunks do NOT each become a separate item. Instead:

```text
ToolStarted → stdout deltas update the same item → ToolCompleted
```

This means 400 stdout lines = 1 ToolCard, not 400 cards.

## Building

Prerequisites:
- Rust nightly (GPUI requires nightly features)
- System libraries for GPUI: `libclang`, `libssl`, `vulkan`/`metal`, etc.

```bash
cd client
cargo run -p spark-client
```

For development without GPU, the `spark-model` and `spark-transport` crates
can be tested independently:

```bash
cargo test -p spark-model
cargo test -p spark-transport
```

## First Version Scope

1. ✅ Sidebar: Task / Workbench
2. ✅ Task Timeline (virtualized)
3. ✅ Input + Send / Stop
4. ✅ Tool Cards (collapsed by default, auto-expand when running)
5. ✅ Attention / Permission overlays
6. ✅ Terminal (via alacritty_terminal VT parser)
7. ✅ Browser screenshot + ReadyForCheck

Files / Diff will be added in the second round.
