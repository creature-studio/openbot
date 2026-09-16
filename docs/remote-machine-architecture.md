# Spark 远程机器 / SSH Runtime 架构与实现

> 本文档既是设计说明，也是**当前代码实现**的对照表。第 12 节列出了沙箱内可验证的部分与必须在真实机器上跑的 E2E（`sand/test_remote_ssh.sh`）。

## 1. 目标与约束

Spark 的 GPUI 客户端可以添加 SSH 机器，Task / Workbench / Bot / AgentSession 都可以选择 **Local** 或 **SSH 远程机器**，而**上层 agent 完全感知不到本地/远程差异**。

硬性约束与证据（`sand/check_spark.sh` 覆盖静态安全/边界不变量；编译、真实 SSH 和完整模型会话不能由静态检查替代）：

| 约束 | 实现位置 |
| --- | --- |
| GPUI 客户端**不直接 SSH**，一切经由 host-agent | `spark-client` 只连 `host-agent serve` 的 UDS；`MachineManager` 是唯一持有 `SshTransport` 的地方 |
| 不复制模型（没有 LocalTask/RemoteTask） | `spark-model::Task/Workbench` 只有 `machine_id` 字段 |
| 一条长连接承载 RPC/PTY/events，**禁止一个 tool call 起一个 ssh process** | `spark-transport/src/ssh/mod.rs` 的 `BridgeConnection`（一个 `sand bridge` 子进程 + 多路复用帧） |
| 远端 sandd **只监听 UDS**（`~/.cache/spark/run/sandd.sock`, 0660），不开 TCP | `sandd/src/main.rs`（只 `UnixListener`），`test_remote_ssh.sh` 会 grep 验证 |
| PTY 走远端 Runtime，**不是独立的 ssh 交互 shell** | 所有 `terminal.*` 工具走 `RuntimeTransport::open_pty/write_pty/...`，沙箱内 grep 断言 |
| 禁止 `StrictHostKeyChecking=no`；未知 key → `Attention::PermissionRequired` + 指纹 + [取消]/[信任并连接]；changed key 高风险、绝不自动接受 | `ssh/hostkey.rs` + `MachineManager::record_connect_error` + `MachineStore::raise_attention` |
| 不存私钥/密码；只存 host alias / 显示名 / port / user | `persistence.rs` 的 `machines` 表；`check_spark.sh` 断言无 `password/private_key` 字段 |
| 不装到 `/usr/local/bin`，默认 `~/.local/share/spark` | `ssh/bootstrap.rs` 的 `SparkPathLayout` |
| bootstrap 必须校验 SHA256 + `sandd --version` | `bootstrap()` 的 `scp_upload` + `remote_sha256` + `--version` 比对 |
| 协议版本与二进制版本分离 | `sand_protocol::SAND_PROTOCOL_VERSION` (=2) vs `SANDB_VERSION` (="0.1.0") |
| 任务结束只销毁 Runtime，**绝不杀远端 sandd** | `MachineManager::destroy_runtime` / `HostAgentApi::destroy_session_runtime` |
| SSH 断开 ≠ Runtime 销毁；任务不能自动 Failed | `MachineStore` → `TaskEntity::mark_connection_lost`（paused 状态行，不置 Failed） |
| SFTP/SCP 只用于 bootstrap 与 artifact | `SshTransport::scp_upload/scp_download` 仅被 bootstrap 与 artifact 调用 |
| 不用 VNC，用丢帧的截图流 | `sandd/browser` + `spark-transport/browser.rs`（帧丢弃） |
| 健康检查走 bridge Ping/Pong，**不是 `ssh hostname`** | `MachineManager::health_check_once` |
| 认证失败 / 主机密钥变更 → `RequiresUserAction`，不无限重试 | `reconnect::plan()` 的三态 `ReconnectOutcome` |
| Task 的机器在创建时固定，v1 不做迁移 | `TaskEntity::machine_id` / `AgentSession::machine_id`（`handoff` 保留机器） |
| 禁用范围：K8s、Docker remote API、云厂商 API、WireGuard、Tailscale SDK、RDP、机器迁移、自研多跳、密码保险箱 | 代码中不存在（ProxyJump 走 OpenSSH） |

## 2. 分层

```text
GPUI Client (spark-client / spark-ui)
   │  newline-JSON over UDS        ← 客户端不会 import 任何 ssh 相关代码
   ▼
host-agent  (MachineManager = 唯一路由点)
   │  RuntimeTransport trait
   ├── LocalTransport ── unix socket ──▶ 本地 sandd
   └── SshTransport ───── one ssh proc ─┬─▶ sandd-binary.sock (帧协议, 二进制数据)
                                        └─▶ sandd.sock        (JSON RPC)
```

调用链（架构要求的那一条）：

```text
tool(shell.exec / file.* / terminal.* / browser.* / computer.*)
  → ToolExecutionContext.transport_for_runtime(runtime_id)
  → MachineManager.transport(machine_id)
  → Arc<dyn RuntimeTransport>.exec(...) / .fs_request(...) / .open_pty(...)
```

`ToolExecutionContext` 只暴露 `machine_for_runtime()` / `transport_for_runtime()`，工具里没有任何 `if remote` 分支（`tools/{fs,exec,terminal,terminal_signal,browser,computer}.rs` 全部改成按 runtime 路由）。

## 3. 数据模型

`client/crates/spark-model/src/machine.rs`：

```rust
pub struct MachineId(pub String);            // "machine-local" | "mach-<hex 指纹>"
pub enum MachineKind { Local, Ssh { host, port, user, ssh_config_host } }
pub enum MachineStatus {                     // 8 态
    Disconnected, Connecting, Bootstrapping, Connected,
    Degraded, Unreachable, Error, RequiresUserAction,
}
pub struct MachineCapabilities { exec, pty, filesystem, browser, computer_use, desktop, gpu }
pub struct MachineMetadata {
    os, kernel, arch, cpu_cores, cpu_percent, memory_total, memory_used,
    gpu, sandd_version, agent_version, uptime_seconds, latency_ms,
}
pub struct Machine { id, name, display_name, kind, status, capabilities, metadata, ... }
```

* `Machine::ssh(...)` 只保存连接坐标；`Machine::ssh_args()` 生成 `-T -o StrictHostKeyChecking=yes`（有 alias 时不传 `-p`，端口交给 `~/.ssh/config`）。
* `Machine::bridge_remote_command(socket)` 生成 `sand bridge --socket ~/.cache/spark/run/sandd.sock`。
* `RuntimeInfo.machine_id`（`spark-transport`）、`AgentSession.machine_id`（host-agent）、`TaskEntity.machine_id`（UI）都用同一个 `MachineId` 类型 —— 全仓库不再混用 `sand_protocol::MachineId`（`check_spark.sh` 会断言）。

## 4. RuntimeTransport trait

`client/crates/spark-transport/src/runtime_transport.rs`：

```rust
#[async_trait::async_trait]
pub trait RuntimeTransport: Send + Sync {
    fn machine_id(&self) -> MachineId;
    fn is_connected(&self) -> bool;

    async fn connect(&self) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
    async fn handshake(&self) -> Result<HandshakeResponse>;
    async fn ping(&self) -> Result<u64>;               // ms，健康检查唯一来源

    async fn create_runtime(&self, CreateRuntimeRequest) -> Result<RuntimeInfo>;
    async fn destroy_runtime(&self, runtime_id: &str) -> Result<()>;
    async fn list_runtimes(&self) -> Result<Vec<RuntimeInfo>>;
    async fn get_runtime(&self, runtime_id: &str) -> Result<RuntimeInfo>;

    async fn exec(&self, ExecRequest) -> Result<ExecResult>;
    async fn open_pty / write_pty / resize_pty / signal_pty / read_pty / close_pty / list_ptys

    async fn fs_request(&self, runtime_id: &str, FsRequest) -> Result<FsResponse>;
    async fn browser_request(&self, BrowserRequest) -> Result<BrowserResponse>;
    async fn computer_request(&self, ComputerRequest) -> Result<ComputerResponse>;
    async fn subscribe_events(&self, runtime_id: Option<&str>) -> Result<BoxStream<RuntimeEvent>>;

    async fn machine_exec(&self, command: Vec<String>) -> Result<ExecResult>;   // 诊断用
    async fn upload_file / download_file(&self, FileTransferRequest) -> ...
}
```

两个实现：

| | `LocalTransport` | `SshTransport` |
| --- | --- | --- |
| 连接 | 阻塞式 `sand_client::SandClient` 连 `sandd.sock` / `sandd-binary.sock` | 一个 `ssh -T ... sand bridge` 子进程，帧多路复用 |
| 认证 | 无 | OpenSSH（`~/.ssh/config`、ssh-agent、known_hosts、ProxyJump 全部复用） |
| 事件 | `poll_events_with` 轮询 UDS | 帧 `Event` 推送 → `EventSink` |
| bootstrap | 不适用 | `ssh/bootstrap.rs` |

## 5. Bridge 协议（本地沙箱已验证）

`SandClient::new_with_paths(json_socket, binary_socket)`，双 socket：JSON 走控制面，二进制 socket 走文件内容与 PTY 字节流。

帧头 24 字节大端（`sand-protocol/src/frame.rs`）：

```
protocol_version: u16   (=1)
kind:             u16   (1=Request 2=Response 3=StreamOpen 4=StreamData
                         5=StreamClose 6=Event 7=Ping 8=Pong)
request_id:       u64
stream_id:        u64
payload_len:      u32   (上限 8 MiB)
payload:          [u32 json_len][json][binary]
```

* `RuntimeTransport` 的 RPC（FS/PTY/Exec/Runtime/Event）全部有**二进制帧分流路径**；socket 不可用时才回退 JSON+base64 的 `SandClient` 调用（沙箱里就是这么验证的）。
* `sand bridge --socket <path>`（`sand/crates/sand-bridge/`）= 把 sandd 的 UDS 镜像成 stdio 帧，供 `ssh` 传输；`Ping → Pong`、事件推送、断线即退出，由 host-agent 负责重连。

## 6. sandd（远端守护进程）

* 只监听 UDS：`~/.cache/spark/run/sandd.sock`（0660）与 `sandd-binary.sock`；`$SAND_SOCKET_DIR` 可覆盖（测试用 `/tmp/sandd`）。**没有任何 TCP 监听**。
* `sandd --version` → `sandd 0.1.0`；握手单独带 `protocol_version`（=2），两者分离。
* `Handshake` 返回：`ok, protocol_version, sandd_version, compatible, features[], machine_id, os, arch, hostname, kernel, uptime_seconds, cpu_cores, memory_total`（`Status` 另含 `gpu`）。
* Runtime 拥有 workspace + cgroup + PTY + 进程；`Runtime.machine_id` 由创建时写入并持久化（machine 级 daemon，task 级 runtime）。
* `DestroyRuntime` 只销毁该 runtime：停 cgroup（`cgroup.kill`）、关闭它的 PTY、停掉 runtime 的 Chrome/computer 会话；**不碰 sandd 自己，也不碰别的 runtime**。
* FS RPC 白名单 + `security` 逃逸标记；`FsPatch` 接受文本 `patch` 或 `patch_b64`。

## 7. Bootstrap（R4）

`client/crates/spark-transport/src/ssh/bootstrap.rs`：

1. `uname -s && uname -m` → `linux-x86_64` / `linux-aarch64`（其他平台明确报错，不做半吊子支持）。
2. 布局（全部在用户家目录）：
   ```
   ~/.local/share/spark/versions/<version>/sandd-<target>
   ~/.local/share/spark/current -> versions/<version>       (软链)
   ~/.local/share/spark/data                                (runtime 数据, 0700)
   ~/.local/state/spark/sandd.log
   ~/.cache/spark/run/sandd.sock                            (0660)
   ```
3. 上传：`scp` 到临时名 → 远端 `sha256sum` 与本地 SHA256 比对（不一致即失败并删除）→ `chmod +x` → `sandd --version` 与期望版本比对 → 原子替换 `current`。
4. 启动：`setsid nohup sandd --socket-dir ... --data-dir ... >> log 2>&1`；如果已经在跑则只校验版本（`BootstrapReport::already_running`）。
5. 之后立刻 `connect_machine(false)` 走真实握手（不假装成功）。

## 8. 安全模型

* **主机密钥**：`ssh` 始终带 `StrictHostKeyChecking=yes`；失败后 `classify`/`ssh-keyscan` 得到 `HostKeyIssue::{Unknown,Changed}`（含 `SHA256:<b64>` 指纹）。
  * Unknown → `Attention::PermissionRequired` + 指纹 + [取消] / [信任并连接]；
  * Changed → 高风险：文案明确警告“可能中间人”，`trust_host_key()` 只有在用户二次确认后才会先删旧行再写新行；**任何自动路径都不会接受 changed key**。
  * `MachineManager` 把待确认的 issue 记在 `pending_host_keys`，`trust_host_key(&machine_id)` 只能消费这个记录，无法凭空信任别处传来的指纹。
* **凭据**：SQLite 只有 `machines(id, name, host, port, user, ssh_config_host, display_name, created_at)`；私钥/口令/口令短语永不进入 Spark，全部交给 OpenSSH + ssh-agent。
* **surface**：host-agent 的客户端 socket 位于 `~/.cache/spark/run/`（0700 目录 + 0600 socket）；sandd 的 socket 0660，仅同用户组可写。
* **健康检查**：只走 bridge Ping/Pong，绝不用 `ssh hostname` 探活（避免每次探测起一个 ssh 进程）。

阈值（架构 §二十七）：`<100ms` Connected；`100–500ms` 视作 slow（`MachineStatus::Degraded`，UI 标黄）；`>500ms` Degraded；bridge 断开 → Disconnected（**不是** runtime 挂掉）。

重连退避：`[1,2,4,8,16,30]` 秒 ±25% jitter，判决由 `reconnect::plan()` 给出三态：
`RetryAfter(delay)` / `RequiresUserAction(msg)`（主机密钥、认证失败，停止重试并弹卡片）/ `GiveUp(msg)`（重试次数耗尽）。

## 9. 断线 ≠ 销毁，重连恢复

```text
bridge 死掉
  → health_check 失败 → MachineStatus::Disconnected（机器级别）
  → 该机器上的 Task 时间线追加“连接中断”状态行（TaskEntity::mark_connection_lost）
    · 不是 Failed，agent loop 不自动失败
  → 远端 sandd / runtime / PTY / HTTP server / Chrome 继续运行
用户/后台 reconnect
  → connect_machine()（握手）
  → attach_after_reconnect(): ListRuntimes
  → 客户端收到 RuntimesUpdated / reconnected，重新挂载同一 runtime 继续干活
ReadyForCheck → Confirm Complete
  → DestroyRuntime：cgroup / PTY / Chrome 全清
  → sandd 仍然在跑（机器级 daemon）
```

## 10. GPUI 机器 UI（R7）

| 界面 | 文件 | 说明 |
| --- | --- | --- |
| MACHINES 侧栏 | `spark-ui/src/sidebar/mod.rs` + `machine/mod.rs` | 状态图标（●已连接 ◐延迟高 …连接中/ bootstrap !需确认 ○不可达 ✗错误/断开）、名称、延迟；`+ Machine`；选中机器的一句话解释（“已断开。远端 runtime / PTY / Chrome 仍然存活。”） |
| Runtime Inspector | `spark-ui/src/inspector/mod.rs`（新增 `Runtime` tab） | host / status / os / kernel+arch / cpu / memory / gpu / sandd 版本 + 延迟 / capabilities / uptime / 该机器上的 runtimes |
| “+ Machine” 表单 | `machine/mod.rs` | name / host / user / port / `~/.ssh/config` 别名开关 / [Test connection]（`ssh host true`）/ [Add machine]；文案明确“私钥、密码永远不写入数据库” |
| 主机密钥卡片 | `machine/mod.rs` + `attention/mod.rs` | 指纹 + [取消] / [信任并连接]；changed key 用红色标题与更强文案 |
| Task 机器选择 | `stores/task.rs::create_task_on` + `AppState::create_task` | 任务创建时固定机器，写入 `TaskEntity::machine_id` |
| 事件流 | `spark-transport/src/event.rs` | `MachinesUpdated / MachineStatusChanged / MachineMetadataUpdated / MachineAttentionRequired / MachineTested / MachineBootstrapProgress / MachineBootstrapFinished / RuntimesUpdated` |

客户端与 host-agent 的协议（`host-agent serve`，newline JSON，UDS）：

```json
{"cmd":"add_machine","name":"devbox","host":"10.0.0.42","user":"ubuntu","port":22,"alias":null}
{"event":"machine_attention","machine_id":"mach-1f2e","message":"首次连接…","fingerprint":"SHA256:AbCd…","changed":false}
{"cmd":"trust_host_key","machine_id":"mach-1f2e"}      ← 只有用户点 [信任并连接] 才会发出
{"event":"machine_status","machine_id":"mach-1f2e","status":"connected","detail":null}
{"event":"reconnected","machine_id":"mach-1f2e","runtimes":["rt-…","rt-…"]}
```

## 11. 分阶段状态（R1–R7）

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| R1 | 机器抽象（Machine/MachineId/RuntimeTransport/tool 路由） | 静态实现；必须通过 workspace 编译与本地 E2E 才能升级为完成 |
| R2 | SSH 连接 / ping / status / exec（OpenSSH 复用，安全默认值） | 静态实现；真实 OpenSSH 结果未在此环境取得 |
| R3 | sand bridge（一条长连接、多路复用、无 per-call ssh） | 静态实现；协议单测与真实 bridge 仍需 cargo 执行 |
| R4 | bootstrap（uname → 上传 → sha256 → `--version` → 启动） | 静态实现；未取得真实远端 bootstrap 证据 |
| R5 | 完整 Runtime（create/exec/PTY/FS/destroy）经 transport | 代码路径已统一；未取得 build/E2E 证据 |
| R6 | browser / computer（Xvfb + Chrome、截图丢帧、desktop 能力开关） | capability 检测与 transport 已接入；远端浏览器未验证 |
| R7 | GPUI Machines UI + Runtime Inspector + host-key 卡片 | 已接入行选择、添加/测试/信任/取消事件；表单输入仍需在有 GPUI 工具链的环境编译验证 |

## 12. 验证方式（重要）

开发沙箱**没有 Rust 工具链**（crates.io / static.rust-lang.org 均不可达，已多次尝试），所以：

```bash
# 1) 静态自检（安全与架构不变量 + 可选 cargo）
sand/check_spark.sh
sand/check_spark.sh --full

# 2) 编译（在有工具链的机器上）
cd sand   && cargo check --workspace --all-targets
cd client && cargo check --workspace --all-targets
cd sand   && cargo test -p host-agent machine::
cd client && cargo test -p spark-transport

# 3) 本地 E2E 回归（不需要 SSH）
sand/test_full.sh

# 4) 真实 SSH 机器 E2E（必须）
export SPARK_SSH_HOST=devbox SPARK_SSH_USER=ubuntu
sand/test_remote_ssh.sh
```

`test_remote_ssh.sh` 覆盖架构要求的那条闭环：加机器 → 自动 bootstrap → 远端 runtime → exec/FS/PTY → HTTP server + 远端浏览器截图/点击 → 掐断 bridge（机器 Disconnected，远端 sandd/runtime/PTY/Chrome 存活）→ reconnect + ListRuntimes 重新挂载 → ReadyForCheck/Confirm → DestroyRuntime 后 sandd 仍在。

**尚未完成的验证**：以上 2)–4) 都需要在真实机器/工具链上执行；本仓库当前只完成了实现与静态自检。第一次跑 `test_remote_ssh.sh` 会在未知主机密钥处停下并打印指纹 —— 那是设计行为，确认一次即可继续。

## 13. 当前验证记录（2026-09-16 UTC）

| 检查 | 结果 | 证据 |
|---|---|---|
| `sand/check_spark.sh` | 通过 | 静态安全、24-byte framing、UDS、transport、machine routing、reconnect、host-key、persistence、GPUI surface invariants 全部通过 |
| Rust workspace build/test | 未执行 | 当前运行环境没有 `cargo`、`rustc` 或 `rustup` |
| `sand/test_remote_ssh.sh` | 阻塞，未进入 SSH | 脚本在 preconditions 处退出：未设置 `SPARK_SSH_HOST`；因此没有伪造 bootstrap/runtime/exec/PTY/FS/browser/disconnect/reconnect/destroy 结果 |
| 真实 SSH Linux E2E | 未完成 | 必须在有 Rust 工具链且可达 Linux SSH 主机的环境重新运行上述脚本；本次不能声称验收完成 |

## 14. 已知限制

* v1 不做机器迁移/切换：Task 与 Workbench 的机器创建后固定。
* bootstrap 只支持 `linux-x86_64` / `linux-aarch64`。
* `computer.*` 在 `capabilities.desktop == false` 的机器上被禁用（Sandbox/无 Xvfb）。
* 浏览器预览是截图流（丢旧帧），不是 VNC/嵌入式 WebView。
* `host-agent serve` 与 GPUI 现在在同一 newline-JSON UDS 上覆盖 machine、runtime、task/session 状态、PTY、browser、computer 命令；实际 AgentLoop/模型执行仍由 host-agent 的 session runner 接管，serve 的 task 命令只负责创建/持久化 pinned session 与传递消息，不能把它当成已完成的在线模型 E2E。
