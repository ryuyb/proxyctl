# R09 — Linux Runtime 与 systemd 集成策略

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：官方文档为主（无 systemd 实机测试）
> 关键结论一句话：**MVP 采用「单 unit + 双进程」运行形态**（`proxy-agent.service` 是 systemd 管理的唯一单元，Mihomo 是 Agent 的子进程、与 Agent 同用户），Agent 以 root 启动但用 systemd sandbox 收敛为「只读文件系统 + 白名单 capability + 白名单 syscall」；TUN 与 nftables 能力通过两档 unit drop-in 显式开关，绝不假设存在。

---

## 1. 结论摘要（TL;DR）

| # | 结论 | 证据 |
|---|---|---|
| C1 | **不用 `DynamicUser=`**，使用静态系统用户 `proxy-agent`。原因：Agent 需要持久化 `/var/lib/proxy-agent`（配置版本、SQLite）、需要跨重启稳定的 UID 以写入 `/etc/proxy-agent`，且 `DynamicUser=` 隐含 `ProtectSystem=strict` + `RemoveIPC=` 并禁止 D-Bus 名称，与 Agent 需要调用 systemd D-Bus 冲突 | [systemd.exec 官方文档](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)（`DynamicUser=`：隐含 `ProtectSystem=strict`、`RemoveIPC=`，且 "currently incompatible with D-Bus policies"） |
| C2 | **MVP 单 unit，不双 unit**。systemd 只管理 `proxy-agent.service`；Mihomo 由 Agent `fork/exec` 作为子进程。理由见 §2.3。双 unit（`mihomo.service`）作为后续可选形态，抽象上通过 `ServiceManager` Port 预留 | 设计文档 §23 已给出该默认模型；本文补充其权限/信号后果 |
| C3 | **Agent 与 Mihomo 同用户、同 unit**，靠 capability 而非 uid 分离。Mihomo 需要 `CAP_NET_ADMIN`(+`CAP_NET_RAW`) 与 `/dev/net/tun`，Agent 把自身 capability 通过 ambient set 传给子进程 | [capabilities(7)](https://man7.org/linux/man-pages/man7/capabilities.7.html)：`fork()` 继承 capability 副本；ambient set 跨 `execve()` 保留 |
| C4 | **`NoNewPrivileges=yes` 与 `AmbientCapabilities=` 不冲突**：systemd 先提升 ambient cap、后设置 NNP；且 ambient cap 在 NNP=1 下能正常跨 `execve()` 保留。但这不代表 NNP 允许「事后提权」——见 §4.2 的实测与陷阱 | [systemd 源码 `src/core/exec-invoke.c`](https://github.com/systemd/systemd/blob/main/src/core/exec-invoke.c) + 本文 §4.2 [实测] |
| C5 | **Mihomo 不支持 `sd_notify`**，所以 Mihomo 侧只能 `Type=exec`（或上游官方用的 `Type=simple`）；Agent 侧自己可以实现 `sd_notify` 用 `Type=notify`。上游官方 unit 也是 `Type=simple` + SIGHUP reload | mihomo v1.19.30 全树 0 命中 `sd_notify`/`NOTIFY_SOCKET`/`WATCHDOG_USEC`/`go-systemd` [上游源码]；上游 `.github/release/mihomo.service` [上游源码] |
| C6 | **hardening 会破坏 TUN/nftables**：`PrivateDevices=yes` 直接让 `/dev/net/tun` 消失；`ProtectKernelTunables=yes` 让 `/proc/sys/net/**`、`/sys/**` 只读，破坏 nftables/sysctl 写入；`ProtectSystem=strict` 让 `/etc` 只读。必须做成两档：`hardened-proxy-only` 与 `tun-enabled` | [systemd.exec 官方文档](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)（`PrivateDevices=`/`ProtectKernelTunables=`/`ProtectSystem=`） |
| C7 | **不以 `sudo` 作为 Agent 的特权通道**。Agent 是 systemd 服务，需要 systemd 操作时用 **D-Bus（`org.freedesktop.systemd1`）**；该操作默认 polkit 要求 `auth_admin`，因此要么 Agent 以 root 运行并由系统调用过滤兜底，要么写一条**窄化到具体 unit 名**的 polkit rule。绝不 `NOPASSWD: ALL` | [systemd 官方 polkit 策略 `org.freedesktop.systemd1.policy.in`](https://github.com/systemd/systemd/blob/main/src/core/org.freedesktop.systemd1.policy.in)：`manage-units` 默认 `allow_any=auth_admin` |
| C8 | **Unix socket 是权限边界**：`/run/proxy-agent` 由 `RuntimeDirectory=` 创建为 `0750 proxy-agent:proxy-agent`；`agent.sock` 由 Agent 显式 `chmod 0660` + `chgrp proxyctl`（客户端加入 `proxyctl` 组即可访问）。Mihomo 的 unix controller socket 被 mihomo **硬编码 `chmod 0666`，且完全不校验 `secret`**，因此只能靠目录权限隔离——这是必须记录的已知事实 | `RuntimeDirectory=` 语义见 man；mihomo `hub/route/server.go`：`os.Chmod(addr, 0o666)` + `router(cfg.IsDebug, "", ...)` [上游源码] |
| C9 | **MVP 只做 systemd**，`direct-process` 作为无 systemd 环境（部分 LXC/容器、OpenRC）的 fallback；`InitSystem` Port 的抽象边界只暴露「观察到的事实 + 生命周期原语」，不暴露 systemd 词汇 | 见 §7、§10 |
| C10 | **deb 打包用 `conffile` 语义保护用户配置**：`/etc/proxy-agent/config.toml` 走 dpkg conffile；`/var/lib/proxy-agent` 为 `dpkg-statoverride`/postinst 创建的持久状态目录；unit 文件走 `/usr/lib/systemd/system/`；postinst 只 `daemon-reload` + 创建用户/目录，**不 enable、不 start、不改用户配置**（是否 enable 交给用户或安装器脚本显式选择） | dpkg conffile 语义 [deb(5)/deb-conffiles(5)](https://manpages.debian.org/bookworm/dpkg/deb-conffiles.5.en.html) |

---

## 2. 运行形态决策（systemd 管理谁、以什么用户）

### 2.1 三个候选形态

```text
形态 A（本文推荐，MVP）：单 unit，Agent 托管 Mihomo 子进程
systemd
  └── proxy-agent.service  (User=root, CapabilityBoundingSet 收敛, hardening)
        ├── proxy-agent (控制平面, Rust)
        └── mihomo (数据面, 子进程, 继承 ambient caps)

形态 B（后续可选）：双 unit
systemd
  ├── proxy-agent.service (User=proxy-agent 或 root, 无网络特权)
  └── mihomo.service      (User=mihomo, AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW)

形态 C（无 systemd fallback）：direct process
proxy-agent 自身以 setsid + 显式 pidfile 方式托管 mihomo
```

### 2.2 为什么 MVP 选形态 A

1. **设计文档已定调**。`docs/Mihomo Linux Management Agent — 项目设计文档.md` §23 明确：MVP 只装 `proxy-agent.service`，Mihomo「不一定作为独立 systemd unit」，默认模型是 `systemd → proxy-agent → Mihomo process`，双 unit 是以后的事。本文的任务是补齐其**权限与信号后果**，而不是推翻它。
2. **配置原子切换需要 Agent 掌控 Mihomo 生命周期**。配置版本化（`/var/lib/proxy-agent/configs/vNNN.yaml`）+ 激活 + reload + health check + 失败回滚这条链要求 Agent 能在同一个事务里「换文件 → 触发 reload → 探测 → 必要时回滚」。若 Mihomo 是独立 unit，Agent 每次 reload 都要跨 D-Bus 请求，且 `systemctl reload` 的失败语义与 Agent 的回滚事务难以对齐。
3. **ambient capability 天然给子进程**。`capabilities(7)`：`fork()` 的子进程继承 capability 副本，ambient set 跨 `execve()` 保留。因此「Agent 持有 `CAP_NET_ADMIN` → spawn mihomo → mihomo 持有 `CAP_NET_ADMIN`」不需要任何额外机制（§4.2 有实测）。
4. **进程生命周期一致**。Mihomo 随 Agent 停止而停止（`KillMode=control-group` 收整个 cgroup），不会出现「Agent 挂了但 mihomo 还在跑，新 Agent 起来后端口冲突」。

### 2.3 形态 A 的代价（必须写清楚）

- **Agent 崩溃 → Mihomo 一起被 systemd 收走**（因为同 cgroup）。这正是我们想要的行为：由 `Restart=on-failure` 把「Agent + Mihomo」整体拉起来，避免僵尸数据面。
- **Agent 的 capability 面变大**：Agent 进程自身持有 `CAP_NET_ADMIN`/`CAP_NET_RAW`，而被 Web/API 暴露的攻击面就在同一进程内。缓解手段（必须做）：
  - Agent 进程自身**不主动使用** `CAP_NET_ADMIN`（只有 mihomo 用），但 Linux 无法把 capability「只给子进程不给父进程」——systemd 只能整 unit 设置。可选的更严格做法：Agent 用 `AmbientCapabilities` 把 cap 传给 mihomo 后立即 `capset()` 从自身 effective set 丢弃（保留在 permitted/ambient 以便下次 spawn）。**这条是 [推测]**，需要在实现阶段验证 Rust 侧能否稳定做到且不影响后续 spawn。→ 记为开放问题 Q3。
- **不同用户不可行**。若 mihomo 与 Agent 不同用户，Agent（非 root）无法向 mihomo 发信号（`kill(2)` 要求同 uid 或 `CAP_KILL`），也无法直接读写 mihomo 的 socket。要跨 uid 就只能回到「systemd 管 mihomo」的形态 B。

### 2.4 用户与组模型

```text
proxy-agent  : 系统用户/组, 静态创建 (systemd-sysusers 或 postinst adduser --system)
               拥有 /var/lib/proxy-agent, /run/proxy-agent, /etc/proxy-agent
proxyctl     : 系统组, 允许通过 /run/proxy-agent/agent.sock 调用本地 API 的人类用户
               不拥有任何文件, 只用于 socket 的 group 位
```

- 用户名规则受 systemd 约束：`[a-zA-Z_][a-zA-Z0-9_-]*`，长度 ≤ 31（`User=`/`Group=` 文档明示）。
- **不选 `DynamicUser=` 的理由**（官方依据，非推测）：
  - `DynamicUser=` 隐含 `ProtectSystem=strict` + `ProtectHome=read-only`，要写盘必须逐个 `ReadWritePaths=` 白名单；
  - 隐含 `RemoveIPC=`、`NoNewPrivileges=`、`RestrictSUIDSGID=`（不可关闭）；
  - 文档明说 **"this option is currently incompatible with D-Bus policies, thus a service using this option may currently not allocate a D-Bus service name (note that this does not affect calling into other D-Bus services)"**；
  - 动态 UID 会被回收（61184…65519），文档警告不要留下属于动态 UID 的文件，否则别的 unit 可能拿到同一 UID 从而拿到这些文件。Agent 需要长期保存配置版本与 SQLite，**天然违反**这条。
  - 结论：`DynamicUser=` 适合一次性/无状态 unit，不适合 Agent。

---

## 3. 推荐的 unit 文件（Agent / Mihomo 两套，含注释）

### 3.1 `proxy-agent.service`（MVP 主 unit）

安装路径：`/usr/lib/systemd/system/proxy-agent.service`
可被 drop-in 覆盖：`/etc/systemd/system/proxy-agent.service.d/*.conf`

```ini
[Unit]
Description=Mihomo Linux Management Agent
Documentation=man:proxy-agent(8) https://example.com/docs
# 网络就绪后再起，避免 doctor 误报
After=network-online.target
Wants=network-online.target
# 让 Agent 的日志进入 journald（默认就有，显式写出便于审计）
# 若 Agent 自己写文件日志，用 LogsDirectory= 而不是追加到 journald

[Service]
# ---- 进程模型 ----
# Agent 自己调用 sd_notify(READY=1) 后才算启动完成；这样 systemctl start
# 返回时 API/socket 一定已就绪，依赖它的 unit 不会踩空。
# 需要 Agent 侧用 sd-notify crate（或 raw UNIX datagram 实现）。
Type=notify
# 需要 systemd >= 253 才支持 Type=notify-reload；MVP 目标基线 systemd >= 249，
# 因此 reload 用 ExecReload= 显式给信号，兼容性更好（见 §8）。
NotifyAccess=main
WatchdogSec=0            # 若 Agent 实现 sd_notify WATCHDOG=1，可改为 30s
# 二进制缺失/用户不存在这类错误要立刻暴露，不要用 simple 的乐观语义
# （Type=exec 自 systemd 240 起可用；notify 已包含 exec 的语义）

# ---- 用户/组 ----
User=root
# 说明：为能通过 D-Bus 管理 systemd unit、并在需要时写 /etc/proxy-agent，
# Agent 以 root 启动；真正的权限收紧由下面的 CapabilityBoundingSet= /
# ProtectSystem= / SystemCallFilter= 完成。见 §5、§6。
Group=root

# ---- 目录与状态 ----
# /run/proxy-agent  0750 proxy-agent:proxy-agent（unit 停止时清空）
# /var/lib/proxy-agent  0750（持久，unit 停止不删）
# /etc/proxy-agent  0750（持久，unit 停止不删；注意 ConfigurationDirectoryMode）
# 这三个指令同时把目录从 ProtectSystem=strict 的只读效果里排除出去，
# 并注入 $RUNTIME_DIRECTORY / $STATE_DIRECTORY / $CONFIGURATION_DIRECTORY 环境变量。
RuntimeDirectory=proxy-agent
RuntimeDirectoryMode=0750
RuntimeDirectoryPreserve=no
StateDirectory=proxy-agent
StateDirectoryMode=0750
ConfigurationDirectory=proxy-agent
ConfigurationDirectoryMode=0750

# ---- 网络与特权 ----
# 仅 proxy 模式（无 TUN / 无 nftables）时保持空集；
# 启用 TUN/透明代理时由 drop-in 追加 CAP_NET_ADMIN / CAP_NET_RAW。见 §4。
AmbientCapabilities=
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_KILL CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_SETUID CAP_SETGID
# 最小化说明：
#  CAP_NET_ADMIN/CAP_NET_RAW —— 传给 mihomo（TUN、raw socket）
#  CAP_KILL                  —— 停止/回收 mihomo 子进程（同 uid 时其实不需要，保留以便未来双用户）
#  CAP_CHOWN/CAP_DAC_OVERRIDE/CAP_FOWNER —— 写 /var/lib/proxy-agent、替换 active 符号链接
#  CAP_SETUID/CAP_SETGID    —— 未来降权 spawn 子进程时需要；MVP 若不需要可移除
# 注意：CapabilityBoundingSet= 会同时收窄 effective/permitted/inheritable set，
# 且不会限制 "+" 前缀的 ExecStart（我们不使用 "+"）。

# ---- 文件系统沙箱（proxy-only 档）----
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
# PrivateDevices= 会让 /dev/net/tun 消失 —— proxy-only 档可以开，
# tun-enabled 档必须关。见 §4。
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
ProtectProc=invisible
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
# Rust 二进制不需要 JIT；但会禁止子进程里任何 JIT/可执行栈。见 §4。
MemoryDenyWriteExecute=yes
# 只允许必要的地址族。AF_NETLINK 用于 nftables/路由查询，MVP 的 doctor 需要它。
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
# 系统调用白名单：@system-service 是 systemd 官方推荐的长期服务起点。
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources @obsolete
SystemCallArchitectures=native
# ProtectSystem=strict 下这些路径仍然可写（上面三个 Directory= 已自动豁免，
# 这里额外给 nftables 规则目录；proxy-only 档可删掉）
ReadWritePaths=/etc/proxy-agent
# 需要读 mihomo 二进制与 geo 数据
# (ReadOnlyPaths 可显式列出，非必须)

# ---- 信号与停止 ----
# Agent 应捕获 SIGTERM：停止 scheduler → 优雅停 mihomo → flush SQLite → exit 0。
# 反过来说：systemd 先发 SIGTERM，等 20s，再 SIGKILL 整个 cgroup。
KillMode=control-group
KillSignal=SIGTERM
TimeoutStopSec=20s
# 显式让 systemd 用自己的 SIGTERM 语义，不配 ExecStop=：
# systemd.service 文档指出 ExecStop= 只适合「同步等待退出」的命令，
# 只发信号不等待反而会造成不干净停止。

# ---- 重启策略 ----
Restart=on-failure
RestartSec=5s
# RestartSteps=/RestartMaxDelaySec= 需要 systemd >= 254（Debian 13 / Ubuntu 24.04 有，
# Debian 12 没有）——用 drop-in 按版本开启，见 §8.3。
StartLimitIntervalSec=0

[Install]
WantedBy=multi-user.target
Alias=proxy-agent.service
```

> `StartLimitIntervalSec=`/`StartLimitBurst=` 属于 `[Unit]` 段（定义在 `systemd.unit(5)`，不是 `systemd.service(5)`）。上面把它写在 `[Service]` 是为了示意，**实际 unit 必须放进 `[Unit]`**。文档也指出这两个选项「apply to all kinds of starts (including manual), not just those triggered by the `Restart=` logic」。

修正后的 `[Unit]` 段：

```ini
[Unit]
Description=Mihomo Linux Management Agent
After=network-online.target
Wants=network-online.target
# 如果 Agent 自己负责崩溃恢复的退避，可以禁用 systemd 的启动限流；
# 否则保留 systemd 默认限流（Debian 默认 5 次 / 10s）。
StartLimitIntervalSec=0
```

### 3.2 `mihomo.service`（形态 B，defer；供后续实现参考）

```ini
[Unit]
Description=Mihomo Proxy Core (data plane)
After=network-online.target
Wants=network-online.target
PartOf=proxy-agent.service      # Agent 重启时随之重启
# 注意：此时 Agent 必须通过 D-Bus 调 systemd 才能停/起 mihomo（见 §6.4）

[Service]
# mihomo 不支持 sd_notify（全树 0 命中 sd_notify/NOTIFY_SOCKET/WATCHDOG_USEC），
# 因此不能用 Type=notify。用 Type=exec：至少能捕获 execve 失败
# （Type=exec 自 systemd 240 起可用；上游官方 unit 用的是 Type=simple）。
Type=exec
# 不用 forking：mihomo 不 fork/daemonize/setsid，它在前台阻塞在 for/select 主循环。
User=mihomo
Group=mihomo

# 数据面需要的特权
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW
NoNewPrivileges=yes

RuntimeDirectory=mihomo
RuntimeDirectoryMode=0750
StateDirectory=mihomo
StateDirectoryMode=0750
ConfigurationDirectory=mihomo
ConfigurationDirectoryMode=0750

# ---- TUN 档必需：不能开 PrivateDevices=，否则 /dev/net/tun 不存在 ----
PrivateDevices=no
DevicePolicy=closed
DeviceAllow=/dev/net/tun rw
DeviceAllow=char-net/tun rw

ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ProtectKernelTunables=no       # nftables / route 写入需要
ProtectControlGroups=yes
ProtectKernelModules=yes
LockPersonality=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
SystemCallFilter=@system-service
SystemCallArchitectures=native
ReadWritePaths=/run/mihomo

# mihomo 的终止：捕获 SIGTERM 后 return（优雅），SIGHUP 会重读配置。
KillMode=control-group
KillSignal=SIGTERM
TimeoutStopSec=10s

Restart=always
RestartSec=3s
# 注意：mihomo 正常退出码 0；Restart=always 会把它也拉起来。
# 若希望「用户主动 stop 后不再自动起」，用 on-failure —— 但那样 OOM/崩溃恢复仍在。

ExecStart=/usr/lib/proxy-agent/mihomo -d /var/lib/mihomo -f /etc/mihomo/config.yaml
# 注意：-f 必须指向磁盘文件，绝不能用 -f -（stdin）或 -config <base64>，
# 否则 SIGHUP 重载静默无效（见 §8.1 陷阱 1）。
# 上游官方 unit 只写 -d /etc/mihomo；两者皆可，但 -f 更明确。
ExecReload=/bin/kill -HUP $MAINPID
# 上游推荐形式；但本项目 Agent 会优先走 PUT /configs?force=true（有错误反馈）。
```

> `DeviceAllow=`/`DevicePolicy=` 定义在 `systemd.resource-control(5)`，不是 `systemd.exec(5)`；且文档明确 **`DevicePolicy=` 不能被 `ExecStart` 的 `"+"` 前缀绕过（"applies to the whole control group"）**。

---

## 4. capabilities 与 hardening 取舍（两档：proxy-only / tun-enabled）

### 4.1 指令语义（官方文档要点）

| 指令 | 官方语义要点 | 引入版本 | 链接 |
|---|---|---|---|
| `CapabilityBoundingSet=` | 设置 capability bounding set；**同时影响 effective/permitted/inheritable**；`~` 取反；`"+"` 前缀的命令不受影响 | 早期（187 之前） | [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) |
| `AmbientCapabilities=` | 设置 ambient set；**会同时加入 inheritable set**；非 root 用户保留 cap 时自动加 `SecureBits=keep-caps`；不影响 `"+"` 命令 | **229** | 同上 |
| `SecureBits=` | `keep-caps` / `keep-caps-locked` / `no-setuid-fixup` / `noroot` 等 | 209 | 同上 |
| `NoNewPrivileges=` | 保证进程及其子进程**永远不会通过 `execve()` 获得新特权**（setuid/setgid/file caps）；默认 false | **187** | 同上 |
| `PrivateDevices=` | 新建私有 `/dev`，只含 `/dev/null` `/dev/zero` `/dev/random` 与 pty；**隐含 `DevicePolicy=closed`、移除 `CAP_MKNOD`/`CAP_SYS_RAWIO`、注入 `@raw-io` syscall 过滤、`/dev` 只读 + noexec** | 209 | 同上 |
| `DevicePolicy=`/`DeviceAllow=` | `strict`/`closed`/`auto` 三档；**不能用 `"+"` 绕过** | 208 | [systemd.resource-control](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html) |
| `ProtectSystem=` | `yes`→`/usr`+boot 只读；`full`→含 `/etc`；`strict`→**整个文件系统只读**，`/dev` `/proc` `/sys` 除外；**隐含豁免所有 `*Directory=` 指定的目录**；`DynamicUser=` 时隐含开启 | 214 | [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) |
| `ProtectHome=` | `yes`→`/home` `/root` `/run/user` 不可访问且为空；`read-only`；`tmpfs` | 早期 | 同上 |
| `PrivateTmp=` | 私有 `/tmp` `/var/tmp`；`disconnected` 表示独立 tmpfs | 早期 | 同上 |
| `ProtectKernelTunables=` | `/proc/sys/**`、`/sys/**`、`/proc/sysrq-trigger`、`/proc/kallsyms`、`/proc/kcore` 等只读/不可访问 | 232 | 同上 |
| `ReadWritePaths=` | 在 `ProtectSystem=` 生效后重新放开的路径 | 早期（`BindPaths=` 家族） | 同上 |
| `RestrictAddressFamilies=` | socket 地址族白名单；**只限制 `socket(2)`，不管 socket 激活传入的 fd，也不管 `socketpair()`/io_uring**；建议搭配 `SystemCallFilter=@service`；在 32-bit x86/ppc/s390 上无效 | 211 | 同上 |
| `SystemCallFilter=` | seccomp 白/黑名单；`@system-service` 是官方推荐的长期服务起点；`execve`/`exit`/`exit_group`/`getrlimit`/`rt_sigreturn`/时间类 syscall 隐式放行；**命令前缀 `"+"` 不受过滤**；多 ABI 系统建议配 `SystemCallArchitectures=native` | 早期 | 同上 |
| `MemoryDenyWriteExecute=` | 禁止 W+X 映射；**与 JIT/可执行栈/trampoline 不兼容**；可被 `memfd_create()` 或未 `noexec` 的可写文件系统绕过，建议配 `InaccessiblePaths=/dev/shm` 或 `SystemCallFilter=~memfd_create`；**x86-64 完整支持，x86 部分支持** | 231 | 同上 |
| `LockPersonality=` | 锁定 `personality(2)` | 235 | 同上 |
| `RuntimeDirectoryMode=` | `RuntimeDirectory=` 的权限位 | 234 | 同上 |
| `RuntimeDirectory=`/`StateDirectory=`/`CacheDirectory=`/`LogsDirectory=`/`ConfigurationDirectory=` | 启动时按表创建（system unit：`/run`、`/var/lib`、`/var/cache`、`/var/log`、`/etc`）；除 `ConfigurationDirectory=` 外**innermost 目录的 owner 设为 `User=`/`Group=`**；若已存在且 owner 不符会**递归 chown**；权限按 `*Mode=`；**隐含对应路径的 `BindPaths=`**；`RuntimeDirectory=` 停止时删除最内层目录（`RuntimeDirectoryPreserve=` 可改） | 235 | 同上 |

### 4.2 `NoNewPrivileges=` 与 `AmbientCapabilities=` 的交互（重点）

**结论：两者可以同时使用，是官方推荐组合。**

依据链：

1. `capabilities(7)` 定义 ambient set 为「跨 `execve()` 保留、对未特权程序也生效」的 capability 集合，并给出变换公式：
   ```text
   P'(ambient) = (file is privileged) ? 0 : P(ambient)
   P'(permitted) = (P(inheritable) & F(inheritable)) | (F(permitted) & P(bounding)) | P'(ambient)
   P'(effective) = F(effective) ? P'(permitted) : P'(ambient)
   ```
   → ambient cap 通过 `P'(ambient)` 项直接进入新进程的 permitted/effective set。
   来源：[capabilities(7)](https://man7.org/linux/man-pages/man7/capabilities.7.html)

2. `no_new_privs` 的官方定义（kernel `Documentation/userspace-api/no_new_privs.rst`）：它保证 `execve()` **不会授予原本不通过 execve 也能获得的特权**（setuid/setgid 位、file capabilities 失效）。它不改变调用者**已经持有**的 ambient set。
   来源：[no_new_privs.rst](https://raw.githubusercontent.com/torvalds/linux/master/Documentation/userspace-api/no_new_privs.rst)

3. systemd 实现顺序（`src/core/exec-invoke.c`）：
   - 提升 ambient set（`capability_ambient_set_apply(..., /* also_inherit= */ true)`）并在 `setresuid()` 之后再提升一次；
   - **之后**才调用 `proc_set_nnp()` 设置 `PR_SET_NO_NEW_PRIVS`；
   - `enforce_user()` 在非 root 且 ambient/securebits 非空时自动加 `SECURE_KEEP_CAPS`，注释原文：*"If we need to keep capabilities but drop privileges we need to make sure we keep our caps, while we drop privileges."*
   来源：[exec-invoke.c](https://github.com/systemd/systemd/blob/main/src/core/exec-invoke.c)、[capability-util.c](https://github.com/systemd/systemd/blob/main/src/basic/capability-util.c)

**[实测] 本机容器内验证**（Linux kernel inside OrbStack VM，`CapBnd` 含 `CAP_NET_ADMIN`；用 `gcc` 编译的探针程序，非 systemd）：

```text
1. capset(prm|=NET_ADMIN, inh|=NET_ADMIN)
   CapInh=0000000000001000 CapPrm=0000000000001000 CapEff=0000000000001000 CapAmb=0
2. prctl(PR_CAP_AMBIENT_RAISE, CAP_NET_ADMIN)          -> OK
   CapAmb=0000000000001000
3. prctl(PR_SET_NO_NEW_PRIVS, 1)                       -> OK，CapAmb 保持 1000
   PR_CAP_AMBIENT_IS_SET(NET_ADMIN)=1
4. fork() + execve() 自身（NNP=1 已被继承）
   child: CapInh=1000 CapPrm=1000 CapEff=1000 CapAmb=1000 NoNewPrivs=1
```

→ **ambient cap 在 `NoNewPrivs=1` 下完整跨 `execve()` 保留**，因此「Agent 用 ambient cap 把 `CAP_NET_ADMIN` 交给 mihomo，同时开 `NoNewPrivileges=yes`」是可行且安全的。

**陷阱（必须写进实现约束）**：
- `PR_CAP_AMBIENT_RAISE` 的权限检查是「**effective set 里有 `CAP_SETPCAP`**，或者 `no_new_privs` 已设置」。systemd 保证了顺序（先 raise 再 NNP），所以用户不会踩到。但**如果 Agent 自己在运行时想动态提升某个 ambient cap**（例如热切换 TUN 模式），在已经设了 NNP 的进程里可能拿到 `EPERM`——本机探针中「先 `PR_SET_NO_NEW_PRIVS(1)`、再 `capset` 收窄 effective set、再 raise」组合确实返回过 `EPERM`；由于探针环境（容器 + 非 systemd 设置的 cap 组合）与真实 systemd 启动路径不同，**该子场景记为 `[未验证]`**，结论是：**不要让 Agent 在运行时动态 raise ambient cap；模式切换必须走 systemd unit drop-in + 重启**。
- `NoNewPrivileges=yes` 还会让 **setuid 二进制和 file capability 全部失效**。Agent 若将来想用 `sudo`/setuid helper 做特权操作，会直接被 NNP 打断——这也是「特权操作不要走 sudo」的又一条理由。
- `NoNewPrivileges=` 只作用于 unit 内的进程；**对通过 IPC 请求外部服务（如 systemd 自己、`at`、`cron`）代做的操作无效**。文档原文：*"It has no effect on processes potentially invoked on request of them through tools such as at(1), crontab(1), systemd-run(1), or arbitrary IPC services."* → 这正好说明为什么「Agent 通过 D-Bus 让 systemd 干活」不受 NNP 保护，D-Bus 侧的授权必须靠 polkit。

### 4.3 两档 hardening 建议

#### 档 1：`hardened-proxy-only`（HTTP/SOCKS/Mixed 代理，无 TUN、无 nftables）

这是**默认档**，也是 PVE 非特权 LXC / 无 `CAP_NET_ADMIN` 环境的唯一可用档。

```ini
# /etc/systemd/system/proxy-agent.service.d/10-proxy-only.conf
[Service]
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
RestrictNamespaces=yes
MemoryDenyWriteExecute=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources @obsolete
SystemCallArchitectures=native
CapabilityBoundingSet=CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_KILL
AmbientCapabilities=
# 显式确认 TUN 不可用：doctor 应报告 TUN = Unavailable，而不是让启动失败
```

此档下 Mihomo 只能跑 `mixed-port`/`socks-port`/`port`，`tun.enable` 必须被 Agent 拒绝或降级。

#### 档 2：`tun-enabled`（TUN + 可选 nftables 透明代理）

```ini
# /etc/systemd/system/proxy-agent.service.d/20-tun.conf
[Service]
# ---- 破坏 TUN 的项必须关闭 ----
PrivateDevices=no            # 关键：PrivateDevices=yes 会让 /dev/net/tun 消失
ProtectKernelTunables=no     # 关键：/proc/sys/net/** 只读会破坏 nftables/sysctl 写入
ProtectSystem=strict         # 保留，但要把 nftables/规则目录加入 ReadWritePaths=
ReadWritePaths=/etc/proxy-agent /run/proxy-agent
# ---- 设备白名单（DevicePolicy 属于 cgroup 级，无法被 "+" 绕过）----
DevicePolicy=closed
DeviceAllow=/dev/net/tun rw
# ---- capability ----
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_KILL
# RestrictAddressFamilies 需放行 nftables 用的 AF_NETLINK（已默认放行）
# MemoryDenyWriteExecute 对 Rust/mihomo 无影响，可保留；但若 mihomo 加载的
# 任何 helper/脚本用 JIT 就必须去掉 —— [未验证]，见 Q2
```

**会破坏 TUN/nftables 的 hardening 清单（必须做成开关）**：

| 指令 | 影响 | 处理 |
|---|---|---|
| `PrivateDevices=yes` | `/dev/net/tun` 不存在 → TUN 必然失败 | tun 档必须 `no` |
| `DevicePolicy=strict`/`closed` 且未 `DeviceAllow=/dev/net/tun` | 打开 `/dev/net/tun` 被 cgroup 拒绝 | tun 档加 `DeviceAllow` |
| `ProtectKernelTunables=yes` | `/proc/sys/net/**`、`/sys/**` 只读 → nftables/sysctl 写入失败 | tun/nft 档必须 `no` |
| `ProtectSystem=strict` 且未豁免 | `/etc/nftables.conf` 等无法写 | 用 `ReadWritePaths=` 放开指定目录 |
| `RestrictAddressFamilies=` 未含 `AF_NETLINK` | nftables / 路由查询失败 | 显式加入 |
| `SystemCallFilter=@system-service` | nftables 需要 `setsockopt`/`socket` 等，`@system-service` 通常够；但若用 `@privileged` 排除会打断 | 保持 `@system-service` 作为白名单、只 `~` 掉明确的危险集 |
| `MemoryDenyWriteExecute=yes` | Rust/mihomo(Go) 自身无 JIT，无影响；**但若 mihomo 的 `post-up`/`post-down` 脚本或其他子进程用 JIT 会失败** | 保留并记为约束 |

### 4.4 capability 最小集表（按进程）

| 进程 | 需要的 capability | 理由 |
|---|---|---|
| `proxy-agent`（控制平面） | `CAP_CHOWN` `CAP_DAC_OVERRIDE` `CAP_FOWNER` | 写 `/var/lib/proxy-agent`、原子替换 `active` 符号链接、递归修正 `*Directory=` owner |
| `proxy-agent`（控制平面，仅形态 B） | `CAP_KILL` | 若未来 mihomo 换成不同 uid，需要发信号 |
| `mihomo`（数据面，proxy-only） | **无** | 端口 > 1024，不需要 `CAP_NET_BIND_SERVICE` |
| `mihomo`（数据面，TUN） | `CAP_NET_ADMIN` | 创建/配置 tun 设备、路由、DNS 劫持 |
| `mihomo`（数据面，ICMP/raw socket） | `CAP_NET_RAW` | 部分探测/ICMP 转发功能 |
| `proxy-agent`（nftables 写入） | `CAP_NET_ADMIN` | 写 nftables 规则表 |
| `proxy-agent`（二进制更新） | **不要给** | 见 §6：更新应走独立的 oneshot unit / 安装器 |

---

## 5. Unix socket 与权限模型

### 5.1 目录

```text
/run/proxy-agent/                0750  proxy-agent:proxy-agent    ← RuntimeDirectory=
/run/proxy-agent/agent.sock      ← Agent 自己 bind，随后 chmod 0660 + chgrp proxyctl
/run/proxy-agent/mihomo.sock     ← mihomo 自己 bind（external-controller-unix）
```

`RuntimeDirectory=proxy-agent` 的语义（官方）：

- system unit 下创建 `/run/proxy-agent`；
- **除 `ConfigurationDirectory=` 外，innermost 目录的 owner 会被设为 `User=`/`Group=`**；
- 若目录已存在但 owner 不符，**会递归 chown**；
- 权限由 `RuntimeDirectoryMode=` 决定（默认 0755，我们设 0750）；
- unit 停止时**删除最内层目录**（`RuntimeDirectoryPreserve=` 可改为保留）；
- 注入 `$RUNTIME_DIRECTORY` 环境变量；
- **隐含该路径的 `BindPaths=`，并使其豁免 `ProtectSystem=strict`**。

### 5.2 `agent.sock` 权限模型

```text
owner : proxy-agent   (bind 的进程)
group : proxyctl      (人类可访问组)
mode  : 0660          (rw for owner+group, no world)
parent: /run/proxy-agent 0750 proxy-agent:proxy-agent
```

- **为什么 0660 而不是 0666**：socket 的 `chmod` 才是真正的访问控制位（`connect(2)` 需要 write 权限）。mihomo 的 controller 被上游硬编码 `chmod 0666`，我们**无法**收紧，只能靠父目录 0750 拦住「非 proxy-agent 用户」。
- **`proxyctl` 组的用途**：让 CLI/TUI（以人类用户身份运行）能 `connect()` 到 `agent.sock`。加入组需要重新登录，安装器应在输出里提示。
- **`SO_PEERCRED` 作为二级鉴别**：Agent 在 `accept()` 后应读取对端 `struct ucred`，把 `pid`/`uid`/`gid` 记录进审计日志；本地 socket 可以「依赖 OS 权限」，但审计日志里必须有 uid，否则无法追溯。
- **不要把 socket 放进 `/tmp`**。`PrivateTmp=yes` 下 `/tmp` 是 unit 私有的，外部客户端根本连不上。
- **不要用 `ListenStream=` socket 激活**（MVP）：socket 激活会带来 fd 传递与 `NotifyAccess` 复杂度，而 Agent 需要自己控制 bind 时机（比如先加载配置再开放 API）。→ 记为 Q5。

### 5.3 `mihomo.sock` 的特殊处理

上游 `hub/route/server.go` 的 `startUnix()` 行为（[上游源码]）：

```go
dir := filepath.Dir(addr)
if _, err := os.Stat(dir); os.IsNotExist(err) {
    if err := os.MkdirAll(dir, 0o755); err != nil { ... }   // 0755！
}
_ = syscall.Unlink(addr)
l, err := lc.Listen(ctx, "unix", addr)
_ = os.Chmod(addr, 0o666)                                    // 0666！
```

三个必须记录的后果：

1. **socket 权限 0666，且无法通过配置收紧**。Agent 只能靠父目录权限（0750）隔离。
2. **Unix socket 上不做 API 认证**。server 用 `router(cfg.IsDebug, "", ...)` 构造，secret 参数传空字符串 ⇒ **不校验 `secret`**。上游中英文 wiki 均明确警告「从 Unix socket 访问 api 接口不会验证 secret」。
   → 这意味着 `mihomo.sock` 一旦被非授权用户触达，等于拿到 mihomo 的完整控制权。**目录权限就是唯一防线**。
3. **相对路径按 homeDir 解析**（`C.Path.Resolve(cfg.UnixAddr)`）。为避免歧义，`external-controller-unix` 一律写**绝对路径** `/run/proxy-agent/mihomo.sock`。

→ **强制**：Agent 必须**先**把 `/run/proxy-agent` 建成 0750（`RuntimeDirectory=proxy-agent` + `RuntimeDirectoryMode=0750` 已保证），mihomo 的 `MkdirAll(0o755)` 才不会生效（目录已存在）。否则 mihomo 会用 0755 建目录，把整个目录暴露给所有本地用户。

→ **结论**：`external-controller-unix: /run/proxy-agent/mihomo.sock` + `RuntimeDirectory=proxy-agent`（0750），**不要**对 `mihomo.sock` 做基于 group 的授权设计（mihomo 会把它变成 0666；而能穿越 0750 目录的用户只有 `proxy-agent` 自己）。

→ 若未来确实要外部访问 mihomo controller，用 **`agent.sock` 做代理**（由 Agent 做鉴权 + 审计），而不是放开 `mihomo.sock`。

> `external-controller-unix` 自 **v1.18.4** 引入（`ca84ab1`，配套 `-ext-ctl-unix` CLI flag）。`external-controller-pipe` 更晚（v1.18.8 无 / v1.18.10 有；v1.18.9 是否已含 `[未验证]`）。Linux 上只用 unix socket。

### 5.4 mihomo controller 的绑定选择

| 方式 | 配置键 / flag | 建议 |
|---|---|---|
| Unix socket | `external-controller-unix:` / `--ext-ctl-unix` | **首选**（本地、无 TCP 暴露面） |
| Named pipe | `external-controller-pipe:` / `--ext-ctl-pipe` | Windows 用，Linux 不用 |
| TCP loopback | `external-controller: 127.0.0.1:9090` | 次选，仍需 `secret` |
| TCP 0.0.0.0 | — | **禁止**（AGENTS.md 安全规则） |

---

## 6. 特权操作的最小权限表

### 6.1 操作分类

```text
只读探测 (Doctor / status)
    → 不需要任何额外特权；只需要读 /proc, /sys, /dev/net/tun 的存在性, nft list ruleset
      （nft list 需要 CAP_NET_ADMIN —— 注意：这是"只读但需特权"的边界，见下）

Mihomo 生命周期 (start/stop/restart/reload)
    → 单 unit 形态：普通进程操作（spawn / signal），不需要 capability
    → 双 unit 形态：需要 org.freedesktop.systemd1.manage-units（polkit）

配置写入 (/var/lib/proxy-agent/configs, active symlink)
    → CAP_CHOWN / CAP_DAC_OVERRIDE / CAP_FOWNER（或单纯靠 0750 目录 + 同 uid）

nftables 写入
    → CAP_NET_ADMIN

TUN 设备
    → mihomo 侧 CAP_NET_ADMIN + /dev/net/tun (0666 或 DeviceAllow)

二进制更新 (下载/校验/替换 mihomo 二进制)
    → 写 /usr/lib/proxy-agent/ → 需要 root 或 CAP_DAC_OVERRIDE + 可写目录
    → 强烈建议：拆成独立的一次性操作（oneshot unit / 安装器），不要给常驻 Agent 写 /usr 的能力
```

### 6.2 关键边界：`ProtectSystem=strict` 与「Agent 更新自己/更新 mihomo」

- `ProtectSystem=strict` 会让 `/usr`、`/etc`、`/var` 全部只读，**只有 `*Directory=` 与 `ReadWritePaths=` 例外**。
- 因此**常驻的 Agent 进程不应该、也不能够**替换 `/usr/lib/proxy-agent/mihomo`。
- 推荐模型（MVP）：
  ```text
  proxy-agent.service              ← 常驻，protect 严格，无 /usr 写权限
  proxy-agent-update.service       ← Type=oneshot，临时关掉 ProtectSystem，
                                     ExecStart=/usr/lib/proxy-agent/proxy-agent update-mihomo --version X
  ```
  Agent 通过 D-Bus `StartUnit("proxy-agent-update.service")` **发起**更新，由 systemd 以另一次受控执行完成写入。这样「谁能改变 /usr」这件事是显式的、可审计的、有独立 unit 边界的。
- **反模式**：给常驻 Agent `CAP_DAC_OVERRIDE` 且把 `/usr` 放进 `ReadWritePaths=`，等于让 Web API 的漏洞可以直接改系统二进制。

### 6.3 最小权限表（Use Case → 权限）

| Application Use Case | 需要的 OS 权限 | 实现位置 | 审计级别 |
|---|---|---|---|
| `RunDoctor`（只读） | 读 `/proc` `/sys` `/dev`；`nft list ruleset` 需 `CAP_NET_ADMIN`（若不给则报告 `Unknown` 而非 `Unsupported`） | Infrastructure `SystemProbe` | INFO |
| `StartMihomo` / `StopMihomo` / `RestartMihomo` | 单 unit：`fork`/`exec`/`signal`（同 uid 无需 cap）；双 unit：`org.freedesktop.systemd1.manage-units` | `ProcessManager` / `ServiceManager` Port | INFO |
| `ReloadMihomo` | **首选** `PUT /configs?force=true`（经 `mihomo.sock`，有 HTTP 错误反馈）；降级路径为向 mihomo pid 发 `SIGHUP` | `MihomoController` Port | INFO |
| `ActivateConfig` / `RollbackConfig` | 写 `/var/lib/proxy-agent/configs/`、`rename(2)`、`symlink(2)` | `ConfigRepository` | INFO（记录 version + checksum） |
| `UpdateSubscription` | 出网 HTTPS + 写 `/var/lib/proxy-agent/subscriptions/` | `SubscriptionConverter` / `ConfigRepository` | INFO（**URL 脱敏**） |
| `ApplyNftablesRules` | `CAP_NET_ADMIN` | `Firewall` Port | **WARN/AUDIT** |
| `UpdateMihomo` | 写 `/usr/lib/proxy-agent/` + 校验和验证 | 独立 oneshot unit | **WARN/AUDIT** |
| `SetTunMode` | 需要 unit drop-in 切换 + 重启（**运行时不可变**） | 引导用户/安装器操作 | **WARN** |

### 6.4 sudo / polkit / D-Bus 的选择

| 方案 | 优点 | 缺点 | 建议 |
|---|---|---|---|
| **A. Agent 以 root 运行 + systemd sandbox 收敛** | 不需要 polkit 规则；systemd 的 `SystemCallFilter`/`CapabilityBoundingSet`/`ProtectSystem` 仍是硬约束；实现最简单 | Agent 进程 uid=0；一旦绕过 sandbox 就是 root | **MVP 推荐**，前提是 §3.1 的 hardening 全开 |
| **B. Agent 非 root + polkit 规则** | uid 分离更干净 | 需要写 polkit rule；`manage-units` 默认 `auth_admin`（`allow_any=auth_admin`），必须显式授权；规则本身容易写宽 | 后续可选 |
| **C. `sudo` + `/etc/sudoers.d/proxy-agent`** | 直观 | sudo 的粒度是「命令行」，比 D-Bus 的方法级授权更粗；`NoNewPrivileges=yes` 会让 sudo 失效；容易滑向 `NOPASSWD: ALL` | **不推荐** |
| **D. 直接调 `systemctl` 子进程** | 最简单 | 与 B 等价（`systemctl` 内部就是 D-Bus），多一层进程开销；错误信息难结构化 | 可用，但不如直接用 D-Bus |

**官方依据**（polkit 默认策略原文，`src/core/org.freedesktop.systemd1.policy.in`）：

```xml
<action id="org.freedesktop.systemd1.manage-units">
  <description>Manage system services or other units</description>
  <defaults>
    <allow_any>auth_admin</allow_any>
    <allow_inactive>auth_admin</allow_inactive>
    <allow_active>auth_admin_keep</allow_active>
  </defaults>
</action>
```

→ 任何非 root 调用者管理 system unit 都需要**管理员认证**。因此形态 B 若 Agent 非 root，**必须**写一条窄化 polkit 规则，例如只授权 `proxy-agent` 用户对 `mihomo.service` 的 `manage-units`：

```javascript
// /etc/polkit-1/rules.d/50-proxy-agent-mihomo.rules   [未验证：规则语法需在实机测试]
polkit.addRule(function(action, subject) {
    if (action.id == "org.freedesktop.systemd1.manage-units" &&
        subject.user == "proxy-agent" &&
        action.lookup("unit") == "mihomo.service") {
        return polkit.Result.YES;
    }
});
```

**绝不允许**：

```text
proxy-agent ALL=(ALL) NOPASSWD: ALL
```

### 6.5 为什么「Doctor 只读」也需要 capability

`nft list ruleset` 需要 netlink `NETLINK_NETFILTER` 的查询权限，实践上需要 `CAP_NET_ADMIN`。这意味着**在 collect 阶段就要处理「读不到」**：

```text
nftables capability = Unavailable   （无 CAP_NET_ADMIN）
                    ≠ Unsupported   （内核/发行版层面不支持）
```

这正是设计文档 §21 要求的五态模型（`Supported` / `Unsupported` / `Unavailable` / `Misconfigured` / `Unknown`）的实际用武之地。

---

## 7. init 抽象与探测（InitSystem Port）

### 7.1 Port 边界设计

Port 放在 **Application** 层，用领域词汇而不是 systemd 词汇：

```rust
/// Application 层 Port。表达"进程监督者"的能力，不暴露 systemd 概念。
#[async_trait]
pub trait ServiceManager: Send + Sync {
    /// 该运行形态下，进程监督由谁负责
    async fn supervision_kind(&self) -> SupervisionKind;
    /// 启动数据面；返回监督句柄（可能只是 pid，也可能是 unit 名）
    async fn start_data_plane(&self, spec: &DataPlaneSpec) -> Result<SupervisedHandle>;
    /// 停止数据面；graceful 为 true 时使用配置的超时序列
    async fn stop_data_plane(&self, handle: &SupervisedHandle, policy: StopPolicy) -> Result<()>;
    /// 请求重载配置（信号或 IPC，由实现决定）
    async fn reload_data_plane(&self, handle: &SupervisedHandle) -> Result<()>;
    /// 观察到的状态（不是"我认为的状态"）
    async fn observe(&self, handle: &SupervisedHandle) -> Result<ObservedState>;
}

/// 进程监督形态（检测结果，而非假设）
pub enum SupervisionKind {
    Systemd { version: SystemdVersion },  // 有 systemd 且是 PID 1
    OpenRc { version: Option<String> },   // 检测到但 MVP 不实现
    DirectProcess,                        // 无 init 管理，Agent 自己托管
    Unknown { reason: String },
}

/// 停进程策略：与 systemd 的 KillMode/KillSignal/TimeoutStopSec 对齐
pub struct StopPolicy {
    pub term_timeout: Duration,   // 发 SIGTERM 后等多久
    pub kill_timeout: Duration,   // 再等多久后 SIGKILL
}
```

关键约束：

- **`ServiceManager` 不是 40 个方法的 `SystemManager`**（AGENTS.md 明令禁止）。上面 6 个方法就是全部；能力探测走另一个更小的 Port：

```rust
#[async_trait]
pub trait RuntimeProbe: Send + Sync {
    async fn os_release(&self) -> Result<OsRelease>;
    async fn init_system(&self) -> Result<InitSystemFacts>;
    async fn container(&self) -> Result<ContainerFacts>;
    async fn capabilities(&self) -> Result<CapabilityFacts>;
    async fn tun_device(&self, path: &Path) -> Result<CapabilityStatus>;
    async fn nftables(&self) -> Result<CapabilityStatus>;
}
```

- Domain 层的 `InitSystem` 是**值对象**（设计文档 §System 已定义），不含任何 `std::process` / `zbus` / 文件系统细节。
- Infrastructure 提供 `SystemdServiceManager`、`DirectProcessManager`、`OpenRcServiceManager`（后两者：Direct 在 MVP 实现，OpenRC 只留探测 + `Unimplemented` 错误）。

### 7.2 探测算法（顺序敏感）

```text
1. /run/systemd/system 是否存在？
      否 → 不是 systemd 作为运行时 init（这是 sd_booted() 的判定依据）
      是 → 继续
2. /proc/1/comm 是否等于 "systemd"？              ← 排除"装了 systemd 但 PID1 是别的"
      否 → 不是 systemd 管理
      是 → 继续
3. systemctl is-system-running （可选，需要能连上 systemd 的 private socket）
      running / degraded / maintenance → systemd 可用
      失败（no socket / 权限）           → 降级为 systemd 不可用
4. 容器检测（与 init 检测正交）：
      systemd-detect-virt --container --quiet → 0 表示在容器里，输出标识（lxc / docker / ...）
      或手工：/proc/1/environ 含 container=、/.dockerenv 存在、
              /proc/self/cgroup 含 "lxc" / "docker" / "kubepods"
5. 结果组合成 InitSystemFacts { init, container, systemd_version, is_pid1 }
```

判定要点（必须写进实现）：

| 信号 | 含义 | 陷阱 |
|---|---|---|
| `/run/systemd/system` 存在 | systemd **是**运行时 init | 仅在容器里 `apt install systemd` 不会创建它；但 `chroot` 后跑 systemd-nspawn 会 |
| `/sbin/init` 是符号链接到 `/lib/systemd/systemd` | systemd **被配置为** init | 容器里这个链接可能指向 systemd 但 PID 1 其实是别的（本机 OrbStack VM 就是 `/sbin/init -> /bin/busybox`） |
| `systemctl is-system-running` 返回 | `running` / `degraded` / `maintenance` / `starting` / `stopping` / `offline` / `unknown` | `offline` 表示 boot 未完成；`unknown` 表示不是 systemd 管的系统 |
| `CAP_SYS_ADMIN` | 决定 Agent 能否调用某些 systemd 方法 | 不能只用它判断 init 类型 |

**[实测] 本机 OrbStack Linux VM**（`docker run` 挂载 VM rootfs 后读取）：

```text
/hostroot/etc/os-release   → PRETTY_NAME="OrbStack"
/hostroot/sbin/init        → symlink -> /bin/busybox
/hostroot/run/systemd/system  → 不存在
/hostroot/usr/lib/systemd/system → 不存在
/hostroot/usr/bin/systemctl   → 不存在
```

→ 这是一个**真实的非 systemd 环境**：`/sbin/init` 存在（指向 busybox），但 `/run/systemd/system` 缺失。**因此单看 `/sbin/init` 会误判**，必须以 `/run/systemd/system` 为主信号。—— 这正好验证了探测顺序里第 1 步优先于 `init` 符号链接的判断。

### 7.3 DirectProcess fallback 的实现约束

当 `SupervisionKind::DirectProcess` 时，Agent 自己充当监督者：

```text
- fork + exec，子进程调用 setsid()（新会话，脱离控制终端）
- 不要 double-fork（那会让父进程无法 waitpid，丢失退出码）
- 显式写 pidfile：/run/proxy-agent/mihomo.pid（含 pid + 启动时间 + 二进制校验和）
  启动时间用于防止 pid 复用误判（读 /proc/<pid>/stat 的 starttime 字段比对）
- 用 pidfd_open(2) + poll 监听子进程退出（Linux 5.3+），避免 SIGCHLD 竞态
- stdout/stderr：直接继承 Agent 的，由 journald 统一收集；
  不要重定向到 mihomo 自己的日志文件（避免双重日志与轮转问题）
- 停止序列：见 §8
- 崩溃恢复：Agent 内的 supervisor task 按指数退避重启（上限 + jitter），
  与 systemd 的 StartLimit* 语义保持一致（避免两套策略打架）
```

与 systemd 形态的关键差异：**没有 cgroup 级 `KillMode=control-group`**。若 mihomo 自己 fork 出子进程，DirectProcess 无法一次收干净。→ Agent 应记录进程组 id（`setpgid`），停止时对进程组发信号。

---

## 8. 优雅启停与 restart 策略协同

### 8.1 mihomo 的信号语义（[上游源码]，快照 = v1.19.30）

`MetaCubeX/mihomo` 仓库 `Meta` 分支 `main.go:239-252` 主循环：

```go
termSign := make(chan os.Signal, 1)
hupSign  := make(chan os.Signal, 1)
signal.Notify(termSign, syscall.SIGINT, syscall.SIGTERM)
signal.Notify(hupSign, syscall.SIGHUP)
for {
    select {
    case <-termSign:
        return                      // 退出 main → 触发 defer executor.Shutdown()
    case <-hupSign:
        if err := hub.Parse(configBytes, options...); err != nil {
            log.Errorln("Parse config error: %s", err.Error())
        }
    }
}
```

可确认的事实：

| 信号 | mihomo 行为 | 证据 |
|---|---|---|
| `SIGINT` / `SIGTERM` | 从 `main()` `return`，**exit 0**；`defer executor.Shutdown()` → `listener.Cleanup()` / `tproxy.CleanupTProxyIPTables()`，另有 `post-down` 脚本 → **优雅停机** | [上游源码] `main.go`、`hub/executor/executor.go` |
| `SIGHUP` | **仅重载配置，不退出进程**（`hub.Parse`） | [上游源码] `main.go` |
| **`SIGUSR1` / `SIGUSR2`** | 全树 **无 `SIGUSR` 注册** ⇒ 默认处置 = **终止进程**。**绝对不要用它们做任何事**（例如"优雅重载"或"reopen log"） | [上游源码] 全树 grep |
| `sd_notify` / `NOTIFY_SOCKET` / `WATCHDOG_USEC` / `go-systemd` | 全树命中 **0**（958 个 `.go` + go.mod/go.sum，跨 v1.16.0…v1.19.30 复核仍为 0）⇒ **不能作为 `Type=notify` 服务** | [上游源码] |
| `SIGHUP` 重载失败是否破坏旧配置 | **失败安全**：`hub.Parse` 出错只 `log.Errorln`，因未走到 `ApplyConfig`，**旧配置继续生效** | [上游源码] `hub/hub.go` |

**SIGHUP 的三个致命陷阱（实现必须遵守）**：

1. **`configBytes` 非空时 SIGHUP 静默无效**。`hub.Parse` 逻辑是：`len(configBytes) != 0` → `executor.ParseWithBytes(configBytes)`；为空 → `executor.Parse()` → `ParseWithPath(C.Path.Config())` 才读磁盘。
   → 因此若 Agent 用 `-config <base64>` 或 `-f -`（stdin）启动 mihomo，`SIGHUP` **只重解析同一份内存字节，永远读不到新配置**。
   → **强制约束：Agent 必须以 `-d <homeDir>`（并从磁盘读配置文件）方式启动 mihomo，禁止用 `-config` / `-f -`。** 这条要作为 `ServiceManager` 实现的不变量写进代码注释与测试。
2. **重载无错误反馈**。SIGHUP 路径只有 `log.Errorln`，Agent 拿不到结构化错误。
3. **mihomo 不监听配置文件变化**（全树无 `fsnotify`；`metacubex/fswatch` 只用于 CA 证书/ECH key/resource fetcher）。写文件之后**必须显式 reload**，否则不生效。

**推荐 reload 路径（优于 SIGHUP）**：

```text
PUT /configs?force=true    ← 经 external-controller（Unix socket 或 127.0.0.1）
    → hub/route/configs.go → executor.ApplyConfig(cfg, force)
    → 有 HTTP 状态码，Agent 能区分"配置非法"与"进程不可达"
```

Agent 的 `ReloadMihomo` Use Case 应**优先走 controller API**（有错误反馈、可与 health check + rollback 组成事务）；`SIGHUP` 作为 controller 不可达时的降级路径。

> 上游官方的 systemd 推荐也印证了这一点：`.github/release/mihomo.service` 与 `Meta-Docs` 都是
> `Type=simple` + `ExecStart=/usr/bin/mihomo -d /etc/mihomo` + `ExecReload=/bin/kill -HUP $MAINPID` + `Restart=on-failure` + `RestartSec=10`。
> → **上游不推荐 `Type=notify`；mihomo 的"就绪"必须由 Agent 主动探测**（轮询 controller 的 `GET /version`，或等待端口/Unix socket 出现）。

### 8.2 Agent 停止 Mihomo 的推荐序列

```text
1. Agent 判定需要停止 mihomo（StopMihomo / RestartMihomo / Agent 自身 shutdown）
2. 标记实例状态 Stopping（拒绝新的 StartMihomo，per-instance lock）
3. 发送 SIGTERM 到 mihomo 主进程（pidfd_send_signal 或 kill(2)）
4. 等待退出，超时 T1：
   - 每 100ms 检查一次；同时读 /proc/<pid>/stat 与 pidfd
   - T1 建议 10s（mihomo 的 Shutdown 要关闭 listener、可能执行 post-down 脚本）
   - 注意：上游 executor.Shutdown() 只做 listener.Cleanup() / tproxy.CleanupTProxyIPTables() 等，
     **没有显式等待/排空已建立的连接**，因此正常情况下应该是亚秒级退出；
     10s 是给 post-down 脚本和异常路径的余量，不是预期耗时。
5. 若 T1 超时：发送 SIGKILL（先对进程组，再对单进程）
6. 等待退出，超时 T2 = 5s
7. 仍不退出 → 标记 Misconfigured/Unknown，记审计日志（这是异常路径，必须告警）
8. 执行清理：
   - 删除 /run/proxy-agent/mihomo.sock（若 mihomo 没删干净）
   - 释放端口占用检查（可选：ss -lntp 比对）
   - 更新实例状态 Stopped，广播 MihomoStatusChanged 事件
```

超时建议（与 systemd 侧对齐）：

| 场景 | T1（SIGTERM→SIGKILL） | 理由 |
|---|---|---|
| `StopMihomo`（用户主动） | **10s** | 容忍 post-down 脚本与连接收尾 |
| `RestartMihomo` | **10s** | 同上；超时会拖慢配置切换的 SLA |
| Agent 自身 shutdown（systemd 发来 SIGTERM） | **15s** | Agent 还要 flush SQLite + 停止 scheduler，systemd 侧 `TimeoutStopSec=20s` 留 5s 余量 |
| mihomo 进程僵死 | 直接 SIGKILL | 由步骤 5 兜底 |

### 8.3 与 systemd restart 策略的协同（避免"打架"）

**冲突模式**：Agent 自己也在重启 mihomo（supervisor loop），systemd 也在重启 `proxy-agent.service`（`Restart=on-failure`）。若 mihomo 崩溃导致 Agent 跟着崩，systemd 重启 Agent，Agent 又重启 mihomo，同时 systemd 的 `StartLimitBurst` 可能在计数——最终 unit 进入 `failed`，谁也没恢复。

**推荐分工（按形态区分）**：

| 形态 | 谁负责 mihomo 的崩溃恢复 | 谁负责 Agent 的崩溃恢复 |
|---|---|---|
| A（单 unit） | **Agent 的 supervisor task**（内部退避重启） | systemd（`Restart=on-failure`） |
| B（双 unit） | **systemd**（`mihomo.service` 的 `Restart=always`）；Agent 只观察，不插手 | systemd |

形态 A 下的具体参数：

```ini
# proxy-agent.service
Restart=on-failure
RestartSec=5s
# 关键：不要用 always —— 用户 systemctl stop 之后不应被拉起
# （systemd 文档：因 systemd 操作导致的停止不会触发 Restart=，所以 always 其实也安全，
#   但 on-failure 语义更明确）

# [Unit]
# 关掉 systemd 的启动限流，避免与 Agent 内部退避"双重计数"
StartLimitIntervalSec=0
```

Agent 内部退避（对应 mihomo）：

```text
第 1 次崩溃: 1s
第 2 次:      2s
第 3 次:      4s
...
上限:         60s
每次重启 + 0–500ms jitter
连续 N 次（建议 5）失败 → 进入 Backoff 状态，不再自动重启，
  写审计日志 + 发 MihomoStatusChanged(critical)，等人工介入
```

**若坚持让 systemd 也参与限流**（形态 B 或混合），用（>= systemd 254）：

```ini
Restart=on-failure
RestartSec=5s
RestartSteps=4
RestartMaxDelaySec=60s
# 产出间隔: 5s, 10s, 20s, 40s, 60s, 60s, ...
```

但**不要同时**开 Agent 内部退避和 systemd `RestartSteps`——两套指数退避会相乘，恢复时间不可预测。**明确选一边**。

版本可用性（[实测] 查发行版包版本）：

| 发行版 | systemd 版本 | `Type=exec`(240) | `Type=notify-reload`(253) | `RestartSteps`(254) | `ConfigurationDirectory::ro`(257) |
|---|---|---|---|---|---|
| Debian 12 bookworm | 252.39-1~deb12u2 | ✅ | ❌ | ❌ | ❌ |
| Debian 13 trixie | 257.13-1~deb13u1 | ✅ | ✅ | ✅ | ✅ |
| Ubuntu 22.04 jammy | 249.11-0ubuntu3.22 | ✅ | ❌ | ❌ | ❌ |
| Ubuntu 24.04 noble | 255.4-1ubuntu8.17 | ✅ | ✅ | ✅ | ❌ |

→ **MVP 基线取 systemd ≥ 249**（Ubuntu 22.04），因此：
- 用 `Type=notify`（Agent 侧）和 `Type=exec`（mihomo 侧），不用 `notify-reload`；
- reload 用 `ExecReload=`/直接发 `SIGHUP`；
- restart 退避只用 `RestartSec=` 固定值 + Agent 内部退避。

### 8.4 停止序列与 systemd 的对接

Agent 收到 systemd 的 `SIGTERM`（`KillMode=control-group`、`KillSignal=SIGTERM`、`TimeoutStopSec=20s`）后：

```text
1. 停止接受新请求（关闭 listener / 停止 accept 循环）
2. 停止 scheduler（取消所有 job，等待进行中的 job 落盘或回滚）
3. 按 §8.2 序列停止 mihomo                    ← 最多 10s
4. flush SQLite（WAL checkpoint）、原子写 state 文件  ← 最多 3s
5. 记录 shutdown 审计日志（含 reason=signal）
6. exit(0)                                    ← 总共留 ~5s 余量
```

**不要**在 `ExecStop=` 里再发一次信号：`systemd.service` 文档明确 *"Note that it is usually not sufficient to specify a command for this setting that only asks the service to terminate ... but does not wait for it to do so."* 我们直接依赖 systemd 自己的 `KillSignal=` 语义更干净。

---

## 9. deb 打包与升级语义

### 9.1 文件布局

```text
/usr/lib/proxy-agent/proxy-agent          代理二进制（非 conffile，升级覆盖）
/usr/lib/proxy-agent/mihomo               mihomo 二进制（由 Agent 的 update use case 管理，
                                           初始版本可随包安装）
/usr/lib/systemd/system/proxy-agent.service
/usr/lib/systemd/system/proxy-agent-update.service
/usr/lib/systemd/system/proxy-agent.service.d/10-proxy-only.conf   # 默认档
/usr/lib/sysusers.d/proxy-agent.conf      ← 声明系统用户/组（推荐，见 9.2）
/usr/lib/tmpfiles.d/proxy-agent.conf      ← 可选的目录兜底（推荐做法仍是 RuntimeDirectory=）
/usr/share/doc/proxy-agent/...
/usr/share/man/man8/proxy-agent.8
```

**不要**把 unit 装进 `/etc/systemd/system/`。理由：

- `/usr/lib/systemd/system/` 是**发行版/包管理**的 unit 目录，升级时被 `dpkg` 覆盖——这正是我们想要的（unit 是我们发布的产物，不是用户配置）。
- 用户自定义应通过 `/etc/systemd/system/<unit>.d/*.conf` drop-in，dpkg 永远不会碰它。
- systemd 的查找顺序是 `/etc/systemd/system` > `/run/systemd/system` > `/usr/lib/systemd/system`，因此 drop-in 天然覆盖。

### 9.2 用户/组创建：用 `sysusers.d` 而不是 postinst 命令

`/usr/lib/sysusers.d/proxy-agent.conf`：

```text
# Type  Name         ID   GECOS
u  proxy-agent    -    "Mihomo management agent"
u  proxy-agent   -    "Mihomo management agent"
g  proxyctl      -    "Users allowed to talk to proxy-agent"
m  proxy-agent   proxyctl
```

`sysusers.d` 的优点是**声明式、幂等、可被 `systemd-sysusers` 在任何时候重放**（`[上游文档]` systemd.sysusers.d(5)）。Debian 的 `systemd` 包已经在 postinst 里调用 `systemd-sysusers`，所以包只需要 drop 这个文件。

> 注意：`sysusers` 创建的**系统用户密码字段为 `!`、shell 为 `/usr/sbin/nologin`、home 为 `/`**。这对服务用户是正确的。`proxyctl` 组不需要用户。

### 9.3 postinst 应该做什么 / 不应该做什么

**应该做**：

```bash
#!/bin/sh
set -e
# 1. 应用 sysusers.d 声明（幂等）
systemd-sysusers proxy-agent.conf || true

# 2. 创建持久目录（幂等；不要 chown -R 整个 /var/lib，避免碰到未知文件）
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/configs
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/subscriptions
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/cache
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/state
install -d -o proxy-agent -g proxy-agent -m 0750 /etc/proxy-agent

# 3. 只放"默认配置"的模板，绝不覆盖已存在的用户配置
if [ ! -e /etc/proxy-agent/config.toml ]; then
    install -o proxy-agent -g proxy-agent -m 0640 \
        /usr/share/proxy-agent/config.toml.default /etc/proxy-agent/config.toml
fi

# 4. 刷新 systemd（不 enable、不 start）
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi

# 5. 明确告知用户：安装完成但未启用
cat <<'EOF'
proxy-agent installed but NOT enabled.
  Enable and start:  systemctl enable --now proxy-agent.service
  Then run:          proxy-agent doctor
To grant a user local API access, add them to the 'proxyctl' group.
EOF
```

**不应该做**：

| 反模式 | 为什么 |
|---|---|
| `systemctl enable proxy-agent` | 静默改变系统启动行为；应留给用户/显式安装器 |
| `systemctl start proxy-agent` | 用户可能还没配好配置；首次启动应由 `doctor` 前置 |
| 覆盖 `/etc/proxy-agent/config.toml` | 破坏用户配置 |
| `--force-confnew` 之类的 dpkg 选项 | 同上 |
| 在 postinst 里下载 mihomo 二进制 | 安装脚本不应在安装时联网拉取不受信任的产物；应由 `proxy-agent mihomo update` 显式执行并校验 |
| `chown -R proxy-agent /var/lib/proxy-agent` | 可能改变用户手动放进来的文件的属主；只 chown 我们创建的目录 |

### 9.4 conffile / dpkg 升级语义

- `debian/conffiles`（或 dh 的 `--with conffiles` 自动识别 `/etc` 下的文件）声明 `/etc/proxy-agent/config.toml` 为 **conffile**。
- 升级时若用户改过 conffile、包内版本也变了，dpkg 会**交互式提问**（默认保留用户版本，并生成 `.dpkg-dist`）。
- **绝不要**在 maintainer script 里手工覆盖 conffile——这会绕过 dpkg 的三方合并逻辑。
- `config.toml` 里**不要放 secret**（放 `/etc/proxy-agent/secrets.toml`，mode 0600，且该文件**不**声明为 conffile，由使用者在 UI 里生成）。

参考：[deb-conffiles(5)](https://manpages.debian.org/bookworm/dpkg/deb-conffiles.5.en.html)、[deb(5)](https://manpages.debian.org/bookworm/dpkg/deb.5.en.html)

### 9.5 升级时不要动的东西

```text
/var/lib/proxy-agent/**        永久用户数据——dpkg 不管理，升级不得触碰
/run/proxy-agent/**            tmpfs，重启即失
/etc/systemd/system/proxy-agent.service.d/**  用户 drop-in，升级不得触碰
/etc/proxy-agent/config.toml                    conffile 语义保护
```

### 9.6 prerm / postrm

```bash
# prerm remove
if [ -d /run/systemd/system ]; then
    systemctl stop proxy-agent.service 2>/dev/null || true
fi

# postrm purge（仅 purge 时）
rm -rf /var/lib/proxy-agent /etc/proxy-agent
# 不要删除 proxy-agent 用户/组（可能被其他文件引用）；
# 若要删除，也只在 purge + 目录已空时执行
```

### 9.7 enable 的时机

- 包本身**不 enable**。
- `install.sh`（`curl | sh` 安装器）在完成 `doctor` 检查并取得用户确认后，可以执行 `systemctl enable --now`。
- 这样「deb 用户」和「一键脚本用户」的行为差异是显式的。

---

## 10. OpenRC / 非 systemd 的 defer 决策

### 10.1 为什么 MVP 只做 systemd

1. **目标环境**：AGENTS.md 与设计文档都锁定 `Debian/Ubuntu + systemd + PVE LXC`；Debian 12+/Ubuntu 22.04+ 默认 systemd。OpenRC 主要在 Alpine/Gentoo/Artix，不在 MVP 目标内。
2. **systemd 提供了 MVP 直接依赖的原语**：`RuntimeDirectory=`（socket 目录生命周期）、`CapabilityBoundingSet=`/`AmbientCapabilities=`（capability 收敛）、`PrivateDevices=`/`DeviceAllow=`（TUN 设备控制）、`KillMode=control-group`（整组回收）、`Restart=` 限流。OpenRC 侧这些要靠 `start-stop-daemon` + `openrc-run` 手写，语义**不等价**（尤其 cgroup 回收和 capability 管理）。
3. **测试成本**：本项目**没有任何 systemd 实机测试条件**（宿主是 macOS；容器内也拿不到 systemd PID 1）。再支持 OpenRC 会让"未验证"面积翻倍。
4. **收益/成本比**：OpenRC 用户占比低，但适配成本高（要写 `mihomo.openrc`、验证 `start-stop-daemon --chuid/--capabilities`、处理 `openrc-run` 的 `supervise-daemon` 差异）。defer 是理性的。

### 10.2 抽象上要留的口子

**要留**：

- `ServiceManager` / `RuntimeProbe` 两个 Port（§7.1），Application 层只认 `SupervisionKind`，不认 `systemctl`。
- `InitSystem` 值对象在 Domain 层**已经有 OpenRC 的取值位**（设计文档 §System/`InitSystem`），探测代码直接返回 `OpenRc`，只是 `ServiceManager` 实现返回 `ApplicationError::Unsupported`。
- 探测逻辑不假设「有 systemd 就用 systemd」：`/run/systemd/system` + PID1 + `systemctl is-system-running` 三步（§7.2）。
- 目录生命周期不硬编码 `/run/proxy-agent`：由 `RuntimeDirectory=` 注入的 `$RUNTIME_DIRECTORY` 优先，回退到默认路径。这样 OpenRC 下可以由 init 脚本创建 `--rundir`。
- `doctor` 输出里 `Init` 一行支持 `systemd (257.13)` / `openrc (0.56)` / `unknown`（设计文档 §21 的格式已预留）。

**不要留**（避免过度抽象）：

- 不要做「通用 init 适配层 DSL」；
- 不要为 OpenRC 预写空实现/占位 unit 文件；
- 不要把 systemd 特有概念（unit、drop-in、`daemon-reload`）泄漏进 Application 层接口。比如 Port 里**不能**出现 `reload_daemon()`，只能有 `reload_data_plane()`。

### 10.3 非 systemd 环境的具体处理

| 环境 | 检测结果 | Agent 行为 |
|---|---|---|
| PVE LXC（有 systemd） | `Systemd` | 正常 |
| PVE LXC（无 systemd，容器内无 init） | `DirectProcess` | 自己托管 mihomo；`doctor` 报告 `Init: direct-process`，Mihomo 生命周期功能仍可用 |
| Docker 容器 | `DirectProcess`（PID1 是 entrypoint） | 同上；Web API 可用；systemd 相关 Use Case 返回 `Unsupported` |
| OrbStack VM / busybox init | `DirectProcess`（**实测**：`/run/systemd/system` 缺失，`/sbin/init -> busybox`） | 同上 |
| OpenRC 发行版 | `OpenRc` | `doctor` 正确报告；`StartMihomo` 返回 `ApplicationError::Unsupported { init: OpenRc }`；Web UI 隐藏相关按钮 |

**关键不变量**：一个功能不可用**不得**导致无关功能失败（AGENTS.md 的 PVE LXC 规则）。即：无 systemd → 仍然能用 direct-process 起 mihomo → 仍然能走 HTTP/SOCKS 代理。这是合法的降级状态。

---

## 11. 对 Agent 架构的影响

### 11.1 需要新增/确认的 Port

| Port | 层 | 职责 | 关键约束 |
|---|---|---|---|
| `ServiceManager` | Application（定义）/ Infrastructure（实现） | 数据面进程的启停、重载、状态观察 | 只暴露领域词汇；不泄漏 unit/drop-in |
| `RuntimeProbe` | Application / Infrastructure | OS、init、容器、capability、TUN、nftables 探测 | 返回五态 `CapabilityStatus`，不是 bool |
| `ProcessManager` | Application / Infrastructure | 通用子进程执行（**带 argv 白名单**） | 见 11.3：绝不允许任意命令 |
| `Firewall` | Application / Infrastructure | nftables/iptables 规则的原子应用与回滚 | 需要 `CAP_NET_ADMIN`；失败必须回滚到上一套规则 |

### 11.2 单 unit 形态对 Application 层的直接后果

```text
- StartMihomo / StopMihomo / RestartMihomo 在单 unit 形态下是"进程操作"，
  由 ServiceManager 的 DirectProcess 实现完成，不经过 D-Bus。
- Agent 重启（systemd Restart=）会导致 Mihomo 重启（同 cgroup）。
  因此"配置激活"事务必须幂等且能在启动时恢复：
  启动时读 state/active 指向的 config version，重新校验 checksum 后拉起 mihomo。
- 需要 per-instance lock 串行化（AGENTS.md 要求）：Starting→Starting 必须被拒绝。
- 状态不能只靠内存：supervisor 需要把"当前 mihomo pid + 启动时间 + config version"
  持久化到 /var/lib/proxy-agent/state/mihomo.json，以便 Agent 重启后接管遗留进程
  （单 unit 形态下系统保证不会有遗留，但 direct-process 形态会有）。
```

### 11.3 安全边界的硬性约束（与 AGENTS.md 对齐）

```text
- Web/API 绝不暴露任意 shell：没有 POST /api/run-command。
- ProcessManager 只接受结构化命令（枚举 + 类型化参数），不接受字符串 argv。
  允许的命令集合是编译期枚举，例如：
      enum AgentCommand {
          StartDataPlane { config: ConfigPath },
          StopDataPlane  { handle: Handle },
          ValidateConfig { path: ConfigPath },
          HashFile       { path: PathBuf },
      }
- mihomo 的 post-up/post-down 脚本能力（--post-up/--post-down flag）绝不透出给 API：
  Agent 自己不设置这两个 flag，也不允许通过 API 注入。
  （注意上游是用 `/bin/sh -c` 执行它们的——见 common/cmd；一旦暴露就是任意命令执行。）
- spawn mihomo 的 argv 是**编译期固定的模板**：
      [ "<mihomo>", "-d", <home_dir>, "-f", <config_path> ]
  其中 config_path 必须是 Agent 自己管理的、校验过 checksum 的路径。
  绝不允许用 "-config"/"-f -" 形式（SIGHUP 陷阱 + 便于注入 base64 配置）。
- 绝不向 mihomo 发送 SIGUSR1/SIGUSR2（默认处置 = 终止进程）。
- 审计日志必须覆盖：mihomo.start / mihomo.stop / mihomo.restart / mihomo.update /
  config.activate / config.rollback / firewall.apply（设计文档 §57）。
```

### 11.4 与既有设计文档的差异点（需要 ADR 收口）

本文与 `项目设计文档` 的**一致**之处：单 unit 模型（§23）、独立服务用户 `proxy-agent`（§55）、最小 capability（§55）、Web 不 exec 任意命令（§56）。

本文**补充/细化**之处（实现前建议写成 ADR）：

1. Agent 以 root 启动 + sandbox 收敛，而非以非 root 用户运行（§6.4 方案 A）。设计文档只说"独立 system user"，未说明 systemd 操作如何授权；本文给出了 D-Bus/polkit 的官方默认策略证据并据此选型。
2. hardening 分两档（`proxy-only` / `tun-enabled`）并给出 TUN 冲突清单（§4.3）。设计文档未涉及。
3. 二进制更新拆成独立 oneshot unit，常驻 Agent 不持有 `/usr` 写权限（§6.2）。设计文档未涉及。
4. `mihomo.sock` 被上游硬编码 `chmod 0666`，因此只靠目录权限隔离（§5.3）。设计文档未涉及。
5. 用 `Restart=on-failure` + **Agent 内部退避**（单 unit 形态）或 **systemd `RestartSteps`**（双 unit 形态），二者不叠加（§8.3）。

---

## 12. 证据与来源

### 12.1 systemd 官方 man pages（systemd 261.2，`latest`）

| 文档 | 链接 | 本文引用点 |
|---|---|---|
| `systemd.service(5)` | https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html | `Type=simple/exec/forking/notify/notify-reload` 语义、`NotifyAccess=`、`Restart=`、`RestartSec=`、`RestartSteps=`、`TimeoutStartSec=`、`TimeoutStopSec=`、`ExecStop=` 的注意事项 |
| `systemd.exec(5)` | https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html | `User=`/`Group=`/`DynamicUser=`、`*Directory=` 家族、`CapabilityBoundingSet=`、`AmbientCapabilities=`、`SecureBits=`、`NoNewPrivileges=`、`ProtectSystem=`、`ProtectHome=`、`PrivateTmp=`、`PrivateDevices=`、`ProtectKernelTunables=`、`ReadWritePaths=`、`RestrictAddressFamilies=`、`SystemCallFilter=`、`MemoryDenyWriteExecute=`、`LockPersonality=` |
| `systemd.kill(5)` | https://www.freedesktop.org/software/systemd/man/latest/systemd.kill.html | `KillMode=control-group/mixed/process/none`、`KillSignal=`、`RestartKillSignal=`、`SendSIGHUP=`、`SendSIGKILL=`、`FinalKillSignal=`、`WatchdogSignal=` |
| `systemd.resource-control(5)` | https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html | `DevicePolicy=strict/closed/auto`、`DeviceAllow=`、`DevicePolicy=` 不可被 `"+"` 绕过 |
| `systemd.unit(5)` | https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html | `StartLimitIntervalSec=`/`StartLimitBurst=` 的准确语义与默认值来源 |
| `systemd.sysusers.d(5)` | https://www.freedesktop.org/software/systemd/man/latest/sysusers.d.html | `sysusers.d` 声明式用户创建 |
| `systemd-detect-virt(1)` | https://www.freedesktop.org/software/systemd/man/latest/systemd-detect-virt.html | `--container` / `--vm`、LXC 检测标识 |

### 12.2 Linux 内核 / capability 文档

| 文档 | 链接 | 引用点 |
|---|---|---|
| `capabilities(7)` | https://man7.org/linux/man-pages/man7/capabilities.7.html | ambient set 定义与 `execve()` 变换公式、`PR_CAP_AMBIENT_RAISE` 的 `CAP_SETPCAP`/NNP 规则、`SECBIT_NO_CAP_AMBIENT_RAISE`、CAP_NET_ADMIN/CAP_NET_RAW/CAP_KILL/CAP_SETPCAP 语义 |
| `no_new_privs` kernel docs | https://raw.githubusercontent.com/torvalds/linux/master/Documentation/userspace-api/no_new_privs.rst | NNP 只阻止 `execve()` **获得**新特权，不剥夺已有 ambient set |
| `capabilities(7)`（bounding set） | 同上 | bounding set 在 `execve()` 中限制 permitted set 的获得 |

### 12.3 systemd 上游源码（可复核）

| 文件 | 链接 | 引用点 |
|---|---|---|
| `src/core/exec-invoke.c` | https://github.com/systemd/systemd/blob/main/src/core/exec-invoke.c | ambient cap 提升在 `setresuid()` 前后各一次；`proc_set_nnp()` 在 ambient 之后调用 |
| `src/basic/capability-util.c` | https://github.com/systemd/systemd/blob/main/src/basic/capability-util.c | `capability_ambient_set_apply()`：先剔除不在 bounding set 里的 ambient cap，再 `PR_CAP_AMBIENT_RAISE` |
| `src/core/org.freedesktop.systemd1.policy.in` | https://github.com/systemd/systemd/blob/main/src/core/org.freedesktop.systemd1.policy.in | `org.freedesktop.systemd1.manage-units` 默认 `allow_any=auth_admin` |
| `NEWS` | https://raw.githubusercontent.com/systemd/systemd/main/NEWS | 版本归属：`Type=exec`=240、`Type=notify-reload`=253、`*Directory=` 家族=235、`RuntimeDirectoryPreserve=`=235 |

### 12.4 Mihomo 上游源码（`Meta` 分支）

| 文件 | 链接 | 引用点 | 等级 |
|---|---|---|---|
| `main.go` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/main.go | `signal.Notify(SIGINT, SIGTERM)` → `return`（exit 0，`defer executor.Shutdown()`）；`signal.Notify(SIGHUP)` → `hub.Parse` 重载、失败仅记日志且旧配置保留；无 `SIGUSR*` 注册（默认终止）；无 `sd_notify`/`NOTIFY_SOCKET`/`WATCHDOG_USEC`/`go-systemd`；无 fork/setsid（前台运行）；`-d`/`-f`/`-config`/`--ext-ctl-unix`/`--post-up`/`--post-down` flags | [上游源码] |
| `hub/hub.go` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/hub/hub.go | `Parse()`：`configBytes` 非空 → 只重解析内存字节；为空 → `executor.Parse()` 重读配置文件。**这是 SIGHUP 陷阱的根因** | [上游源码] |
| `hub/route/configs.go` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/hub/route/configs.go | `PUT /configs?force=true` → `executor.ApplyConfig(cfg, force)`，**有 HTTP 错误反馈** → Agent 首选 reload 路径 | [上游源码] |
| `hub/executor/executor.go` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/hub/executor/executor.go | `Shutdown()`：`listener.Cleanup()` / `tproxy.CleanupTProxyIPTables()` / `resolver.StoreFakePoolState()`；**无显式连接排空**，因此停止超时不宜过长也不宜过短 | [上游源码] |
| `hub/route/server.go` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/hub/route/server.go | `startUnix()`：父目录 `MkdirAll(0o755)`；socket `os.Chmod(addr, 0o666)`；`router(cfg.IsDebug, "", ...)` **secret 传空 ⇒ unix socket 不做认证**；相对路径按 homeDir 解析 | [上游源码] |
| `.github/release/mihomo.service` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/.github/release/mihomo.service | 上游官方推荐：`Type=simple` + `ExecStart=/usr/bin/mihomo -d /etc/mihomo` + `ExecReload=/bin/kill -HUP $MAINPID` + `Restart=on-failure` + `RestartSec=10` + `LimitNOFILE=infinity` | [上游源码] |
| `Meta-Docs` service 文档 | https://raw.githubusercontent.com/MetaCubeX/Meta-Docs/main/docs/startup/service/index.md | 上游文档给出与上一致的 `Type=simple` + SIGHUP reload 模型（**不是 `Type=notify`**） | [上游文档] |
| `config/config.go` | https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/config/config.go | `ExternalControllerUnix` 自 **v1.18.4** 引入（v1.18.3 无）；配套 CLI flag `-ext-ctl-unix` 同版本 | [上游源码] |
| `common/cmd/cmd_windows.go` | — | 全树唯一 `SysProcAttr` 命中（`HideWindow`，执行 post-up/down 脚本用），证明无 daemonize | [上游源码] |

> 快照口径：[上游源码] 结论基于 **v1.19.30**（`Meta` 分支 HEAD 经 md5 比对与 tag v1.19.30 一致；958 个 `.go` 全树 grep）。

### 12.5 发行版版本（[实测] `packages.debian.org` / `packages.ubuntu.com`，2026-09-12）

| 发行版 | systemd 版本 | 来源 |
|---|---|---|
| Debian 12 bookworm | `252.39-1~deb12u2` | https://packages.debian.org/bookworm/systemd |
| Debian 13 trixie | `257.13-1~deb13u1` | https://packages.debian.org/trixie/systemd |
| Ubuntu 22.04 jammy | `249.11-0ubuntu3.22` | https://packages.ubuntu.com/jammy/systemd |
| Ubuntu 24.04 noble | `255.4-1ubuntu8.17` | https://packages.ubuntu.com/noble/systemd |

### 12.6 实测环境（本机，2026-09-12）

```text
宿主:        macOS (Darwin, arm64) —— 无 systemd
容器运行时:  Docker 29.4.0 (OrbStack), runc, cgroup v2
Docker Hub:  不可达（无法 pull 新镜像）—— 只能使用本地已缓存镜像
可用镜像:    agentscope/copaw:latest (含 python3 + gcc), redis:8-alpine, postgres:18, ...

实测 1 [实测] OrbStack Linux VM 的 init 形态：
    /etc/os-release          PRETTY_NAME="OrbStack"
    /sbin/init               -> /bin/busybox
    /run/systemd/system      不存在
    /usr/lib/systemd/system  不存在
    /usr/bin/systemctl       不存在
    → 真实的"非 systemd 但有 /sbin/init"样本，验证了探测顺序必须以
      /run/systemd/system 为主信号。

实测 2 [实测] ambient capability × NoNewPrivs × execve（自编译 C 探针）：
    capset(prm|=CAP_NET_ADMIN, inh|=CAP_NET_ADMIN)
    prctl(PR_CAP_AMBIENT_RAISE, CAP_NET_ADMIN)         -> OK
    prctl(PR_SET_NO_NEW_PRIVS, 1)                      -> OK, CapAmb 保持
    fork() + execve()                                  -> 子进程 CapAmb/CapEff 均含 CAP_NET_ADMIN, NoNewPrivs=1
    → 证伪"NNP 会清空 ambient cap"的常见误解；支持 C4 结论。

未做（原因见 §13）：
    真正的 systemd 单元测试（拿不到带 systemd PID1 的镜像）
    TUN 设备创建测试（容器内无 /dev/net/tun）
    nftables 规则写入测试
```

---

## 13. 未验证假设与开放问题

### 13.1 明确标记为「未验证」的断言

| # | 断言 | 等级 | 影响 | 建议验证方式 |
|---|---|---|---|---|
| U1 | systemd 的 `AmbientCapabilities=` + `NoNewPrivileges=yes` 组合在**真实 systemd 启动路径**下保留 cap | [未验证]（间接证据：[上游源码] `exec-invoke.c` 顺序 + [实测] 内核语义） | 若失败，TUN 档必须去掉 `NoNewPrivileges=yes`（安全性下降） | 在 Debian 13 VM/LXC 上部署最小 unit，`capsh --print` 验证 |
| U3 | Agent 能否在 spawn mihomo 后立即从**自身** effective set 丢弃 `CAP_NET_ADMIN`（保留在 permitted/ambient），从而缩小 Web 攻击面 | [推测] | 若能，Agent 进程自身不再持有网络特权，安全性显著提升 | Rust 侧用 `caps` crate 或 raw `capset()` 做 PoC；验证下次 spawn 时 cap 仍在 ambient/permitted |
| U4 | mihomo 创建 TUN 设备是否**只需** `CAP_NET_ADMIN`（不需要 `CAP_NET_RAW`） | [未验证] | 影响 `AmbientCapabilities` 最小集 | 在 TUN 可用环境（PVE LXC privileged 或 VM）用 `capsh --drop=cap_net_raw` 实测 |
| U5 | `MemoryDenyWriteExecute=yes` 对 mihomo（Go）与 Agent（Rust）在**所有代码路径**下都安全 | [推测]（两者均无 JIT；但 mihomo 若加载纯 Go 之外的插件/子进程则有风险） | 若有影响，需在 tun 档去掉该指令 | 在 Linux 上跑完整 mihomo 功能集（TUN + nftables + DNS 劫持）验证 |
| U6 | polkit 规则 `action.lookup("unit")` 的语法与 `proxy-agent.service` 调用 systemd 时的 subject 类型（system bus 上无 session，`subject.user` 是否可用） | [未验证] | 影响形态 B 的可行性 | 在 Debian 13 上写规则 + 用 `busctl` 模拟调用 |
| U7 | Debian/Ubuntu 的 `dpkg` 是否自动把 `/etc/proxy-agent/config.toml` 识别为 conffile | [推测]（dh 的默认行为是 `/etc` 下文件走 conffiles，但需确认 `dh_installdeb` 版本行为） | 影响升级时是否覆盖用户配置 | 构建一次 deb，`dpkg-deb -I` 检查 `conffiles` 列表 |
| U8 | `RuntimeDirectory=` 在 unit 停止时删除 `/run/proxy-agent` 是否会影响 mihomo 的 socket 清理竞态 | [推测] | 低风险 | 实机观察 |

> **已被上游源码解决的旧假设**：本文初稿曾把「SIGHUP 重载失败是否会破坏正在运行的旧配置」列为 `[未验证]`。经 `Meta` 分支 `hub/hub.go` 源码确认：`hub.Parse` 出错时 **不调用 `ApplyConfig`，旧配置继续生效**（失败安全）。因此该条已升级为 `[上游源码]`，见 §8.1。

### 13.2 开放问题（需要 ADR 或后续实验收口）

- **Q1**：形态 A（单 unit）与形态 B（双 unit）最终选哪个？本文推荐 MVP 用 A，但需要确认「Agent 重启 → Mihomo 必重启」造成的短暂代理中断是否可接受（对长连接用户是可见的）。若不可接受，B 更优但需要 polkit 规则（U6）。
- **Q2**：Agent 需要 `CAP_NET_ADMIN` 才能执行 `nft list ruleset`；但 Agent **自身**并不需要改 nftables（那是 mihomo 或独立 helper 的事）。是否应该让 `doctor` 在无 `CAP_NET_ADMIN` 时返回 `nftables: Unknown` 并把「透明代理可用性」标为待确认，而不是强行给 Agent 加 cap？本文倾向后者（少给权限），但需要产品层确认 `doctor` 的期望输出。
- **Q3**：二进制更新的边界（§6.2）——独立的 `proxy-agent-update.service` 是否足够？还是应该走 `proxy-agent` CLI + polkit 的 `org.freedesktop.policykit.exec`？前者简单但需要 Agent 能触发 systemd unit（又回到 Q1 的 D-Bus 授权问题）。
- **Q4**：是否要用 systemd socket activation（`.socket` unit）来托管 `agent.sock`？好处是 socket 生命周期与 systemd 绑定；坏处是 `accept()` 前的鉴权窗口和 `NotifyAccess` 复杂度。本文建议 MVP 不用，记为 Q4。
- **Q5**：`RuntimeDirectory=` 的 `RuntimeDirectoryPreserve=` 取值。若 Agent 需要跨重启保留 mihomo 的 socket（形态 B），需要 `restart`；单 unit 形态下 `no` 即可。
- **Q6**：多实例（`proxy-agent@.service` 模板）虽然 MVP defer，但 unit 设计上是否现在就用 `%i` specifier 预留？本文建议**不要**提前加模板（AGENTS.md：不要无具体需求引入抽象），但目录命名要避免成为障碍——即 `/var/lib/proxy-agent` 而非 `/var/lib/proxy-agent/<instance>`，将来加模板时用 `StateDirectory=proxy-agent/%i`。

### 13.3 本文的测试局限（必须向用户声明）

- **没有在真实 systemd PID 1 下运行过任何 unit**。宿主是 macOS；OrbStack 容器默认不是 systemd init；Docker Hub 不可达导致无法拉取 `jrei/systemd-*` 之类的镜像。
- 所有 systemd 指令语义均来自 **systemd 261.2 官方 man page 全文**（已下载核读）与 **systemd 上游源码**（`exec-invoke.c`、`capability-util.c`、`org.freedesktop.systemd1.policy.in`、`NEWS`）。
- 内核侧 ambient/NNP 语义做了真实 Linux 内核上的 C 探针实测（见 §4.2 / §12.6 实测 2）。
- mihomo 信号语义与 controller socket 行为来自 **`Meta` 分支源码实读**（`main.go`、`hub/route/server.go`）。
- 分发版 systemd 版本号来自 `packages.debian.org` / `packages.ubuntu.com` 实查。
- **落地前必须在 Debian 13 / Ubuntu 24.04 的 VM 或 privileged LXC 上完成**：`systemd-analyze verify proxy-agent.service`、`systemd-analyze security proxy-agent.service`、TUN 档的 `capsh --print` + 实际 TUN 设备创建、`nft list ruleset`、以及 U1/U4/U5 三项（ambient×NNP 保留、TUN 是否只需 `CAP_NET_ADMIN`、`MemoryDenyWriteExecute=` 对完整 mihomo 功能集的影响）。
