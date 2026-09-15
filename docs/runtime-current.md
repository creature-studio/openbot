# Runtime 现状实测报告

基于 `sand-runtime-dump.tar.gz` 解包内容，包含：

- `/usr/local/bin/start-sand-box` (515 行)
- `/usr/local/bin/supervise-sand-supervisor` (234 行)
- `/usr/local/bin/supervise-exec-daemon` (230 行)
- `/usr/local/bin/start-exec-daemon` (7 行)
- `/usr/local/bin/box-cgroups.sh` (145 行)
- `/usr/local/bin/sand-desktop-supervise.sh` (73 行)
- `/usr/local/bin/sand-supervisor.mjs` (1947 行)
- `sand-supervisor-contract.mjs`, `host-policy`, `desktop`, `cgroup-accounting`, `process-identity`, `rfb-handshake`, `host-bundle`
- `box-scripts/sand-window-router.mjs`, `start-desktop.sh` (549 行) 等
- `exec-daemon/index.js` (~14MB webpack 打包, package @anysphere/exec-daemon-runtime)
- `host/host-main.cjs` (~25MB CJS 打包)
- `docs/process-snapshot.txt`, `supervisor-status.json`

## 1. 启动链路（真实）

```
容器入口
  └─ start-sand-box
       ├─ env: DISPLAY=:1, SAND_BOX_BOOT_ID, SAND_BOX_TELEMETRY_LOG=/tmp/sand-box-telemetry.log
       ├─ cgroup: source box-cgroups.sh -> sand_cgroup_setup
       │    ├─ 检查 /sys/fs/cgroup/cgroup.controllers 是否含 cpu
       │    ├─ 检查 cgroup.type 是否 threaded (dev-box 跳过)
       │    ├─ mkdir /sys/fs/cgroup/interactive, /sys/fs/cgroup/agent
       │    ├─ 将当前 cgroup 的 cgroup.procs 迁移到 agent leaf (解决 no-internal-process 约束)
       │    ├─ 写入 +cpu 到 /sys/fs/cgroup/cgroup.subtree_control
       │    └─ 应用 cpu.weight (可通过 env SAND_CGROUP_*_WEIGHT 配置)
       ├─ 桌面目录准备: /tmp/sand-desktop/shared, /tmp/sand-window-tokens.d, /tmp/sand-novnc-tokens.d
       ├─ 清理 chrome 单例锁: /home/box/chrome-profile/**/Singleton*
       ├─ fork websockify: 0.0.0.0:6081 --token-plugin TokenFile --token-source /tmp/sand-novnc-tokens.d + bounded-log
       │    └─ 注册: sand_desktop_register shared fork-websockify 4 /tmp/novnc-forks.log <pid> -- websockify ...
       ├─ window-router: /exec-daemon/node /usr/local/bin/sand-window-router.mjs 1339 1337 14000
       │    └─ 监听 1339, 根据 x-sand-display / x-sand-window-owner 路由到 1337 或 14000+display
       │    └─ 注册 shared fork-router
       ├─ session-sync, web-bot-auth, ua-governor, cookie-persist (box-bounded-log)
       ├─ supervise-exec-daemon 1337 /usr/local/bin/start-exec-daemon
       │    └─ 循环: 检查 ss -ltn sport=:1337, 清理 stale exec-daemon bundle 进程, setsid 启动 start-exec-daemon
       │         └─ start-exec-daemon: oom_score_adj -1000, cd /workspace, /exec-daemon/exec-daemon serve --port 1337 --pty-websocket-port 1338 --auth-token local --rg-path /exec-daemon/rg --pty-auth-token local-pty --computer-use-enabled --computer-use-lazy-init --mcp-meta-tool-enabled --origin-cli-enabled
       │              └─ node /exec-daemon/index.js serve ...
       ├─ egress tunnel (可选): supervise-egress-tunnel 8790 start-egress-tunnel -> 8790 WS, 8791 CONNECT, 写入 /tmp/sand-egress-proxy
       ├─ box-store copy-in: 如果 SAND_BOX_STORE_COPY_IN=1 且 host-main.cjs 存在, 运行 node host-main.cjs --box-copy-in, 处理 /home/box/sand-data 恢复
       ├─ sand-supervisor: supervise-sand-supervisor /tmp/sand-supervisor/status.json /tmp/sand-supervisor.log /exec-daemon/node /usr/local/bin/sand-supervisor.mjs
       │    └─ bash 循环: setsid launcher, 监听 status.json mtime, stale_after 180s, startup_grace 120s, quick_failures 8 次放弃, backoff 1s->30s
       │         └─ sand-supervisor.mjs
       ├─ desktop: /usr/local/bin/start-desktop.sh (via box-bounded-log)
       │    ├─ Xvfb :1 -screen 0 1280x800x24
       │    ├─ x11vnc -display :1 -rfbport 5900
       │    ├─ websockify 0.0.0.0:6080 -> localhost:5900 (或 token 模式)
       │    ├─ xfwm4 --compositor=off
       │    ├─ picom --backend xrender
       │    ├─ plank dock1 + dconf 配置
       │    └─ 生成 /usr/local/bin/box-chrome 脚本, 负责 Chrome 启动, profile, CDP 9222+display, proxy, UA, WebAuthn
       ├─ box-doctor, sand-memory-watch (python), sand-team-converge.mjs, persist-cli-auth
       └─ exit-watch: 监控 exec-daemon supervisor 和 supervisor supervise 进程, 任一退出则结束 box
```

端口约定 (来自 start-sand-box 内嵌 box-contract, 源 sand/src/shared/box/box-contract.ts):

```
1337   主 exec-daemon HTTP
1338   主 PTY WebSocket
1339   window-router (display路由)
6081   fork noVNC (websockify token)
8790   egress tunnel WS
8791   egress HTTP CONNECT
14000+ 每 Agent fork exec-daemon (14002/03/06/08/09 observed)
136xx  每 Agent PTY (13602 等)
5900+  Xvfb/x11vnc per display (5900 :1, 5901 :2...)
6080   主 noVNC
9222+  Chrome CDP per display (9223 :1, 9224 :2...)
1340   host gateway (SAND_BOX_PORT_HOST_GATEWAY)
```

## 2. 三核心进程关系

### 2.1 sand-supervisor (Node, 可读)

- 角色: 主机监护 + 升级 + 桌面监督 + box-script 同步
- 入口: /usr/local/bin/sand-supervisor.mjs, 由 supervise-sand-supervisor bash 包装
- 状态文件:
  - /tmp/sand-supervisor/status.json: {updatedAtMs, hostBundlePresent, hostRunning, hostVersion, pendingUpgradeVersion, lastCommandId, lastCommandKind}
  - /tmp/sand-supervisor/desktop-health.json: {updatedAtMs, revision, supervisionEnabled, total, up, down, crashlooping, restartsInWindow, components[]}
  - /tmp/sand-supervisor/command.json: {id, kind: ping|restart|upgrade, issuedAtMs, reason, mode: bundle|image|restart, version, bundlePath, sha256, forceNow}
  - /tmp/sand-supervisor/acks/: ack 文件名 = command id sanitize
  - /home/box/sand-data/.sand-host-upgrade.json: upgrade applied/failed marker
  - /home/box/sand-data/.sand-host-crash.json: crash marker
  - /home/box/sand-data/gateway.json: {port, token, pid, startedAt}
  - /tmp/sand-desktop/<group>/<name>.json: desktop component descriptor {argv, env, order, logFile, pidFile, listenPort}
  - /tmp/sand-desktop/<group>/<name>.pid
  - /tmp/sand-window-tokens.d/<display>: owner token
  - /tmp/sand-novnc-tokens.d/<token>: "token: localhost:5900"
- 逻辑:
  - Supervisor class extends DesktopSupervisor
  - Tick 5000ms: adoptOrphanHost, bootFetch, postSwapRestore, processCommand, manageHost, manageDesktop, probeForkRfb, sampleCgroups, syncBoxScripts, writeStatus
  - Host 管理: decideHostAction -> bundlePresent? running? busy? backoff (base 1s max 60s). launchHost: spawn node host-main.cjs with env SAND_PACKAGED=1, SAND_DATA_ROOT, SAND_HOST_IN_BOX=1, oom_score_adj -998
  - 命令处理: 读取 command.json, parseCommand, shouldProcessCommand (pending vs acked), probeBusyState via gateway.json port /health, decideUpgradeAction (defer vs force, maxDefer 6h), requestHostPause via POST /prepare-upgrade
  - Upgrade: bundle mode 验证 sha256, swapHostBundle (hostBundleSwapTargets, 检查不重叠 data root), 记录 marker, armPostSwapRollback (watching phase, healthyUptime 60s, maxQuickExits 3), 失败回滚
  - Boot fetch: 若 hostSupervision on, autoUpdate 未 opt-out, bundlePresent, localVersion == imageSha (/etc/sand-box-image-sha), 则在 20s budget 内 fetch S3 pointer sand-host-bundle-{channel}.version, digest, 下载 tgz 到 /tmp/sand-supervisor/incoming-host-bundle.tgz, swap
  - Desktop: readDesktopSpecs from /tmp/sand-desktop, 每个 spec 检查 pidFile, isPidAlive, inspectTcpPortListener (via /proc/net/tcp + cmdline匹配), isPidComm, isDockPid, findCompositorReapTargets. 重启 backoff base 1s max 30s, window 10min max 8. 写 health snapshot.
  - Fork RFB: readForkRfbTargets from novnc token dir, probeRfbTcp / probeRfbWebSocket via RFB handshake, 失败阈值 2 次则重启 x11vnc 或 websockify (SIGTERM + 5s后 SIGKILL)
  - Cgroup accounting: 读取 /sys/fs/cgroup/interactive/cpu.stat, cpu.pressure, agent 同, 格式化 log: "cgroup interactive: cpu=xx% ..."
  - Box-scripts sync: 从 /home/box/sand-host/box-scripts 同步到 /usr/local/bin, 除非 SAND_BOX_SCRIPT_SYNC_DISABLED=1, 排除 IMAGE_OWNED 脚本 (start-sand-box, sand-exit-watch, sand-supervisor.mjs, fetch-exec-daemon, sand-desktop-supervise.sh, box-cgroups.sh, ensure-machine-id, box-xvfb, box-x11vnc, start-exec-daemon, supervise-*, sand-supervisor-*.mjs)

### 2.2 host-main (打包, 25MB)

- 角色: Assistant Computer 总控 (sand-host)
- 位置: /home/box/sand-host/host-main.cjs, version 文件, package.json type commonjs
- 依赖: /exec-daemon/node (自带 node), pdf-worker.js, diff-worker.js, unified-diff-worker.js, diff-patch-worker.js, sand-eval-runner.cjs (18MB)
- 职责 (从打包字符串推断):
  - 多 Agent orchestration, tool schema, browser coordination, provider logic, conversation state, feature flag, MCP, experiments
  - 通过 HTTP 调用 exec-daemon (1337 主, 或经 window-router 1339 到 14000+display)
  - 管理 /home/box/sand-data, /home/box/agent-data (symlink to sand-data), /home/box/chrome-profile, /workspace
  - 负责 box-store copy-in/out, gateway.json 写入, /prepare-upgrade, /health 端点
  - 浏览器会话协调: 为每个 display/fork 分配 chrome-profile/Fork-*, CDP port 9222+display
- 启动: 由 sand-supervisor spawn, detached, stdio 重定向到 /tmp/sand-host.log 或继承
- 发现 exec-daemon: 通过 box-contract 端口表, 或 gateway 发现
- Fork exec-daemon 创建: host-main 根据 Agent 需求创建新的 exec-daemon fork, 端口 = 14000 + display, PTY = 13600 + display? 观察到 14002->13602 等对应. 每个 fork 带 computer-use-enabled, origin-cli-enabled. 由 host-main 直接 spawn? 或通过 window-router 注册? 需要进一步确认, 但从进程表看 fork exec-daemon 进程父进程可能是 host-main 或 supervise? 从 snapshot 看 PID 26547 14002, 3224 14003, 1077568 14006, 5330 14008, 614250 14009, 均为 /exec-daemon/node /exec-daemon/index.js serve --port 1400x --pty-websocket-port 1360x ...

### 2.3 exec-daemon (打包, 14MB)

- 角色: Shell/PTY/Filesystem/ComputerUse/MCP Meta/Origin CLI 执行层
- 位置: /exec-daemon/index.js (webpack), /exec-daemon/exec-daemon (bash wrapper), package.json name @anysphere/exec-daemon-runtime
- 自带工具: /exec-daemon/node, rg, pty.node, cursorsandbox, gh, agent-sdk, tools, canvas-runtime, lib
- 主实例参数: serve --port 1337 --pty-websocket-port 1338 --auth-token local --rg-path /exec-daemon/rg --pty-auth-token local-pty --computer-use-enabled --computer-use-lazy-init --mcp-meta-tool-enabled --origin-cli-enabled
- Fork 实例参数: 类似但无 pty-auth-token, 无 lazy-init? observed: serve --port 14003 --pty-websocket-port 13603 --auth-token local --rg-path /exec-daemon/rg --computer-use-enabled --origin-cli-enabled
- HTTP API (从字符串推断):
  - gRPC? 包含 @grpc/grpc-js
  - Shell exec: 可能 /exec, /shell, 涉及 shell_exec_pb, ShellStream, ShellSuccess, ShellTimeout etc.
  - PTY: pty_host_service_pb: SpawnPty, AttachPty, ListPtys, ResizePty, SendInput, TerminatePty, PtyEvent (ptyData, ptyExited)
  - Computer Use: computer_use_tool_pb, ComputerUseAction screenshot, etc., X11 executor, mac executor, scaling, display, lazy init
  - MCP: mcp_exec_pb, McpTextContent, McpToolResult, ListMcpResources, ReadMcpResource, mcp-disk-catalog, mcp-allowlist
  - Origin CLI: origin-cli-enabled
  - File ops: read, write, ls, grep, delete, diagnostics, fetch, git-diff, etc. (agent-exec/dist/*)
  - Background shell, subagent, canvas diagnostics, etc.
- PTY WebSocket API:
  - 监听 1338 主, 136xx fork
  - 协议: 基于 protobuf? 从字符串看 ptyId, cols, rows, cwd, shell, pid, processArgs, ptyData, ptyExited
  - 认证: pty-auth-token local-pty (主), 或无 (fork)?
  - 实现: 使用 node-pty (pty.node), helperPath, shell fork, resize, kill, data streaming
- Computer Use 实现:
  - lazy init: 首次调用时初始化 X11 executor
  - X11 executor: 使用 X11, 可能 xdotool? 但目标是降低 roundtrip, 尽量不用 shell
  - screenshot: 可能是 XShm, scrot? 需要进一步逆向
  - mouse/keyboard: XTest?
  - 浏览器 Computer: 通过 Playwright? 但 exec-daemon 内可能直接驱动?
- 生命周期:
  - 由 supervise-exec-daemon 监护主实例, fork 实例由 host-main 管理? 若 host-main crash, fork 可能 orphan
  - cgroup: 加入 agent cgroup (supervise-exec-daemon 调用 sand_cgroup_join agent), 但无 per-runtime cgroup

### 2.4 Desktop 栈

- start-desktop.sh: Xvfb :1, x11vnc :1 rfbport 5900, websockify 6080->5900, xfwm4, picom, plank dock1, box-chrome
- box-chrome: 生成 /usr/local/bin/box-chrome 脚本, 处理:
  - DISPLAY 提取 number, 决定 profile: /home/box/chrome-profile (display 1) 或 Fork-N (display >=2)
  - CDP port: 9222+display
  - XDG_RUNTIME_DIR: /tmp/xdg-runtime-box or box-N
  - DBUS: from /tmp/xdg-runtime-box-*/dbus-session-address
  - Chrome flags: --no-sandbox, --disable-dev-shm-usage, --password-store=basic, --no-first-run, --no-default-browser-check, --hide-crash-restore-bubble, --start-maximized, --class=box-chrome, --user-data-dir, --remote-debugging-port, --remote-debugging-address=127.0.0.1, --enable-unsafe-swiftshader or --use-angle=gl (spoof gpu), --ignore-gpu-blocklist, proxy via /tmp/sand-egress-proxy, UA token GrokAgent/1.0 + owner, WebAuthn proxy id file
  - 启动: setsid -f box-bounded-log --run /tmp/chrome:1.log -- env DISPLAY HOME TZ XDG_RUNTIME_DIR DBUS... nice -n 10 google-chrome-stable flags
  - 等待 CDP ready via curl http://127.0.0.1:CDP/json/version, 再检查 xdotool search --onlyvisible --class chrome
  - 统计 chrome_launch telemetry: display, mode window/prepare, attempt count, outcome ready/window_timeout/cdp_timeout, durationMs
  - WebAuthn incognito 授权: python3 脚本修改 Preferences json incognito=true
- window-router: HTTP 反向代理, 监听 1339, 读取 x-sand-display header (默认1), x-sand-window-owner header, 查找 /tmp/sand-window-tokens.d/<display> 绑定 token, timingSafeEqual 比较, 失败 403, 成功代理到 127.0.0.1:1337 (display<=1) 或 14000+display
- noVNC: websockify 6081 token plugin, token source /tmp/sand-novnc-tokens.d, heartbeat 30s, 用于 fork display VNC

### 2.5 Network/Egress

- egress tunnel: 若 SAND_EGRESS_TUNNEL_ENABLED=1, supervise-egress-tunnel 8790 start-egress-tunnel
- 代理地址写入 /tmp/sand-egress-proxy, 内容如 127.0.0.1:8791
- Chrome 使用 --proxy-server=http://<addr> --proxy-bypass-list=localhost;127.0.0.1;[::1]
- 盒外通过 WS 隧道出网

## 3. 数据与状态散落

- /tmp/sand-supervisor/status.json: host 状态
- /tmp/sand-supervisor/desktop-health.json: desktop 健康
- /tmp/sand-supervisor/command.json: 命令邮箱
- /tmp/sand-supervisor/acks/: ack 去重
- /home/box/sand-data/.sand-host-upgrade.json: upgrade marker
- /home/box/sand-data/.sand-host-crash.json: crash marker
- /home/box/sand-data/gateway.json: host gateway 发现
- /tmp/sand-desktop/*/*.json: desktop 组件 descriptor
- /tmp/sand-desktop/*/*.pid: pid 文件
- /tmp/sand-window-tokens.d/*: window owner token
- /tmp/sand-novnc-tokens.d/*: RFB token
- /home/box/sand-data, /home/box/agent-data, /home/box/chrome-profile, /workspace
- /tmp/*.log: 各种组件日志 (exec-daemon.log, sand-supervisor.log, sand-host.log, start-desktop.log, novnc-forks.log, sand-window-router.log, etc.)
- /tmp/sand-box-telemetry.log: boot_stage, exec_daemon_restart, supervisor_restart, host_boot_fetch, chrome_launch, cgroup cpu, etc.
- 进程表: ps -eo pid,rss,args
- 端口表: ss -ltn

## 4. cgroup 现状

- box-cgroups.sh 创建 /sys/fs/cgroup/interactive, /sys/fs/cgroup/agent
- 仅两个分组, 无 per-runtime 隔离
- sand_cgroup_place 将 fork-websockify, fork-router, ua-governor, start-desktop 放入 interactive
- supervise-exec-daemon 将 exec-daemon 放入 agent
- host-main 未显式放入 cgroup? 由 supervisor 继承? 需要确认, 但 supervisor 本身未调用 cgroup_join, 所以可能在 root cgroup
- 无 cgroup.kill 使用, 清理依赖 kill -- -pid (process group)
- 无 pidfd, 依赖 PID + cmdline 检查, 存在 PID reuse 风险
- CPU accounting: supervisor 读取 cpu.stat, cpu.pressure, 但无 memory, IO, PIDs 统计, 无 OOM 事件监听

## 5. Orphan 风险点

- exec-daemon fork 的 child process (shell): 若 exec-daemon 被 SIGKILL, child 可能成为 orphan, 且 cgroup 中仍残留, 因无 cgroup.kill
- Chrome: singleton 锁在启动时清理, 但运行中 crash 可能残留, 且无 per-runtime cgroup, 难以批量清理
- PTY: client disconnect 后行为不明确, 可能 PTY 继续存活或被销毁, 无清晰策略
- host-main crash: adoptedHost 机制尝试收养 orphan host (通过 gateway.json pid), 但若 gateway.json 未更新, 可能误判
- desktop 组件: supervisor 重启时 pid 文件可能 stale, 依赖 isPidAlive + argv 匹配, 但仍有 race
- box-chrome: 使用 flock 锁 /home/box/chrome-profile/.sand-launch.lock, 但多 display 并发仍可能 race
- window-router token: 若 token 文件残留, 可能导致路由到已销毁的 exec-daemon
- egress tunnel: 未在 dump 中详述, 但可能也有 orphan

## 6. Shutdown/Restart/Upgrade

- start-sand-box 等待 exec-daemon supervisor 和 supervisor supervise, 任一退出则 exit 1 结束 box
- sand-exit-watch 监控 owner 进程, 记录 telemetry
- supervisor 收到 SIGTERM/SIGINT: abort controller, stop loop, stopHost (SIGTERM adopted + child, 记录 expected exits)
- exec-daemon supervisor: 收到 TERM/INT 停止, kill -TERM -- -child, 等待
- upgrade: bundle 模式下载 tgz, 验证 sha256, swapHostBundle (原子重命名, 备份), 记录 marker, 停止 host, 重新 launch, post-swap watch 若 crash loop 则 rollback
- image 模式: 仅暂停 host, 等待外部 recreate
- restart: 停止 host, 重启
- box-store copy-in: 启动时若 flag 开, 运行 host-main --box-copy-in, 返回 10 表示 hydrated, 0 表示 no-op, 其他表示失败则禁用 snapshot-out 并保持 host down, 防止空身份

## 7. 调用链总结

```
用户在聊天客户端发消息
  ↓
平台/Gateway 下发 command.json 到 /tmp/sand-supervisor/command.json (ping/restart/upgrade)
  ↓
sand-supervisor tick 读取 command, probeBusyState via gateway.json:port /health, decideUpgradeAction, 可能 POST /prepare-upgrade
  ↓
若 upgrade bundle: 下载/验证/swap, 记录 marker, stopHost, launchHost (node host-main.cjs)
  ↓
host-main 启动, 写入 gateway.json, 暴露 /health, /prepare-upgrade, 并开始 orchestration
  ↓
host-main 根据 Agent 需求, 通过 exec-daemon HTTP API (1337 主, 或 1339 window-router -> 14000+display) 执行 Shell/FS/ComputerUse/MCP
  ↓
exec-daemon:
  - Shell: spawnInSandbox? 在 /workspace, /home/box 等执行, 返回 stdout/stderr/exit
  - PTY: WebSocket 1338/136xx, SpawnPty, SendInput, Resize, Terminate, 事件流
  - ComputerUse: X11 executor (XTest, XShm?) 或 mac sidecar, screenshot, mouse, keyboard, window list/focus
  - Browser: 通过 Chrome CDP 9222+display, 或 Playwright sidecar? 当前未见 Playwright sidecar, 可能直接 CDP
  - MCP: 动态加载 MCP server, 处理 OAuth callback
  - Origin CLI: 扩展工具面
  ↓
Chrome: 经 127.0.0.1:8791 egress 代理出网, profile 隔离, noVNC 6081/6080 提供可视化
  ↓
结果回传 host-main -> 平台 -> 聊天展示
```

## 8. 已确认的问题

1. Supervisor / host-main / exec-daemon 职责重叠: host-main 负责 Agent orchestration 又负责 exec-daemon fork 管理, supervisor 负责 host 又负责 desktop, exec-daemon 负责 Shell/PTY/ComputerUse/Browser/MCP 全部
2. 每个 Agent fork 一个 exec-daemon: 进程表显示 5 个 fork (14002/03/06/08/09) + 主 1337, 线性增长
3. 每个 Agent 有独立 HTTP/PTY 端口: 14000+, 136xx, 导致端口管理复杂, 且 window-router 需 token 验证
4. Process/Chrome/PTY 生命周期 orphan: 无 per-runtime cgroup, 依赖 PID+cmdline, 无 pidfd, 无 cgroup.kill
5. 状态散落: JSON, 进程表, 目录, 端口, 无统一 State
6. Runtime 非一等公民: 只有 Agent 概念, 无 Runtime 抽象
7. ComputerUse/Browser/Shell/PTY 耦合在 exec-daemon 内
8. 进程退出依赖 PID/shell script/supervisor, 隔离不干净
9. 难以统一支撑 Task/Workbench/Eval/Assistant: 当前仅 Assistant 形态
10. Observability: 日志分散, 无 structured tracing, 无 metrics 统一
