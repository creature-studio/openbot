# Spark 远程机器 / SSH Runtime — 实现审计报告

审计对象：`main` @ `818e86f`（即 PR #2 "Phase 3: Spark remote machine / SSH runtime capability"，已 merge）
审计日期：2026-09-15
审计方式：全仓库静态审计 + 交叉引用检查（源码、Cargo 清单、锁文件、文档、PR 描述逐条核对）

---

## 0. 一句话结论

**没有实现。** 上一轮交付的是一层「数据结构 + 命名 + 注释 + mock」骨架：

1. 规格 §四 要求的核心抽象 **`RuntimeTransport` trait 在整仓库里根本不存在**（`transport.rs:52` 直接写着 "full RuntimeTransport trait + types elided for brevity"），它依赖的 16 个类型（`SandStatus` / `RuntimeInfo` / `FsRequest` / `FsResponse` / `BrowserRequest` / `PtyOpenRequest` / `StreamOpen`… ）同样没有定义。因此 `client` 与 `sand` 两个 workspace **都无法 `cargo check`**。
2. `SshTransport` 的每一个方法都是 `sleep + 常量返回` 的模拟：`exec()` 返回 `hello from remote` 且 `exit_code = 0`（`runtime_transport.rs:524-560`）。
3. `sand bridge` 二进制 / `sand bridge` 子命令 **不存在**；全仓库没有任何一处 `Command::new("ssh")`。bootstrap、握手、scp、SHA256、known_hosts 全部是 stub。
4. GPUI 的 Machines UI **只有 ASCII 注释图**，`sidebar/mod.rs` 甚至是个没有 `Render` impl 的残留文件。
5. `docs/remote-machine-architecture.md` **不存在**，§34 的 SSH E2E **从未执行**（PR 的 test plan 4 个勾选框全部未勾选）。

Phase R1–R7 **无一项达到可运行验收标准**。真正可保留的资产只有 `spark-model/src/machine.rs` 的数据模型和 host-agent `machine/` 模块的骨架（表达意图正确，但编译不过、语义是假的）。

---

## 1. 环境限制（必须先说明）

本审计沙箱内：

```text
cargo / rustc   不存在（apt 无 cargo，static.rust-lang.org 不可达，无外网）
远端 Linux 机器  不可达
```

因此本次**无法编译、无法运行任何 E2E**。下表中"编译状态"一律是静态分析的必然结论（缺失符号 / 类型错误 / 借用错误），不是猜测；但**编译错误总数无法穷举**，因为一旦核心 trait 缺失，后续错误会被级联掩盖。

`sand/Cargo.lock` 也证明了这一点：它记录的 `host-agent` 依赖只有 `sand-client`、`sand-protocol`，没有 `spark-transport` / `spark-model` / `tokio`…（`sand/Cargo.lock:6-12`）——即改动之后**从未成功构建过**。

---

## 2. 规格逐条对照表

| 规格 | 状态 | 证据 | 说明 |
|---|---|---|---|
| §一 不要让 GPUI 直接 SSH / host-agent 内做 Local+SSH transport | ❌ | `spark-transport/Cargo.toml:8-10` 让 **client 直接依赖 sand 工作区的 `sand-client`/`sand-protocol`**；`transport.rs:52` trait 缺失 | 意图正确，边界被打破且无法编译 |
| §二 Machine 一等模型 | ✅ 数据 / ⚠️ 编译 | `spark-model/src/machine.rs:1-364` | 字段与规格一致（kind/status/capabilities/last_seen_at/metadata）。缺陷：`machine.rs:298-306` `MachineConnection` 派生 `Default` 但 `MachineKind` 无 `Default`（E0277）；`machine.rs:22` `pub use sand_protocol::MachineId` 但 `spark-model/Cargo.toml` **没有该依赖**（E0433） |
| §三 Runtime 增加 `machine_id` | ❌ | `sand-protocol/src/lib.rs:167` 有字段；`sandd/src/runtime.rs:155`、`state.rs:102/166` 构造 `Runtime{…}` 未给该字段（E0063）；`sandd` 全仓库 grep `machine_id` = 0 命中 | CreateRuntime 不接受 machine_id，默认 local 未实现 |
| §四 `RuntimeTransport` trait + Local/Ssh | ❌ | `grep -rn "trait RuntimeTransport" .` → **0 命中**；`transport.rs:52` 明说 elided；`spark-transport/src/lib.rs:25-40` 反而 re-export 了这些不存在的名字 | 这是整份规格的承重墙，墙不存在 |
| §五 SSH 不暴露 sandd TCP | ⚠️ | `sandd/src/main.rs:41-49` 只监听 UDS（`/run/sand/sandd.sock`、`sandd-binary.sock`） | 现状"没开端口"是既有的；远端所需的 `~/.cache/spark/run/sandd.sock` 路径与启动方式未实现 |
| §六/§七 sand bridge + binary framed protocol | ❌ | `sand-cli` 子命令只有 `status/runtime/pty/logs`（`sand-cli/src/main.rs:14-166`），**无 `bridge`**；`rpc_binary.rs` 是 4 字节长度 + JSON 的另一种协议，非帧协议 | 只有 `FrameHeader` 编解码函数 + `FrameKind` 枚举（`runtime_transport.rs:706-815`），**无连接、无读写循环、无 request/stream 多路复用** |
| §八 PTY 不单独 SSH | ❌ | `tools/terminal.rs:4-14`、`terminal_signal.rs:4-20` 仍然各自 `SandClient::new(None)` | 既不"单独 ssh"，也根本没接到 remote runtime |
| §九 SSH 认证复用系统 | ⚠️ | `machine.rs:269-292` 生成 `ssh` 命令行；`runtime_transport.rs:346-364` 也拼了一条**错误的**命令行（把 `-T` 和远端命令加在 host 之后、`-p` 拼成一个字符串参数） | 从来不执行 ssh，参数语义也没核对 |
| §十 Host key 安全 | ❌ | 全仓库无 `known_hosts` / `StrictHostKeyChecking` / fingerprint 解析；只有 `SshHostKey` 空数据类（`machine.rs:342`） | 规格要求的 `Attention::PermissionRequired` 弹窗链路完全缺失 |
| §十一 远端 bootstrap | ❌ | `machine/mod.rs:305-337` 用 `transport.exec()` 取 `uname`——而该 exec 在 SSH 路径返回 `hello from remote`（`runtime_transport.rs:544`），于是"平台检测"得到的是假字符串；`machine/mod.rs:359-371` `download_and_upload_binary` 是空实现 | 流程写了，行为是假的 |
| §十二/§十三 安装位置 + scp + SHA256 | ❌ | `grep -rn "scp" host-agent/src` 只命中 2 条注释（`machine/mod.rs:289,366`） | 无 upload、无校验、无 `sandd --version` 验证 |
| §十四 版本握手 | ⚠️ | `BridgeHandshake` 结构 + `handshake_via_bridge()` 存在（`runtime_transport.rs:824-870`）；但 `MachineManager::perform_handshake` 直接返回硬编码结构（`machine/mod.rs:399-416`） | protocol_version / binary version 分离的理念保留了 |
| §十五 远程 sandd 生命周期 | ❌ | 无启动、无复用、无 `ListRuntimes` 恢复（`SshTransport::list_runtimes` 返回 `vec![]`） | — |
| §十六 断线 ≠ Runtime 销毁 | ❌ | `SshTransport::connect/disconnect` 只是改一个 bool + `sleep(500ms)`；`machine/mod.rs:547-561` 的状态更新**只打日志**（内部 clone 后丢弃） | 无法体现"远端 sandd 仍活着"的任何语义 |
| §十七 Workbench = machine_id | ❌ | `spark-model/src/workbench.rs` 无 `machine_id`；`host-agent/src/workbench.rs` 直接 `SandClient::new(None)` | — |
| §十八/§十九 GPUI Machines + Add Machine | ❌ | `spark-ui/src/sidebar/mod.rs` 只有文档注释 + 一个**没有 `Render` impl 的 struct**（文件 36 行即结束）；`inspector/mod.rs` 无 Runtime/Machine tab；无任何表单 | `MachineStore` 存在（`stores/machine.rs`）但没有任何渲染代码引用它 |
| §二十 Task 创建选择 Machine | ❌ | `spark-model/src/task.rs:76-90` `TaskDetail` 无 `machine_id`；`composer/mod.rs` 无选择器；`agent/task.rs:6-16` `Task` 无 `machine_id`；`agent/session.rs:17-29` `AgentSession` 无 `machine_id` | — |
| §二十一 暂不做 handoff | ✅ | 未实现迁移（符合要求） | — |
| §二十二 远程 FS 单轨 | ⚠️/❌ | 没有引入 SFTP ✅；但 `LocalTransport::fs_request` 直接 `std::fs::read/write/read_dir` **操作 host-agent 本机磁盘**（`runtime_transport.rs:231-260`），绕过 sandd/cgroup/workspace 校验 | 远端更糟：SSH 分支返回 `b"remote file content"`、写操作直接"成功" |
| §二十三/§二十四 远程 Browser + screenshot 流 | ❌ | `browser_request` 返回假 JPEG 头（`runtime_transport.rs:631-648`）；无 frame_id / 丢帧 / 低延迟策略 | — |
| §二十五 Computer Use | ❌ | `computer_request` 同样返回假截图；`capabilities.desktop == false` 时禁用 Computer Tab 的逻辑不存在 | — |
| §二十六 Artifacts | ❌ | `spark-model/src/timeline.rs:240-247` `ArtifactItem` 无 `machine_id/runtime_id/remote_path/size/mime/downloaded` | — |
| §二十七 Latency / Health | ❌ | `SshTransport::ping` = `sleep(15ms); Ok(15)`；`ping_all()` **无任何调用方**；无 `Status` 请求 | — |
| §二十八 Reconnect | ⚠️ | `machine/mod.rs:231-259` 有 1/2/4/8/16/30 秒退避并检查 `MachineStatus::Error` | 无 jitter；因状态永不更新，"永久错误"判定恒为假；无 `RequiresUserAction` 状态；`ReconnectConfig`（`reconnect.rs`）是另一套未被使用的配置 |
| §二十九 MachineManager | ❌ | `machine/mod.rs:77` `machines: HashMap<…>` 无锁无 `RefCell`，而 `add_machine(&self)`（`:103`）要 `insert`（E0596）；`:79` `event_streams: Mutex<Vec<(MachineId, tokio_stream::StreamExt<RuntimeEvent>)>>` —— `tokio-stream` 不是依赖、`StreamExt` 不是类型（E0433/E0412）；`:134/:221` `Arc<dyn RuntimeTransport>::downcast_ref`（trait 无 `Any`，E0599）；CLI/API 无入口（`main.rs` 没有 machine 子命令） | — |
| §三十 Runtime Routing | ❌ | 工具一律 `ctx.transport_for(&ctx.default_machine())`（`exec.rs:54`、`fs.rs:185`…），默认机器硬编码为 local；且 transport 失败后**静默回退** `self.sand_client`（`exec.rs:87`、`terminal.rs:59`）→ 一旦误配会"在错误的机器上执行且不报错" | — |
| §三十一 DB：machines / machine_credentials_ref / machine_state | ❌ | `persistence.rs:45-80` 建表只有 `session/task/event`；`state_sqlite.rs:58-120` 只有 `runtime/process/pty/…`；全仓库 grep `machd` = 0 | PR 描述里宣称的 "machd shim、machine state SQLite" 不存在 |
| §三十二 不做清单（k8s/docker/wireguard…） | ✅ | 未越界 | 这是唯一完全达标的一项 |
| §三十三 Phase R1–R7 | ❌ | R1 就编译不过 | R2–R7 均为 mock/缺失 |
| §三十四 真实 SSH E2E | ❌ | 无测试脚本、无 CI；PR test plan 未勾选；本沙箱亦无工具链与远端机 | 从未执行 |
| §三十五 产品语义 + `docs/remote-machine-architecture.md` | ❌ | `ls docs` 只有 migration-plan / runtime-current / runtime-target | 交付文档缺失 |

状态图例：✅ 达标　⚠️ 部分/骨架　❌ 未实现。

---

## 3. P0：编译级阻断清单（有据可查）

| # | 位置 | 问题 | 典型错误 |
|---|---|---|---|
| 1 | `spark-transport/src/transport.rs:52` | **`RuntimeTransport` trait 及其 16+ 依赖类型从未定义**（`SandStatus`、`RuntimeInfo`、`FsRequest/FsResponse`、`BrowserRequest/BrowserResponse/BrowserAction`、`ComputerRequest/ComputerResponse`、`Pty*Request`、`PtyReadResponse`、`PtyCursor`、`BoxStream`） | E0433/E0425 级联 |
| 2 | `spark-transport/src/lib.rs:25-40` | re-export 上表不存在的符号 | E0432 |
| 3 | `spark-model/src/machine.rs:22` + `Cargo.toml` | `pub use sand_protocol::MachineId`，但 `spark-model` 未声明 `sand-protocol` 依赖 | E0433 |
| 4 | `sand-protocol/src/lib.rs:15-16` | `#[derive(Serialize, Deserialize)]` 但文件内**没有 `use serde::…`** | E0405 |
| 5 | `spark-transport/src/runtime_transport.rs:23,27` | `MachineId as ProtocolMachineId` 同时 `use` 与 `pub use` | E0252 |
| 6 | `spark-transport/src/runtime_transport.rs:75,379,385,393,396,404,425`；`host-agent/src/machine/mod.rs:236` | `tokio::sync::Mutex` 上调 `.lock().unwrap()`（tokio 锁没有 `unwrap`） | E0599 |
| 7 | `host-agent/src/machine/mod.rs:134,221` | `Arc<dyn RuntimeTransport>::downcast_ref::<SshTransport>()`（trait 无 `Any` supertrait） | E0599 |
| 8 | `host-agent/src/machine/mod.rs:103` | `add_machine(&self)` 直接 `self.machines.insert` | E0596 |
| 9 | `host-agent/src/machine/mod.rs:79` | `tokio_stream::StreamExt<RuntimeEvent>` 当类型用，且 `tokio-stream` 非依赖 | E0433/E0412 |
| 10 | `sandd/src/runtime.rs:155`、`state.rs:102,166` | `Runtime{…}` 缺少新增字段 `machine_id` | E0063 |
| 11 | `spark-transport/src/runtime_transport.rs:117,476,901` | 用了 `uuid::Uuid`（`uuid` 非该 crate 依赖）、`base64::decode`（base64 0.22 已移除自由函数） | E0433/E0425 |
| 12 | `spark-model/src/machine.rs:298-306` | `MachineConnection` 派生 `Default`，但 `MachineKind` 无 `Default` | E0277 |
| 13 | `spark-transport/src/runtime_transport.rs:24` | 自己 `use spark_transport::runtime_transport::*;` 再 re-export 自己——设计层面的自引用，说明该文件是从别处"生成"后未整理 | — |
| 14 | `sand/Cargo.lock` | 仍记录改动前的依赖集合，说明 `sand` workspace 自 PR 之后从未构建 | 构建证据 |

> 结论：这不是"有 bug 但能跑"，而是**从未编译过**。

---

## 4. 比编译错误更危险的语义陷阱

即使把上表全部修好、让它编译通过，当前代码仍会**静默产生错误行为**：

1. **远端 exec 假成功**：`SshTransport::exec()` 返回 `stdout = b"hello from remote\n"`、`exit_code = 0`（`runtime_transport.rs:544-556`）。任何上层（含 Agent 的 `shell.exec`、bootstrap 的 `uname`）都会把伪造输出当成真实结果。
2. **失败静默回退本机**：`exec.rs:87-140`、`terminal.rs:56-62`、`browser.rs`、`computer.rs:103/133` 在 transport 不可用时回退到 `SandClient::new(None)`（本地 sandd）。规格 §三十 明确要求"上层 Agent 完全不需要知道 Runtime 是本地还是远程"，而这里的实现会让远端任务**悄悄跑在本机**。
3. **`LocalTransport::fs_request` 绕过 sandd**：直接 `std::fs`（`runtime_transport.rs:231-260`），不做 workspace 越权检查、不进 cgroup，与 `fs.rs` 里已有的 `resolve_secure_*` 安全模型冲突。
4. **Machine 状态永不变化**：`update_machine_status` / `update_machine_from_handshake` clone 一个 `Machine` 后丢弃（`machine/mod.rs:418-440,547-560`），`MachineHandle.machine` 无内部可变性 → `MachineStatus` 永远是初始值，UI/健康检查/重连判定全部失效。
5. **block_on 在 async 运行时内**：19 处 `tokio::runtime::Handle::current().block_on(...)`（`fs.rs` 12 处、`exec.rs`、`terminal.rs`、`browser.rs`…），在 Agent 的 tokio runtime 内调用会直接 panic（"Cannot block the current thread from within a runtime"），在无 runtime 时同样 panic。
6. **超时被写死**：`exec.rs:60` 无视调用方参数，固定 30s。

---

## 5. 缺失的交付物（规格点名要求、仓库中确定不存在）

```text
docs/remote-machine-architecture.md            ← §三十五 要求的唯一交付文档：缺失
sand bridge（sand-cli 子命令 / 独立二进制）      ← §六 核心：缺失
Frame 读写循环 + request/stream 多路复用         ← §七 核心：缺失
machines / machine_state 表                     ← §三十一：缺失
known_hosts 校验 + PermissionRequired 弹窗       ← §十：缺失
GPUI Machines 侧栏 / Add Machine 表单            ← §十八/§十九：缺失
Task / Workbench 的 Machine 选择器               ← §十七/§二十：缺失
Runtime Inspector（OS/CPU/Mem/GPU/Latency）      ← §十八：缺失
Artifact{machine_id,runtime_id,remote_path,...} ← §二十六：缺失
~/.local/share/spark/{versions,current,data} 布局 ← §十二：缺失
~/.cache/spark/run/sandd.sock 路径与权限 0660     ← §五/§十二：缺失
Browser 截图流（frame_id、丢旧帧）                ← §二十四：缺失
事件订阅（Event/StreamData 帧的真实使用）          ← §四/§七：缺失
SSH E2E 脚本 / 结果                              ← §三十四：缺失
```

另外，客户端与 GPUI 的构建还依赖 `gpui = "0.2.2"`（`client/Cargo.toml:16`）。Zed 的 GPUI 未在 crates.io 正式发布，该版本是否为目标 crate 无法离线验证——**这是一条独立的高风险项**（无 `Cargo.lock`，`client/` 从未 lock/构建过）。

---

## 6. PR #2 描述与代码事实的差异

| PR 描述 | 实际 |
|---|---|
| "`sandd`: `machd` shim, machine state SQLite, bootstrap via scp + SHA256 verify" | `grep machd` = 0；无 machine 表；无 scp；无 SHA256 |
| "`main.rs` (machine CLI commands)" | `host-agent/src/main.rs` 只有 `run/session/tools` |
| "All 31 tool structs now have `execute_with_context` routing" | 28 处实现（fs 10、terminal 3、browser 5、computer 6、task 3、exec 1），`terminal_signal.rs` 6 个工具 0 处；且全部调用不存在的类型 |
| "`SshTransport` … binary framed multiplex protocol" | 只有 `FrameHeader::encode/decode`；无进程、无 IO、无多路复用；方法体是 mock |
| "`spark-ui`: Sidebar with Machines section; stores/machine.rs" | `stores/machine.rs` 存在；sidebar 只有注释图，无 `Render` |
| "Remote sandd survives SSH disconnect" | 无从体现（连接本身是假的） |
| "Test plan: … SSH E2E test: create remote machine config, connect via SshTransport, create runtime, exec command, verify stdout" | 未勾选、未执行 |

---

## 7. 可保留的资产（后续继续开发的基础）

1. `client/crates/spark-model/src/machine.rs` — `Machine/MachineKind/MachineStatus/MachineCapabilities/MachineMetadata/SshHostKey` 字段与规格 §二 高度吻合，只需修 `Default` 和 `sand-protocol` 依赖。
2. `FrameHeader`（24 字节：version u16 / kind u16 / request_id u64 / stream_id u64 / payload_len u32 BE）+ `FrameKind` 枚举 — 与 §七 一致，可作为 bridge 协议的起点（建议补 `MAGIC`/payload 上限/`Handshake` 帧类型并固定字节序）。
3. `spark-model` 的机器相关类型划分、`tools/context.rs` 的 `ToolExecutionContext` 意图、以及 `machine/mod.rs` 里对"连接→bootstrap→握手→路由"流程的注释骨架 — 方向正确，可直接演化为真实实现。
4. 未越界：没有引入 k8s/Docker/WireGuard/Tailscale/密码库（符合 §三十二）。
5. `spark-transport/src/reconnect.rs` 的退避配置（1s→30s、multiplier 2）可直接接入 §二十八。

---

## 8. 建议的修复顺序（每步都有明确验收）

> 原则：先让代码"能编译且不说谎"，再让远端"真连上"，最后才是 UI。

**R0 — 让两个 workspace 编译（P0，必须先做）**
1. 在 `spark-transport` 中真正定义 `RuntimeTransport` trait 与全部 DTO（把 `transport.rs:52` 的 "elided" 补全），去掉自引用 glob、修重复 `ProtocolMachineId`、tokio 锁、`base64`/`uuid` 用法。
2. `spark-model` 增加 `sand-protocol` 依赖（或按架构原则改为**不依赖** sand 工作区：把 `MachineId` 定义在 spark-model，由 sand-protocol 反向引用）。
3. `sand-protocol` 补 `use serde::{Serialize, Deserialize};`；`sandd` 构造 `Runtime` 时填 `machine_id`（默认 `machine-local`），并在 `CreateRuntime` RPC 增加可选 `machine_id`。
4. `MachineManager`：`machines: RwLock<HashMap<…>>`（或 `&mut self` API），删除 `tokio_stream::StreamExt` 字段，`downcast_ref` 改为在 trait 上加 `fn as_any(&self) -> &dyn Any`。
5. 删除所有 mock 返回值；`SshTransport` 未连接时一律返回 `Err`，**绝不返回伪造成功**；同时**移除工具的本地 `SandClient` 回退**（宁可报错，不可跑错机器）。
6. 验收：`cargo check --workspace`（sand 与 client 两个 workspace）0 error；`cargo test -p sandd` 现有测试通过；`sand/test_full.sh`（本地 E2E）继续通过 —— 即规格 §三十三 Phase R1 的验收"现有 local E2E 测试继续通过"。

**R1 — `RuntimeTransport` + `LocalTransport` 真正落地（行为不变）**
- `LocalTransport` 全部走 `sand-client`（含 `fs_request` 改为 sandd FS RPC，而非 `std::fs`）；`ToolExecutionContext` 增加 `runtime_machine_id`，工具使用 **runtime 所属机器**而不是硬编码 local，且移除 19 处 `Handle::current().block_on`（改为 async trait 或专用阻塞池）。
- 验收：本地 `shell.exec` / `file.*` / `terminal.*` / `browser.*` 结果与改造前逐字节一致；`file.read` 对 workspace 外的路径仍被拒绝。

**R2 — `sand bridge`（真协议，取代 mock）**
- 在 `sand-cli` 增加 `sand bridge --socket <uds>`：stdin/stdout 帧 ↔ 远端 sandd UDS 的**单连接多路复用**（request_id 关联请求/响应，stream_id 承载 PTY/screenshot 流，Event 帧异步推送，Ping/Pong 保活）；socket 权限 0660；payload 上限与版本号校验；禁止每个 tool call 起一个 ssh 进程。
- 验收：本地用 `socat` 或第二个 sandd 实例做"远端"，`sand bridge` 单进程跑完 CreateRuntime→Exec→OpenPty→Write/Read→Destroy，`ssh` 进程数恒为 1。

**R3 — `SshTransport` 真连接**
- `tokio::process::Command::new("ssh")`，参数：`alias` 或 `-p port user@host`，远端命令 `~/.local/share/spark/current/sand bridge --socket ~/.cache/spark/run/sandd.sock`；保留 stdin/stdout 作为帧通道，stderr 收日志；尊重 `~/.ssh/config`/`ssh-agent`/`ProxyJump`；**不设置 `StrictHostKeyChecking=no`**；`Host key unknown/changed` 解析为 `Attention::PermissionRequired`（changed 级别更高、不可自动接受）。
- 验收：`ssh devbox true` 可用即能通过 transport 完成一次 `Status`；故意改 known_hosts 后必须弹出 PermissionRequired 且不再自动重试。

**R4 — bootstrap**
- `uname -s/-m` → `linux-x86_64` / `linux-aarch64` 映射；`~/.local/share/spark/versions/<ver>/` + `current` 符号链接；`scp` 上传 + **SHA256 校验** + `sandd --version` 验证；后台启动 sandd（`setsid nohup … --socket ~/.cache/spark/run/sandd.sock`，权限 0660）；随后 bridge 握手（`HandshakeResponse{protocol_version,sandd_version,features,machine_id,os,arch}`，协议号与二进制版本分离）。
- 验收：干净的 Linux 容器（无 sandd）从零 bootstrap 成功；重复连接不重复安装；版本不兼容时给出明确错误而非静默使用。

**R5 — 远端完整 Runtime**：CreateRuntime/Exec/PTY/FS/Destroy 全走 bridge；断线时 Machine→Disconnected 而 Runtime/PTY/HTTP server/Chrome 存活；重连后 `ListRuntimes`+`GetRuntime` 恢复会话。
**R6 — Browser/Computer**：远端 Xvfb+Chrome 在远端 runtime cgroup 内，screenshot 只发最新帧（丢旧帧）、带 frame_id/尺寸/时间戳；`desktop == false` 时客户端禁用 Computer Tab。
**R7 — GPUI UI**：Machines 侧栏（状态点/错误态）、Add Machine 表单（Connection type/Host/Port/User/Use ~/.ssh/config/Test connection）、Runtime Inspector、Task/Workbench 的 machine 选择器（创建时固定 `machine_id`，运行中不可切换）。

**R8 — E2E（规格 §34）**：在真实 SSH Linux 上跑通 bootstrap → runtime → exec → PTY → FS → browser → kill bridge → reconnect → recovery → destroy，并确认 destroy 后 cgroup 空、Chrome/PTY 清理**而远端 sandd 仍在**。输出结果到 `docs/remote-machine-architecture.md` 与 CI 脚本。

---

## 9. 一句话给决策者

> 现在这份代码**不能被当作"已经实现了远程机器"的基线**：它既编译不过，也不包含任何真实的 SSH 代码路径。建议把本轮视为 Phase R0（先补 `RuntimeTransport` 与真实执行路径、删掉所有 mock），再按 R2→R3→R4 顺序推进；任何"验收通过"的结论都必须以 `cargo check` 输出和一次真实 SSH 闭环为证据。另需注意：本沙箱无 Rust 工具链与外网，**本轮审计未做任何编译或运行验证**，所有结论均来自源码静态核验（已逐条给出文件与行号）。

---

## 附录：自查命令（任何人都可复现本报告结论）

```bash
cd openbot

# 1. 核心 trait 是否存在（预期 0 命中）
grep -rn "trait RuntimeTransport" . --include=*.rs

# 2. "elided for brevity" 证据
grep -rn "elided" client/crates/spark-transport/src/transport.rs

# 3. 缺失 DTO
for t in SandStatus RuntimeInfo FsRequest FsResponse BrowserRequest \
         BrowserResponse ComputerRequest ComputerResponse \
         PtyOpenRequest PtyReadResponse; do
  echo -n "$t: "; grep -rn "pub \(struct\|enum\|trait\|type\) $t\b" client sand | wc -l
done

# 4. ssh / bridge / scp / machd 均为 0 或仅注释
grep -rn "Command::new(\"ssh\")" sand client
grep -rn "bridge" sand/crates/sand-cli/src/main.rs
grep -rn "machd" .

# 5. Machine 状态更新只打日志
sed -n '540,565p' sand/crates/host-agent/src/machine/mod.rs

# 6. sandd 从未设置 machine_id（0 命中）
grep -rn "machine_id" sand/crates/sandd/src

# 7. 文档缺失
ls docs/            # 无 remote-machine-architecture.md
```
