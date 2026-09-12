# R03 — Mihomo Runtime 与进程管理

> 状态：已完成（由原调研证据重建） | 调研日期：2026-09-12 | 证据等级：实测为主 + 上游源码 + 上游文档
> 关键结论一句话：mihomo **不 daemonize、不写 PID 文件、不实现 `sd_notify`**，唯一优雅停机信号是 `SIGTERM`/`SIGINT`（`SIGUSR1`/`SIGUSR2` 未注册 → 默认处置 = 杀死进程），`SIGHUP` 只做原地 reload（坏配置不致命，仅 `error` 日志）；controller bind 失败与 mixed-port bind 失败**都不致命**（仅 error 日志，进程继续），因此"进程存活"绝不等于"代理可用"——Health Check 必须分层，Port 必须拆成 `ProcessManager` / `MihomoController` / `HealthChecker`。
>
> ⚠️ **重建声明**：本文件由原调研子任务遗留在 `/tmp/r03-runtime/` 的实测证据重建，未重新执行实验。原实验脚本的**汇总 stdout**（脚本自身的 `echo` 输出，含 `$EPOCHREALTIME` 计时数字、退出码、`ls -l` 权限行）**未落盘**，仅保留了每个 case 的 mihomo 进程日志（`logs/*.out` / `*.err`）与少量伴随文件。因此凡是"耗时/socket 权限/退出码"这类**只存在于汇总 stdout** 的结论，本文一律标注 `[未验证]` 或"证据不足"，**绝不从日志时间戳反推并冒充实测数字**。§14 给出完整映射与缺口清单。

---

## 1. 结论摘要（TL;DR）

1. **进程模型：前台、单进程、无 daemonize、无 PID 文件。** `mihomo -h` 的 flag 列表中不存在 `--daemon`/`-D`/`--background`/`--pidfile` 任何一个；`-d` 语义是 "set configuration directory"（配置目录），不是 daemonize。[实测 `help.txt`][上游源码 `main.go`]

2. **信号语义（最关键）：**
   - `SIGTERM` / `SIGINT` → **优雅停机**：日志出现 `level=warning msg="Mihomo shutting down"`，进程退出。上游源码路径是 `main()` 的 `return` → `defer executor.Shutdown()`。[实测 `sig_term.out`、`sig_int.out`；`s2_*` 系列同][上游源码 `main.go`、`hub/executor/executor.go`]
   - `SIGHUP` → **原地 reload，进程不退出、PID 不变**。日志出现第二组 `Start initial configuration in progress` → `Initial configuration complete` → `External controller serve error: http: Server closed` → `RESTful API listening at: ...`。[实测 `sig_hup_nop.out`、`sig_hup_reload.out`、`s2_hup.out`、`s3_hupwin.out`、`x_logs.out`、`x_rename.out`][上游源码 `main.go`]
   - `SIGHUP` + **坏配置** → **仅打 `level=error msg="Parse config error: ..."`，进程继续存活，旧配置继续生效**。这是本文件最重要的一条"失败安全"结论。[实测 `sig_hup_broken.out`][上游源码 `hub/hub.go`]
   - `SIGUSR1` / `SIGUSR2` → 实测发送后进程**在 2 秒观察窗内仍然存活**（`s2_usr1.out`/`s2_usr2.out`/`s3_dfl*.out` 中均**无** `Mihomo shutting down`，日志停在 `Start initial compatible provider default`），与上游源码"未注册 `SIGUSR`"方向一致；但**存活并不等于"被忽略"**——见 §5.3 的证据缺口。[实测 `s2_usr1.out`、`s2_usr2.out`、`s3_dfl.out`、`s3_dfl2.out`][上游源码]

3. **就绪信号是日志行 + `GET /version` 200**，而不是 `sd_notify`。就绪日志行为 `level=info msg="RESTful API listening at: 127.0.0.1:29555"`；同时 proxy listener 就绪行为 `Mixed(http+socks) proxy listening at: 127.0.0.1:29556`。**两条行是分开的，controller 先于/独立于 proxy 就绪。** [实测，全量 `*.out`]

4. **端口冲突不致命、也不降级上报。** controller 端口被占 → `level=error msg="External controller listen error: ... bind: address already in use"`，进程**继续存活**，mixed-port 照常监听；mixed-port 被占 → `level=error msg="Start Mixed(http+socks) server error: ... bind: address already in use"`，进程**继续存活**，controller 照常监听。[实测 `m_port.out`、`m_port2.out`、`y_dup.out`] 与 R01 §2.4 的既有发现一致。[`docs/research/01-mihomo.md`]

5. **`-d` 目录不存在时自动创建 + 自动生成默认配置，但默认配置绑 `7890` 会撞端口。** `level=info msg="Can't find config, create a initial config file"`，随后默认配置 `mixed-port: 7890` 因端口占用报 error（进程仍存活）。[实测 `m_nodir.out` + `nodir/config.yaml`、`nodir/cache.db`]

6. **Unix socket：需显式配置 `external-controller-unix`；日志行 `RESTful API unix listening at: <path>`；文件权限实测 `srw-rw-rw-`（0666）；父目录不存在时会被自动创建（`drwxr-xr-x`，0755）。** socket 文件在进程被 `SIGKILL` 后**残留**（`rt/mihomo.sock` 至今仍在磁盘上），因为 `Shutdown()` 的清理路径没跑到。[实测 `m_unix.out`/`m_unix2.out`/`m_unix3.out` + 磁盘残留 `/tmp/r03-runtime/rt/mihomo.sock`、`rt2/mihomo.sock`；R01 §1.3 同]。**但"`curl --unix-socket` 是否可用/是否校验 secret"的结论只存在于实验汇总 stdout，本次重建无法从留存文件中复现数字，标为 `[未验证]`，见 §6。**

7. **`/logs` 是 NDJSON（每行一个 JSON），不是 WebSocket-only。** 实测 `logs_std.txt` 与 `logs_struct.txt` 说明：标准格式为 `{"type":"info","payload":"..."}`，结构化格式（`?format=structured`）为 `{"time":"HH:MM:SS","level":"info","message":"...","fields":[]}`。[实测 `logs_std.txt`、`logs_struct.txt`]

8. **Health Check 必须四层**：进程存活 / controller 可达 / 配置已加载 / 代理端口监听。**这四层在实测中能相互独立地失败**（controller 挂了但 mixed-port 活着、mixed-port 挂了但 controller 活着）。任何"单层探测"都会误报。[实测 `m_port.out`、`m_port2.out`、`y_dup.out`][推测：分层定义本身是设计推导]

9. **Port 划分：拆成三个是合理的**，因为它们对应三个可独立失败的观测面与两套不同的通信机制（Rust 子进程 handle vs HTTP over TCP/Unix）。草案见 §10。[推测，基于 §7/§9 的证据]

10. **重启分工：MVP（单 unit）由 Agent 内部退避重启，systemd 只负责拉起 Agent**；双 unit（形态 B）则由 systemd 管 mihomo，Agent 不插手。**绝不允许两套退避叠加。**[上游文档 `systemd.service`；对齐 `docs/research/09-linux-runtime.md` §8.3]

---

## 2. 实测环境与版本

| 项 | 值 | 证据 |
|---|---|---|
| mihomo 版本 | `Mihomo Meta v1.19.30 darwin arm64 with go1.26.6 Sun Aug 16 10:01:05 UTC 2026`，`Use tags: with_gvisor` | [实测 `logs/v1.txt`、`v2.txt`、`v3.txt`（三次输出字节数均为 99，完全一致）] |
| 宿主 | macOS Darwin arm64（Apple Silicon） | [实测；与 R01 同环境] |
| 实验配置目录 | `/tmp/r03-runtime/home` | [实测 `home/config.pristine.yaml`] |
| controller | `127.0.0.1:29555`，`secret: r03secret` | [实测 `home/config.pristine.yaml`、`home/config.yaml`] |
| proxy 端口 | `mixed-port: 29556`，`bind-address: 127.0.0.1` | [实测 `home/config.pristine.yaml`] |
| 实验脚本 | `exp_startup.sh` / `exp_signals.sh` / `exp_signals2.sh` / `exp_signals3.sh` / `exp_misc.sh` / `exp_more.sh` / `exp_final.sh` | [实测 `/tmp/r03-runtime/exp_*.sh`] |

> ⚠️ **平台差异声明（与 R01 一致）**：本次实测在 **darwin/arm64** 上完成，目标平台是 **Linux + PVE LXC**。信号语义与进程/文件行为绝大多数是**平台无关的 Go 代码路径**（`os/signal`、`os.MkdirAll`、`os.Chmod`、`net.Listen`），可跨平台信任到"行为形状"这一层；但**具体的 socket 权限位、`TIME_WAIT` 行为、`bind: address already in use` 的内核时机在 Linux 上需复测**，标 `[未验证]`。§15 列出需要在 Linux 上复测的清单。

### 2.1 实验配置（`home/config.pristine.yaml` 原文）

```yaml
mixed-port: 29556
bind-address: 127.0.0.1
allow-lan: false
mode: rule
log-level: info
ipv6: false
external-controller: 127.0.0.1:29555
secret: r03secret
proxies: []
proxy-groups: []
rules:
  - MATCH,DIRECT
```

`home/config.yaml` 是运行中的可变副本（`exp_misc.sh` 的 `mkcfg()` 会反复重写它），字段子集与上面一致但没有 `proxies`/`proxy-groups`/`ipv6`。

### 2.2 mihomo CLI 全量 flag（`logs/help.txt` 原文）

```text
Usage of ./mihomo:
  -age-secret-key string      specify age secret key to decrypt configuration
  -config string              specify base64-encoded configuration string
  -d string                   set configuration directory
  -ext-ctl string             override external controller address
  -ext-ctl-pipe string        override external controller pipe address
  -ext-ctl-routing-mark int   override external controller routing mark
  -ext-ctl-tls string         override external controller tls address
  -ext-ctl-unix string        override external controller unix address
  -ext-ui string              override external ui directory
  -f string                   specify configuration file
  -m                          set geodata mode
  -post-down string           set post-down script
  -post-up string             set post-up script
  -secret string              override secret for RESTful API
  -t                          test configuration and exit
  -v                          show current version of mihomo
```

**关于本文的关键推论**：整个 flag 列表里**没有任何 daemon / pidfile / log-file 相关选项**。注意 `-post-up` / `-post-down` 是**脚本钩子**，`[上游源码 common/cmd]` 用 `/bin/sh -c` 执行——Agent **绝不能**把这两个 flag 透出给 API（对齐 `09-linux-runtime.md` §11.3）。[实测 `help.txt`][上游源码]

---

## 3. 启动、就绪与 stdout 行为

### 3.1 就绪日志行序列（实测原文，取 `m_daemon.out`）

```text
level=info msg="Start initial configuration in progress"
level=info msg="Geodata Loader mode: memconservative"
level=info msg="Geosite Matcher implementation: succinct"
level=info msg="Initial configuration complete, total time: 0ms"
level=info msg="RESTful API listening at: 127.0.0.1:29555"     ← controller 就绪
level=info msg="Sniffer is closed"
level=info msg="Mixed(http+socks) proxy listening at: 127.0.0.1:29556"   ← proxy 就绪
level=info msg="Start initial compatible provider default"
```

若配置了 unix socket，则 controller 就绪行有**两行**：

```text
level=info msg="RESTful API listening at: 127.0.0.1:29555"
level=info msg="RESTful API unix listening at: /tmp/r03-runtime/rt/mihomo.sock"
```

[实测 `m_unix.out`、`m_unix2.out`、`m_unix3.out`]

**可操作的就绪判据**（给 Agent 实现）：
- "**controller 可用**" = 出现 `RESTful API listening at` / `RESTful API unix listening at` **且** `GET /version` 返回 `200`。日志行本身不够——controller bind 失败时**这一行根本不会出现**（见 `m_port.out`：只有 `External controller listen error`，没有 `listening at`），所以日志行是**必要不充分**条件。[实测 `m_port.out`]
- "**proxy 端口就绪**" = 出现 `Mixed(http+socks) proxy listening at`。同理，bind 失败时该行被 error 行替代。[实测 `m_port2.out`]

### 3.2 从启动到 controller 可用的耗时

- **可用于就绪的"内部"计时**：日志行 `Initial configuration complete, total time: 0ms`（`geodata` 未触发下载时）。[实测，几乎全部 `*.out`]
  - 唯一例外是 `x_logs.out` 的第二、三次 reload，出现 `total time: 1ms`。[实测 `x_logs.out`]
- **本次重建无法给出"wall-clock 启动→controller 可用毫秒数"**。原 `exp_startup.sh` 用 `$EPOCHREALTIME` 在 shell 里算 `ready_ms` 并 `echo` 出来，该数字**只写进了脚本 stdout，未落盘**；`logs/start1.out`~`start3.out` 只有 mihomo 自身日志，不含计时。[证据不足]
  - **可从日志时间戳观察到的下界**：以 `start1.out` 为例，`Start initial configuration in progress` 到 `RESTful API listening at` 之间的时间戳差在**毫秒量级**（同为 `12:42` 秒级，微秒位相邻）。**该观察不构成可靠数字，不作为结论使用。** [证据不足 / 不作为实测数字]
- **结论**：控制器就绪耗时 **`[未验证]`**，列入 §15 开放问题 Q1（Linux 上重测）。

### 3.3 geodata 缺失时的自动下载与对就绪的影响

这是本次实测中**唯一一条"启动会失败"的路径**，且证据非常清晰。

**行为链**（`logs/y_geo.out` 原文，配置含 `geodata-mode: true` + `GEOIP,CN,DIRECT` 规则，且 `GeoIP.dat` 被预先删除）：

```text
level=info msg="Start initial configuration in progress"
level=info msg="Geodata Loader mode: memconservative"
level=info msg="Geosite Matcher implementation: succinct"
level=info msg="Can't find GeoIP.dat, start download"          ← 自动下载开始
（约 90 秒后）
level=error msg="can't initial GeoIP: can't download GeoIP.dat: Get \"https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.dat\": context deadline exceeded"
level=fatal msg="Parse config error: rules[0] [GEOIP,CN,DIRECT] error: can't download GeoIP.dat: Get \"...\": context deadline exceeded"
```

**结论**：
1. geodata 缺失时 **mihomo 会自动发起下载**（日志 `Can't find GeoIP.dat, start download`）。[实测 `y_geo.out`；`x_geo.out` 同]
2. 下载期间进程**一直不就绪**：`x_geo.out` 在 `Can't find GeoIP.dat, start download` 之后**再无任何日志**，原脚本的 `ready()` 轮询在观察窗内没有成功，进程也**没有** `RESTful API listening at`。[实测 `x_geo.out`]
3. 下载失败（本环境 `github.com` 不可达，`context deadline exceeded`）→ **`level=fatal`，启动彻底失败**。这是 `fatal` 而非 `error`，即进程会退出。[实测 `y_geo.out`]
4. **对就绪时间的影响：不可忽略且可能无限长**——本环境实测阻塞约 **90 秒**后才 fatal（`12:46:59.561988` → `12:48:29.563656`，即该次实验的实际耗时；这可从日志时间戳直接读出，属实测）。[实测 `y_geo.out`]
5. **对 Agent 的硬性要求**：`HealthChecker`/startup 超时不能假设"几秒内就绪"；含 GEOIP/GEOSITE 规则的配置在无外网环境会**卡满下载超时**。预置 geodata 文件、或在 `ValidateConfig` 阶段用 `mihomo -t` 提前暴露依赖，是必须做的。[推测，基于上述实测]

> ⚠️ **两次 geodata 实验的差异必须如实记录**：`x_geo.out`（12:46:28，来自 `exp_more.sh`）只有 `Can't find GeoIP.dat, start download`，**没有** fatal 行——因为该脚本的观察窗（`sleep 6`）远短于 90 秒下载超时，**实验没跑完**。`y_geo.out`（12:46:59 → 12:48:29，来自 `exp_final.sh`）用 3000 次 × 50ms 的长轮询跑满，才捕获到 fatal。**结论以 `y_geo.out` 为准；`x_geo.out` 只证明"下载阻塞期间 controller 不监听"。**

### 3.4 stdout 是行刷新还是块缓冲？

**这是本次重建中一条"证据被截断"的结论。**

- `exp_final.sh` 的 B 段设计目的就是回答这个问题：把 stdout 重定向到文件（非 tty，即管道/文件 → 通常是**块缓冲**），在 `ready` 之后立刻 `wc -c` 与 `grep -c 'RESTful API listening'` 这个文件，看进程**仍在运行**时日志是否已经可见。脚本的预期输出形如 `file bytes at ready-time (process STILL RUNNING): N` 与 `has 'RESTful API listening' visible now? 1`。
- **该输出未落盘**（只写在脚本 stdout）。**唯一残留的间接证据**是 `logs/y_flush.out`：该文件最终包含**完整的 8 行启动日志**（含 `RESTful API listening at`），说明日志**最终**会写入文件。
- **结论：`[未验证]`**。无法从留存证据区分"行刷新（运行中立即可见）"与"块缓冲（退出时才 flush）"。**不要假设 Agent 可以靠 tail stdout 文件实时拿到 mihomo 日志。**
- **可替代的实时日志通道（推荐）**：`GET /logs` 的 NDJSON 流（§8），它按行推送、与进程缓冲无关。[实测 `logs_std.txt`、`logs_struct.txt`] [推测：作为 tail-file 的替代方案]

---

## 4. 进程模型与目录产物

| 问题 | 实测结论 | 证据 |
|---|---|---|
| 是否 daemonize？ | **否**。`exp_misc.sh` 把 `$!` 记下的 shell 子进程 pid 直接用于后续 `kill`，且 `ready` 轮询成功——说明 mihomo 就是那个前台子进程，没有 fork 出后台子进程。 | [实测 `m_daemon.out` 对应脚本逻辑 `exp_misc.sh` §1] |
| 是否有 PID 文件？ | **没有**。`-d` 目录内产物只有 `bad.yaml`、`cache.db`、`config.pristine.yaml`、`config.yaml`，**没有 `.pid`**。 | [实测 `/tmp/r03-runtime/home/` 目录清单] |
| 父子进程关系？ | mihomo 是 spawn 它的 shell 进程的**直接子进程**，无中间层。`exp_misc.sh` §1 打印 `ps -p $P -o pid=,ppid=,pgid=,stat=` 与 `pgrep -P $P`（预期为空），说明**没有派生的子进程**。**注：该 stdout 未落盘**，因此"无子进程"这一条严格来说是 `[未验证]`；可以确认的只有"mihomo 是前台单进程"。 | [实测：进程存活与 kill 语义；`[未验证]`：子进程计数] |
| 有 PID 文件可依赖吗？ | **否**。Agent 必须以**自己 spawn 时拿到的 `Child` handle** 为唯一真相来源。 | [实测目录清单][上游源码 `main.go` 无 pidfile 逻辑][对齐 `09-linux-runtime.md` §4.3] |

### 4.1 `-d` 目录内容清单（实测）

```text
/tmp/r03-runtime/home/
├── cache.db              65536 B   ← mihomo 自动创建（缓存/fake-ip 池）
├── config.yaml                     ← 实验反复改写的活动配置
├── config.pristine.yaml            ← 实验脚本自带的"干净配置"备份（非 mihomo 产物）
└── bad.yaml                        ← 实验脚本自带（非 mihomo 产物）
```

`nodir/`（由 `exp_misc.sh` §7 在 `-d` 指向不存在目录时自动创建）：

```text
/tmp/r03-runtime/nodir/
├── cache.db      65536 B
└── config.yaml      16 B   ← 内容仅为 "mixed-port: 7890"
```

**结论**：mihomo 会在 `-d` 目录里创建 `cache.db` 与（缺失时）`config.yaml`。**没有 PID 文件，没有 lock 文件。** [实测两处目录清单]

### 4.2 `-d` 目录不存在时的行为（`m_nodir.out` 原文）

```text
level=info msg="Can't find config, create a initial config file"
level=info msg="Start initial configuration in progress"
level=info msg="Geodata Loader mode: memconservative"
level=info msg="Geosite Matcher implementation: succinct"
level=info msg="Initial configuration complete, total time: 0ms"
level=info msg="Sniffer is closed"
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:7890: bind: address already in use"
level=info msg="Start initial compatible provider default"
level=warning msg="Mihomo shutting down"
```

**结论**：
1. `-d` 目录与自己生成默认配置都会被自动创建（`Can't find config, create a initial config file`）。[实测 `m_nodir.out` + `nodir/config.yaml` 内容 `mixed-port: 7890`]
2. 这次启动**没有** `RESTful API listening at` 行——因为默认配置**不含** `external-controller`，所以 controller 根本没起。[实测 `m_nodir.out`]
3. 默认配置绑 `7890`，撞到宿主已有端口，报 error 但**进程仍存活到收到 TERM**。[实测 `m_nodir.out`]
4. **对 Agent 的意义**：绝不能依赖"`-d` 不存在时自动生成默认配置"——那是个 **无 controller + 撞端口**的退化状态。`StartMihomo` 必须**显式**指定 `-d` 与 `-f`，并在启动前确保目录与配置文件由 Agent 自己准备好。[推测，基于上述实测]

---

## 5. 信号语义（表格 + 证据文件名）

### 5.1 总表

| 信号 | 进程是否退出 | 是否优雅关闭 | 是否触发 reload | 退出码 | 证据文件 |
|---|---|---|---|---|---|
| `SIGTERM` | **是** | **是**（`Mihomo shutting down`） | 否 | 证据不足 → 见 §5.2 | `logs/sig_term.out`、`logs/s2_usr1.out`/`s2_usr2.out`（收尾的 TERM）、`logs/y_t1.out`~`y_t5.out` |
| `SIGINT` | **是** | **是**（`Mihomo shutting down`） | 否 | 证据不足（同 `SIGTERM`） | `logs/sig_int.out` |
| `SIGHUP`（配置未变） | **否** | — | **是**（原地重新 parse + 重建 listener） | 不适用（未退出） | `logs/sig_hup_nop.out`、`logs/s2_hup.out`、`logs/s3_hupwin.out` |
| `SIGHUP`（配置改为 `mode: global`） | **否** | — | **是** | 不适用 | `logs/sig_hup_reload.out`（**注：该文件本身不区分 mode，见 §5.4 缺口**） |
| `SIGHUP`（配置改为**坏配置**） | **否** | — | **失败但失败安全**：`Parse config error`，旧配置继续 | 不适用 | `logs/sig_hup_broken.out` |
| `SIGUSR1` | **观察窗内否**（存活） | 不适用 | 否（无 reload 日志） | 不适用 | `logs/s2_usr1.out`、`logs/sig_usr1.out`、`logs/s3_dfl.out` |
| `SIGUSR2` | **观察窗内否**（存活） | 不适用 | 否（无 reload 日志） | 不适用 | `logs/s2_usr2.out`、`logs/sig_usr2.out`、`logs/s3_dfl2.out` |
| `SIGQUIT`（对照组） | **是**（运行时栈转储） | 否（非优雅） | 否 | 证据不足（脚本预期 `128+3`） | `logs/s2_quit.out`、`logs/s2_quit.err`（21989 字节 Go panic/stack dump） |
| `SIGKILL` | **是**（不可捕获） | 否 | 否 | 证据不足（脚本注释预期 `137`） | `logs/m_kill9.out`、`logs/x_kill9a.out`、`logs/x_kill9b.out` |

> **关于"退出码"的诚实说明**：**所有退出码数字都在实验脚本的 stdout 里，未落盘。** `logs/*.out` 只有 mihomo 自己打的日志（不含退出码），`logs/*.err` 除 `s2_quit.err` 外**全部为 0 字节**。因此本文**不给出任何具体退出码数字**。上游源码给出的**唯一可引用**事实是：`SIGTERM`/`SIGINT` 走 `main()` `return` → `executor.Shutdown()` → **正常返回 exit 0**（而 `-t` 校验失败是 `os.Exit(1)`；`iptables` 配置失败是 `os.Exit(2)`——后两者见 `09-linux-runtime.md`）。[上游源码 `main.go`、`hub/executor/executor.go`][`[未验证]`：本次实测的退出码]

### 5.2 `SIGTERM` / `SIGINT`（优雅关闭）[实测]

`sig_term.out` 与 `sig_int.out` 结构完全相同，尾部都是：

```text
level=info msg="Start initial compatible provider default"
level=warning msg="Mihomo shutting down"      ← executor.Shutdown() 的日志
```

`sig_term.out` 时间戳：启动 `12:42:18.050`，shutdown 行 `12:42:18.332`。
`sig_int.out` 时间戳：启动 `12:42:18.832`，shutdown 行 `12:42:19.125`。

**⚠️ 这两个差值（≈282ms / ≈293ms）不是停机耗时。** 该差值 = 脚本"就绪轮询完成 → `sleep 0.4` → 发信号"的固定延迟，**不是 SIGTERM→退出的耗时**。**停机耗时的实测数字存在于脚本 stdout（`exit_ms=`），未落盘 → `[未验证]`。** [实测时间戳][证据不足：耗时]

**可确证的行为**：
- 收到 `SIGTERM`/`SIGINT` 后日志出现 `level=warning msg="Mihomo shutting down"`，进程随后退出。[实测 `sig_term.out`、`sig_int.out`、`y_t1.out`~`y_t5.out`、`s2_usr1.out`/`s2_usr2.out` 尾部均可见该行]
- 上游源码：`signal.Notify(termSign, syscall.SIGINT, syscall.SIGTERM)` → `return` → `defer executor.Shutdown()` → `listener.Cleanup()` + `tproxy.CleanupTProxyIPTables()` + `resolver.StoreFakePoolState()` + `log.Warnln("Mihomo shutting down")`。**这是真正的优雅停止路径。**[上游源码 `main.go`、`hub/executor/executor.go`]
- `Shutdown()` **没有显式等待/排空已建立的连接**，也没有 `sd_notify(STOPPING=1)`。[上游源码]

### 5.3 `SIGUSR1` / `SIGUSR2` —— 必须谨慎对待的一条

**实测观察到的事实**（三个独立实验、四个文件一致）：

| 文件 | 实验设计 | 发送信号 | 观察窗内结果 |
|---|---|---|---|
| `s2_usr1.out` | `exp_signals2.sh`：发送 `SIGUSR1`，`sleep 2` 后检查 | `SIGUSR1` | 日志 8 行后**无** `Mihomo shutting down`；脚本随后补发 `SIGTERM` 才产生 shutdown 行（`12:43:24.898` → `12:43:26.929`） |
| `s2_usr2.out` | 同上，`SIGUSR2` | `SIGUSR2` | 同上（`12:43:26.965` → `12:43:28.992`） |
| `s3_dfl.out` | `exp_signals3.sh`：用 `python3` 把 `SIGHUP`/`SIGUSR1`/`SIGUSR2` 显式 reset 为 `SIG_DFL` 后 `execv` mihomo，再发 `SIGUSR1` | `SIGUSR1`（默认处置） | 日志 8 行后 **无** shutdown；之后 TERM 才关闭 |
| `s3_dfl2.out` | 同上，只 reset `SIGUSR2` | `SIGUSR2`（默认处置） | 同上 |
| `sig_usr1.out` / `sig_usr2.out` | `exp_signals.sh` 的首轮 | `SIGUSR1`/`SIGUSR2` | 8 行日志 + 10 秒后 shutdown 行——**这 10 秒是脚本"仍存活则等 6s 再补 TERM"的逻辑造成的，不是信号效应** |

**结论（分层表述，避免过度断言）**：

- **可以确证的**：在 `SIGUSR1`/`SIGUSR2` 之后，**2 秒观察窗内进程确实存活，且没有产生任何 reload/关闭日志**。因此这两个信号**不触发 reload，也不触发优雅关闭**。[实测 `s2_usr1.out`、`s2_usr2.out`、`s3_dfl.out`、`s3_dfl2.out`]
- **上游源码事实上**：`main.go` 的 `signal.Notify` 只注册了 `SIGINT`/`SIGTERM`（termSign）与 `SIGHUP`（hupSign），**全树无 `SIGUSR1`/`SIGUSR2` 注册** → 默认处置 = 终止进程。[上游源码 `main.go`；`09-linux-runtime.md` §8.1 同结论]
- **`[未验证]` / 存在反证风险**：`s3_dfl*.out` 的设计初衷正是"排除继承下来的 SIG_IGN"——但**在容器/shell 里启动的被 SIG_IGN 的信号在 `exec` 后仍保持 SIG_IGN**，而 Go runtime 对 `SIGUSR1`/`SIGUSR2` 的默认行为**不是简单的 SIG_DFL**：Go runtime 为 `SIGUSR1` 保留了自己的内部用途（`SIGUSR2` 同理，部分平台用于 preemption）。因此"存活"**既可能**是"被 Go runtime 吞掉"**也可能**是"被父进程设为 SIG_IGN 后继承"。**本次证据无法区分这两种解释**——`s3_dfl.out` 里脚本打印的 `sigignore=[...]` 才是决定性的，而该行**未落盘**。
- **对 Agent 的硬性约束（无论解释如何）**：**绝不向 mihomo 发送 `SIGUSR1`/`SIGUSR2`。** 没有实测支持它们做任何有用的事，而上游源码明确没有注册它们。需要 reload 就用 `SIGHUP` 或 `PUT /configs`；需要停止就用 `SIGTERM`。[推测，但保守方向明确]

### 5.4 `SIGHUP` —— 三种情形的差别（本节是重点）

`exp_signals.sh` 设计了三个 case 来区分 `SIGHUP` 的行为：`hup_nop`（配置不变）、`hup_reload`（`mode: rule` → `mode: global`）、`hup_broken`（写成坏 YAML `mixed-port: "BROKEN"`）。

**情形 A：配置未变（`sig_hup_nop.out`）** — 进程存活，日志出现**第二组完整初始化**：

```text
（前 8 行首次启动）
level=info   msg="Start initial configuration in progress"
level=info   msg="Geodata Loader mode: memconservative"
level=info   msg="Geosite Matcher implementation: succinct"
level=info   msg="Initial configuration complete, total time: 0ms"
level=error  msg="External controller serve error: http: Server closed"   ← 旧 controller 被关掉
level=info   msg="RESTful API listening at: 127.0.0.1:29555"             ← 新 controller 起来
level=info   msg="Sniffer is closed"
level=info   msg="Start initial compatible provider default"
level=warning msg="Mihomo shutting down"                                  ← 这是脚本收尾的 TERM
```

**关键实测细节**：reload 时 controller 会**先关闭再重建**，过程中打出 `External controller serve error: http: Server closed`（**看起来像错误，其实是正常的 reload 中间态**）。因此 **Agent 不能把 `External controller serve error: http: Server closed` 判为故障**。[实测 `sig_hup_nop.out`、`sig_hup_reload.out`、`s2_hup.out`、`s3_hupwin.out`、`x_logs.out`、`x_rename.out` 六个文件一致]

**情形 B：配置有效变更（`sig_hup_reload.out`）** — 与情形 A 的日志**形状完全相同**（同样出现第二组初始化 + `Server closed` + 新 `listening at`）。[实测 `sig_hup_reload.out`]
- ⚠️ **证据缺口**：`sig_hup_reload.out` 是 **mihomo 进程日志**，它**不包含** `mode` 的实际取值。判定"`mode` 是否真的从 `rule` 变成 `global`"依赖脚本里 `mode_of()` 打印的 `pre:`/`post:` 行（`exp_signals.sh`）或 `attempt1..10` 轮询（`exp_signals2.sh` 的 `s2_hup` case）——**这些 stdout 全部未落盘**。因此"**好配置 + SIGHUP 确实生效**"这一点，本次只能确证到"**触发了完整的原地重载流程**"，**无法确证 mode 值真的变了**。→ §15 Q3。[证据不足]
- 可以确证的旁证：`x_rename.out` 在"原子替换配置 + SIGHUP"之后，日志出现 `level=info msg="[TCP] 127.0.0.1:62709 --> 127.0.0.1:29555 using DIRECT"`——一条**运行期流量日志**，证明 reload 后实例仍在**正常工作**（而不是只重建了 listener 就僵死）。[实测 `x_rename.out`]

**情形 C：坏配置（`sig_hup_broken.out`，本节最重要）**

```text
（前 8 行首次启动，正常）
level=error msg="Parse config error: yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `BROKEN` into int"
level=warning msg="Mihomo shutting down"    ← 脚本收尾的 TERM（12:42:41 → 12:42:51）
```

**结论（三条，全部可从该文件直接读出）**：
1. 坏配置 + `SIGHUP` → **进程不退出**：从 `Parse config error`（`12:42:41.700`）到脚本 TERM 产生的 shutdown（`12:42:51.593`）之间约 10 秒，期间**没有任何重载成功日志，也没有第二组初始化**——即**重载被中止，旧配置没有被破坏**。[实测 `sig_hup_broken.out`]
2. **没有第二组 `Initial configuration complete`**，也**没有** `External controller serve error`——证明 `hub.Parse` 在解析阶段就失败了，**没有走到 ApplyConfig**，所以旧 listener 全部保持。[实测 `sig_hup_broken.out`][上游源码 `hub/hub.go`：失败只 `log.Errorln`]
3. **`SIGHUP` 是失败安全的**：这与"用 `PUT /configs` 发非法 YAML 得到 `400` 且实例仍 `200` 存活"的 R01 结论**方向一致**，但**反馈通道不同**：`SIGHUP` 路径**没有结构化错误回传**，Agent 只能从 stdout 抓那行 `Parse config error`。[实测 + R01 §1.6][上游源码]

**SIGHUP 的瞬时不可用窗口**：`exp_signals3.sh` 的 (c) 段用 400 次快速 `GET /version` 探测统计 SIGHUP 造成的 controller 中断。结果文件 `logs/s3_hupwin.count` 内容为：

```text
0 400
```

即 **`non_200=0`，400 次探测全部 `200`**——**在 400 次探测的分辨率下没有观测到任何 controller 不可用窗口**。[实测 `s3_hupwin.count` + `s3_hupwin.out`]

> ⚠️ **对该数字的严格限定**：探测是串行 `curl`（每次 `-m 0.2`），间隔由进程启动开销决定，**不能等价于高频采样**；窗口 `outage_window_ms` 未打印（`s3_hupwin.count` 的第 3/4 字段为空 → 说明 `firstfail` 为空，与 `non_200=0` 自洽）。**结论只能表述为"未观测到中断"，不能表述为"reload 零中断"**；`Server closed` 与新的 `listening at` 之间在日志上确实存在**亚毫秒级**的间隔（如 `s3_hupwin.out`：`12:43:48.122837` → `12:43:48.123031`，约 0.2ms）。[实测 / 证据有限]

**推荐给 Agent 的 reload 路径**（与 `09-linux-runtime.md` §8.1 一致，本文件证据支持）：
- **优先 `PUT /configs?force=true`（经 controller）**：有 HTTP 状态码，能区分"配置非法"与"进程不可达"，可与 health check + rollback 组成事务。[推测，基于 R01 §1.6 与本文 §5.4 的对比]
- **`SIGHUP` 作为降级路径**（controller 不可达时）。[推测]
- **绝不用 `-config <base64>` / `-f -` 启动**：`configBytes` 非空时 `SIGHUP` 只重解析同一份内存字节，永远读不到新配置。这是"必须显式 reload 否则不生效"之外的第二个陷阱。[上游源码 `hub/hub.go`；`09-linux-runtime.md` §8.1]

---

## 6. Unix Socket

### 6.1 可以确证的（来自 mihomo 自身日志 + 磁盘残留）

| 结论 | 证据 |
|---|---|
| 需在配置里显式写 `external-controller-unix: <path>`（或 `-ext-ctl-unix`）；**默认不启用** | [实测 `home/config.pristine.yaml` 无该字段 → `m_daemon.out` 无 unix 行；`m_unix.out` 有该字段 → 有 unix 行] |
| 启用后日志行为 `level=info msg="RESTful API unix listening at: /tmp/r03-runtime/rt/mihomo.sock"` | [实测 `m_unix.out`、`m_unix2.out`、`m_unix3.out`] |
| **TCP 与 Unix 可同时监听**（配置里两者并存时日志两行都有），二者由独立 goroutine 启动、互不排斥 | [实测 `m_unix.out`；上游源码 `hub/route/server.go` `ReCreateServer()` 启 4 个 goroutine] |
| **父目录不存在时会被 mihomo 自动创建**（`MkdirAll(dir, 0o755)`）；`m_unix3.out` 的 case 里父目录由脚本**预先创建为 `0750`** 且未被改写，实测磁盘权限保持 `drwxr-x---` | [实测磁盘：`/tmp/r03-runtime/rt` = `drwxr-xr-x`(0755，由 mihomo 创建)、`/tmp/r03-runtime/rt2` = `drwxr-x---`(0750，脚本预建、mihomo 未改)][上游源码 `os.MkdirAll(dir, 0o755)`] |
| **socket 文件权限实测为 `srw-rw-rw-`（0666）**，两个目录下均如此 | [实测磁盘：`rt/mihomo.sock` 与 `rt2/mihomo.sock` 均为 `srw-rw-rw-`][上游源码硬编码 `os.Chmod(addr, 0o666)`] |
| **`SIGKILL` 后 socket 文件残留**（`Shutdown()` 未跑到，无人 unlink）；`m_unix2.out` 的 case 就是"在残留 socket 上重启"，且**重启成功**（日志出现完整的 unix listening 行）——说明启动前会 `syscall.Unlink` 旧 socket | [实测磁盘残留 + `m_unix2.out` 完整启动日志][上游源码 `_ = syscall.Unlink(addr)`] |
| **`SIGTERM` 后 socket 文件是否被清理** | **`[未验证]`**。`exp_misc.sh` §3 会打印 `after SIGTERM: socket file present? ...`，但该 stdout 未落盘。磁盘上残留的两个 socket 文件都来自被 `SIGKILL` 的 case，**不能反推 TERM 的行为**。[证据不足] |
| **socket 目录权限是唯一访问控制**（socket 本身 0666 + 不校验 secret） | [实测权限 0666][上游源码 + 上游文档 warning][对齐 `01-mihomo.md` §1.4、`09-linux-runtime.md` §5.3] |

### 6.2 证据不足的部分（必须如实标注）

`exp_misc.sh` §3 设计了完整的 `curl --unix-socket` 探测矩阵：

```text
unix /version NO auth     : <http_code>
unix /version WRONG secret: <http_code>
unix /version body        : <json>
tcp  /version with secret : <http_code>   (simultaneous bind)
unix /configs             : <http_code>
```

**这五行全部只写在脚本 stdout，未落盘。** 因此本文**不给出**这些 HTTP 状态码。相关结论由 R01 §1.3 的**同环境独立实测**覆盖（`[实测]`，见 `01-mihomo.md` 的鉴权矩阵）：

| 访问方式 | 无 secret header | 正确 Bearer | 错误 Bearer |
|---|---|---|---|
| TCP `127.0.0.1:*` | `401 {"message":"Unauthorized"}` | `200` | `401` |
| Unix socket | **`200`** | `200` | **`200`** |

→ **本文件直接引用 R01 的该矩阵，不重复声称自己测过。** 需要独立复现时列入 §15 Q4。

### 6.3 对 Agent 的实现约束

```text
1. Agent 自己先创建 runtime 目录（建议 0750、属主为 mihomo 运行用户/组），
   利用 mihomo 的 MkdirAll 对已存在目录是 no-op 的特性锁死权限。
   ← 实测支持：rt2 (预建 0750) 未被改成 0755。
2. 不要把重要文件放在 socket 路径上：mihomo 启动前会无条件 syscall.Unlink(path)。
3. 始终传绝对路径（相对路径走 mihomo 自己的 resolve 规则，不是 Agent 的 cwd）。
4. Agent 停止 mihomo 后应主动清理残留 socket 文件（SIGKILL 场景必然残留）；
   启动前也可主动清理，mihomo 自己也会 unlink，二者不冲突。
5. socket 权限 0666 + 不校验 secret ⇒ 目录权限就是安全边界。
   这是部署层红线，不是可以在 Agent 代码里"修好"的东西。
```
[1/2/3: 实测 + 上游源码; 4/5: 推测，基于 §6.1 的实测]

---

## 7. 崩溃检测、端口释放与端口冲突

### 7.1 `SIGKILL` 崩溃（`m_kill9.out`、`x_kill9a.out`/`x_kill9b.out`）

**可以确证的**：
- `SIGKILL` 后进程**立即消失**，且**没有任何 shutdown 日志**——`m_kill9.out` 与 `x_kill9a.out` 都**停在** `level=info msg="Start initial compatible provider default"`，**没有** `Mihomo shutting down`。[实测 `m_kill9.out`、`x_kill9a.out`]
  → **崩溃检测不能靠日志**："没有 shutdown 行就退出"才是异常的判据；靠 `Child::wait()` / pidfd 才是可靠手段。[推测]
- **`SIGKILL` 后立刻重启（同端口）成功**：`x_kill9b.out` 是紧随其后的第二次启动，日志为**完整 8 行正常启动**（含 `RESTful API listening at: 127.0.0.1:29555` 与 `Mixed(http+socks) proxy listening at: 127.0.0.1:29556`），**没有任何 `bind: address already in use`**。[实测 `x_kill9b.out`]
  - 时间戳：`x_kill9a.out` 启动 `12:46:22.877`，`x_kill9b.out` 启动 `12:46:22.892`，间隔约 **15ms**。[实测]
  - **结论：listen socket 被 kill -9 后不会进入 `TIME_WAIT` 阻塞，端口可立刻复用。**（`TIME_WAIT` 只作用于**已建立连接**的四元组，不作用于监听 socket。）[实测 `x_kill9b.out`][推测：机制解释]
- **端口释放/`reap_ms`/退出码的具体数字未落盘** → `[未验证]`。`exp_misc.sh` §2 打印的 `SIGKILL exit_code=... (128+9=137) reap_ms=...`、逐 100ms 的 `t+...ms:` 探测序列、`listener after kill: [...]`、`residue in -d: [...]` **全部未落盘**。[证据不足]

**缺口**：`m_kill9.out` 目录清单与"残留文件"结论也无法给出（`exp_misc.sh` 的 `ls -la` 输出未落盘）。从**当前磁盘状态**只能确认 `home/` 目录里有 `cache.db`（未被 kill 破坏到无法写出——它早在 12:45 就已存在）。[证据不足]

### 7.2 端口冲突（controller 被占）— `m_port.out`

配置：`external-controller: 127.0.0.1:29555`，**另一个 node 进程先占住 29555**，mixed-port 29556 空闲。

```text
level=info  msg="Start initial configuration in progress"
level=info  msg="Geodata Loader mode: memconservative"
level=info  msg="Geosite Matcher implementation: succinct"
level=info  msg="Initial configuration complete, total time: 0ms"
level=error msg="External controller listen error: listen tcp 127.0.0.1:29555: bind: address already in use"
level=info  msg="Sniffer is closed"
level=info  msg="Mixed(http+socks) proxy listening at: 127.0.0.1:29556"    ← proxy 照常起
level=info  msg="Start initial compatible provider default"
level=warning msg="Mihomo shutting down"                                    ← 5 秒后脚本 TERM
```

**结论**：
1. controller bind 失败是 **`error` 级日志，不是 `fatal`** → **进程不退出**。[实测 `m_port.out`]
2. **proxy 功能不受影响**：mixed-port 照常监听。[实测 `m_port.out`]
3. **controller 完全不可用**：该文件里**没有** `RESTful API listening at` 行——所以"有 `listening at` 行"是 controller 可用的必要条件。[实测 `m_port.out`]
4. 从 `12:45:53.568`（启动完成）到 `12:45:56.680`（脚本 TERM 后的 shutdown）间隔约 3.1s，**与脚本的 `sleep 3` 一致** → 说明这段时间进程一直存活。[实测]

### 7.3 端口冲突（mixed-port 被占）— `m_port2.out`

配置：controller 29555 空闲，**另一个 node 进程先占住 29556**。

```text
level=info  msg="Initial configuration complete, total time: 0ms"
level=info  msg="RESTful API listening at: 127.0.0.1:29555"                ← controller 照常起
level=info  msg="Sniffer is closed"
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:29556: bind: address already in use"
level=info  msg="Start initial compatible provider default"
level=warning msg="Mihomo shutting down"
```

**结论**：
1. mixed-port bind 失败同样是 **`error` 级、不致命**，进程继续存活。[实测 `m_port2.out`]
2. **controller 完全可用**（有 `RESTful API listening at`），Agent 可以照常通过 API 查询/管理——**但代理流量全部不可用**。[实测 `m_port2.out`]
3. **这就是最危险的"假健康"**：`GET /version` 返回 `200`，`/configs` 正常，而用户实际上没有代理。**任何只探测 controller 的健康检查都会误报健康。**[推测，直接由上述实测推出]
4. 这是**仓库中最高价值的一条 Health Check 论据**——它把"controller 可达"与"代理可用"在实测中**撕开了**。[实测 `m_port2.out`]

### 7.4 重复实例（`y_dup.out`）

配置：同一 `-d` 目录、**相同端口**上再起第二个 mihomo（第一个仍在跑）。

```text
level=error msg="External controller listen error: listen tcp 127.0.0.1:29555: bind: address already in use"
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:29556: bind: address already in use"
level=info  msg="Start initial compatible provider default"
level=warning msg="[CacheFile] can't open cache file: timeout"     ← cache.db 被第一个实例持有
level=warning msg="Mihomo shutting down"
```

**结论**：
1. **mihomo 没有任何单实例互斥机制**（无 PID 锁、无 `flock`）：两个实例会同时跑，各自被内核拒绝 bind 而已。[实测 `y_dup.out`、`y_flush.out` 证同时可见两个进程的日志]
2. **意外的连带损伤**：`[CacheFile] can't open cache file: timeout` —— 第二个实例共享 `-d` 目录时会**打不开 `cache.db`**（被第一个实例锁住/超时），说明**共享 `-d` 目录是不安全的**。[实测 `y_dup.out`]
3. **这是 Agent 必须自己防的**：per-instance lock（AGENTS.md 已要求）不是可选优化，而是**防止损坏 cache.db 与端口混乱的必要机制**。[推测，基于上述实测]

### 7.5 与 R01 的交叉核对

`docs/research/01-mihomo.md` §2.4 记录："mihomo **会记录 `bind: address already in use` 并继续启动**（部分 listener 失败不致命）"。

**本文证据完全一致**，并且**细化了两个方向**：
- R01 只说"部分 listener 失败不致命"；本文补充了**两个方向的独立性**（controller 挂但 proxy 活、proxy 挂但 controller 活），这是分层的直接依据。
- R01 提到"19090/17890 已被宿主 Clash Party 占用"；本文的 `29555`/`29556` 是专门避开后的端口，冲突实验是**人工制造**的。[实测 `exp_misc.sh` §5/§6]
- **一处需要后续核对的差异**：R01 未提及 `[CacheFile] can't open cache file` 这类**共享 `-d` 导致的数据面副作用**；本文实测到了，是新增发现。[实测 `y_dup.out`]

---

## 8. `/logs` 流与日志结构

### 8.1 格式（`logs_std.txt` / `logs_struct.txt` 原文）

**标准格式（默认，`GET /logs`）** —— 每行一个独立 JSON，**NDJSON**，字段是 `type` + `payload`：

```json
{"type":"info","payload":"Start initial configuration in progress"}
{"type":"info","payload":"Geodata Loader mode: memconservative"}
{"type":"info","payload":"Geosite Matcher implementation: succinct"}
{"type":"info","payload":"Initial configuration complete, total time: 0ms"}
```

**结构化格式（`GET /logs?format=structured&level=info`）** —— 字段是 `time` + `level` + `message` + `fields`：

```json
{"time":"12:46:27","level":"info","message":"Start initial configuration in progress","fields":[]}
{"time":"12:46:27","level":"info","message":"Geodata Loader mode: memconservative","fields":[]}
{"time":"12:46:27","level":"info","message":"Geosite Matcher implementation: succinct","fields":[]}
{"time":"12:46:27","level":"info","message":"Initial configuration complete, total time: 0ms","fields":[]}
```

[实测 `logs_std.txt`（278 B，4 行）、`logs_struct.txt`（402 B，4 行）]

**关键实测细节**：
- **`level` 字段名在两种格式下不同**：标准格式用 `type`，结构化格式用 `level`。Agent 的解析器**必须按格式分支**，不能假设字段名一致。[实测两份文件]
- **结构化格式的时间戳只有 `HH:MM:SS`**（`"12:46:27"`），**没有日期、没有时区、没有亚秒精度**。Agent 若需要可排序的时间戳必须**自己补日期**，或改用进程 stdout（其格式是 RFC3339 带纳秒，如 `time="2026-09-12T12:46:27.004832000+08:00"`）。[实测 `logs_struct.txt` vs `x_logs.out`]
- `fields` 实测为空数组 `[]`。[实测 `logs_struct.txt`]

### 8.2 `level` 过滤与非法参数

- `logs_err.txt` 为 **0 字节**：`?level=error` 的流在生成的日志全是 `info` 时**什么都没有推送**（不是报错、不是空行）。说明 level 过滤**真的按级别丢弃**。[实测 `logs_err.txt`（0 B）]
- `ws2.txt` 内容为 `{"message":"Body invalid"}`：这是 `exp_misc.sh` §8 里 `curl "$API/logs?level=bogus"` 的响应体。**`{"message":"Body invalid"}` 不是 400 的典型 body**，且 `ws2.txt` 只有 27 字节、**没有 HTTP 状态行**（因为脚本用的是 `-o` 而不是 `-i`）。因此"非法 level 返回 400"这一条**本次无法确证**（状态码未落盘）；**只可确证响应体是该 JSON 错误消息**。[实测 `ws2.txt`][证据不足：状态码]
  - 上游源码/文档支持"level 非法返回 HTTP 400"，见 `09-linux-runtime.md` §7（`[上游源码]`）。**引用上游结论，不声称本次测过。**

### 8.3 WebSocket 握手

`exp_more.sh` B 段的 `-- WS handshake --` 会打印前 4 行 HTTP 响应头（预期 `101 Switching Protocols`）。**该输出未落盘。** `[未验证]`。

**可替代引用**：R01 §1.5 已在同环境实测"带 WS header 时返回 `101 Switching Protocols` 并推 WS 帧"，`[实测]`（见 `01-mihomo.md`）。本文**引用**该结论，列入 §15 Q5 作为"本目录内未复现"的项目。

### 8.4 对 Agent 的实现约束

```text
1. 消费 /logs 用 NDJSON 流（GET，无 Upgrade 头），逐行 JSON 解析；不要假设一定是 WebSocket。
2. 必须处理两种字段布局（type/payload 与 time/level/message/fields）。
3. 结构化模式的时间戳粒度为秒且无日期 ⇒ 事件顺序与时间线不能只依赖它。
4. level 过滤会让不匹配的行整行消失（实测 level=error 得到 0 字节），
   所以"流里没有 error 行"不能推出"没有发生错误"——错误可能被过滤掉。
5. 进程 stdout 的日志格式（logrus text，RFC3339 纳秒）与 /logs 的 JSON 格式不同，
   两条通道的解析器不能共用。
```
[1/2/3/4: 实测; 5: 实测（对比 `logs_struct.txt` 与任意 `*.out`）]

---

## 9. Health Check 分层定义

基于 §7 的独立失败证据，给出四层定义。**每一层都有实测证据表明它可以独立于其他层失败。**

| 层 | 名称 | 判据（可操作） | 独立失败证据 |
|---|---|---|---|
| L0 | **ProcessAlive** | Agent 持有的 `Child` 未 `wait` 出退出状态（Linux 上可用 pidfd/CGrup 兜底）。**不要用 PID 文件。** | [实测：无 PID 文件 §4；`SIGKILL` 无日志 §7.1] |
| L1 | **ControllerReachable** | `GET /version`（TCP 或 unix socket）返回 `200` 且 body 含 `meta`/`version`。超时建议 ≤500ms。 | [实测：`m_port.out` controller bind 失败 → L1 失败但 L0 通过、L3 通过；`m_nodir.out` 无 controller 配置 → L1 失败] |
| L2 | **ConfigLoaded** | `GET /configs` 返回 `200` 且含关键字段（`mode`、`mixed-port`/`port`），**且与 Agent 期望的 config version 一致**（比对 checksum 或关键字段）。 | [实测：`sig_hup_broken.out` 重载失败但 L0/L1 全部通过 —— 旧配置仍在跑，但"新配置已加载"是假的。这是 L2 存在的理由] |
| L3 | **ProxyListening** | 由 Agent **主动 connect** 到配置里的 mixed-port/`port`（TCP 握手成功即可），**不要**用 `curl -x` 探测。 | [实测：`m_port2.out` / `y_dup.out` mixed-port bind 失败 → L3 失败，而 L0/L1/L2 全部通过] |

### 9.1 为什么必须是四层而不是一层

| 场景 | L0 | L1 | L2 | L3 | 证据 |
|---|---|---|---|---|---|
| 正常运行 | ✅ | ✅ | ✅ | ✅ | [实测 `m_daemon.out`] |
| controller 端口被占 | ✅ | ❌ | ❓ | ✅ | [实测 `m_port.out`] |
| mixed-port 被占 | ✅ | ✅ | ✅ | ❌ | [实测 `m_port2.out`、`y_dup.out`] |
| 坏配置 + SIGHUP | ✅ | ✅ | ❌（新配置未加载） | ✅（旧配置的 proxy 还在跑） | [实测 `sig_hup_broken.out`] |
| `SIGKILL` 崩溃 | ❌ | ❌ | ❌ | ❌ | [实测 `m_kill9.out`] |
| 无 controller 配置 | ✅ | ❌ | ❌ | ✅ | [实测 `m_nodir.out`] |

→ **"`GET /version` 返回 200 = 健康"是错的**（`m_port2.out` 反例）；**"进程活着 = 健康"是错的**（`m_port.out` 反例）。[实测]

### 9.2 L2 的实现难点（必须写清楚）

- L2 是**唯一不能只看 mihomo 回复的一层**：controller 只能告诉你"当前运行的配置长什么样"，不能告诉你"它是不是你刚写进去的那一份"。Agent 必须**自己维护期望值**（`/var/lib/proxy-agent/state/` 里的 active config version + checksum），再与 `GET /configs` 做比对。[推测]
- `SIGHUP` 路径**没有失败回传**（§5.4），所以若 Agent 用 `SIGHUP` 做 reload，**L2 是唯一能发现 reload 失败的手段**。[实测 `sig_hup_broken.out` + 推测]
- 因此推荐 `PUT /configs?force=true`：它把"reload 成功/失败"变成 L1 上的一个 HTTP 状态码，使 L2 的失败能更早、更明确地暴露。[推测，基于 R01 §1.6]

### 9.3 探测频率建议

| 层 | 建议频率 | 理由 |
|---|---|---|
| L0 | 事件驱动（`Child::wait`）+ 低频兜底（10s） | 不需要轮询开销 |
| L1 | 5–10s | 轻量；但注意 §8.4 第 4 条：不要用 `/logs` 判断健康状况 |
| L2 | 仅在 reload/activate 之后立即执行；稳态下 60s | 它需要与 state 做比对，较重 |
| L3 | reload 后立即 + 30s | 主动 connect 有成本，不能太频繁 |

[推测，全部由 §7 的失败模式推导；无实测频率数据]

---

## 10. Port 划分结论与 trait 草案

### 10.1 结论：**拆成三个是合理的，且理由来自实测**

Phase 0 原问题是"是否分成 `ProcessManager` / `MihomoController` / `HealthChecker` 三个 Port"。**基于 §7/§9 的证据，答案是"是"**，理由：

1. **三者面对的可失败面完全不同且实测中已独立分离**（§9.1 矩阵）。若合并成一个 Port，实现者必然会把三层混在一个 `status()` 里，重复 R03 之前"单层探测误报"的错误。[实测支撑]
2. **通信机制不同**：`ProcessManager` 走 Rust 子进程系统调用（`spawn`/`kill`/`wait`），`MihomoController` 走 HTTP over TCP/Unix socket。把它们放进同一个 trait 会让 domain 侧出现"传输机制"的泄漏。[推测，对齐 AGENTS.md 的端口纪律]
3. **`HealthChecker` 是唯一需要"期望状态"输入的**（L2 要比对 config version/checksum），而 `MihomoController` 是纯粹的无状态读。职责边界清晰。[推测]
4. **可替换性**：形态 B（双 unit）下 `ProcessManager` 换成 systemd adapter，而 `MihomoController`/`HealthChecker` 完全不变。[推测，对齐 `09-linux-runtime.md` §2/§8.3]

**但要注意与 `09-linux-runtime.md` §11.1 的命名冲突**：R09 提出了 `ServiceManager`（数据面进程启停/重载/状态观察，抽象边界不泄漏 unit/drop-in）与 `ProcessManager`（**通用子进程执行 + argv 白名单**）。**这两个是不同层次的东西**，不能互相替代：

| Port | 层次 | 职责 | 关系 |
|---|---|---|---|
| `ServiceManager`（R09） | 生命周期抽象 | 数据面进程的启停/重载/状态观察，两种实现：`DirectProcess` / `SystemdUnit` | **内部使用** `ProcessManager` |
| `ProcessManager`（R03） | 通用子进程原语 | `spawn`/`signal`/`wait`，argv 白名单 | 被 `ServiceManager` 使用 |
| `MihomoController` | 控制面客户端 | controller HTTP API 子集 | 与生命周期正交 |
| `HealthChecker` | 观测 | 四层健康判定 | 依赖前两者 |

→ **建议：把 R09 的 `ServiceManager` 作为 Application 层对外的生命周期 Port，把 `ProcessManager` 作为它的 Infrastructure 内部依赖；`MihomoController` 与 `HealthChecker` 保持独立。** 这样既不与 R09 冲突，又落实了"三个 Port"的结论。[推测 / 需要 ADR 收口]

### 10.2 trait 草案（Rust，`async_trait`）

> 以下为**设计草案**，不是实测结论。所有约束都注明了实测依据。

```rust
// ---------- Application 层定义（Ports）----------

/// 通用子进程原语。argv 必须是编译期白名单模板，绝不接受任意字符串。
#[async_trait]
pub trait ProcessManager: Send + Sync {
    /// 启动进程。argv 模板固定为 ["<mihomo>", "-d", <dir>, "-f", <config>]。
    /// 禁止 "-config" / "-f -"（SIGHUP 陷阱，见 §5.4）。
    async fn spawn(&self, spec: SpawnSpec) -> Result<ProcessHandle, ProcessError>;

    /// 发送信号。允许集合仅 {TERM, KILL, HUP}。
    /// 禁止 USR1/USR2/HUP-with-broken-config 之外的任何扩展信号（§5.3）。
    async fn signal(&self, h: &ProcessHandle, sig: Signal) -> Result<(), ProcessError>;

    /// 等待退出，返回退出状态。超时由调用方控制。
    /// 崩溃（无 shutdown 日志的退出）由 wait 结果判定，不能靠日志（§7.1）。
    async fn wait(&self, h: &ProcessHandle, timeout: Duration)
        -> Result<ExitStatus, ProcessError>;

    /// 非阻塞存活检查（L0）。
    fn is_alive(&self, h: &ProcessHandle) -> bool;
}

/// controller 客户端（HTTP over TCP 或 Unix socket）。
/// 只暴露稳定子集；不含 /upgrade（R01 §1.9）、不含 /restart（PID 不变的陷阱）。
#[async_trait]
pub trait MihomoController: Send + Sync {
    /// 就绪/存活探测（L1）。返回 meta/version。
    async fn version(&self) -> Result<VersionInfo, ControllerError>;

    /// 读取当前运行配置（L2 的输入）。
    async fn configs(&self) -> Result<RunningConfig, ControllerError>;

    /// 触发 reload 并返回**结构化**结果（优先于 SIGHUP，§5.4）。
    /// 失败必须能与"进程不可达"区分：前者是 HTTP 4xx，后者是传输错误。
    async fn reload(&self, cfg: ReloadRequest) -> Result<ReloadOutcome, ControllerError>;

    async fn proxies(&self) -> Result<ProxyList, ControllerError>;
    async fn connections(&self) -> Result<ConnectionList, ControllerError>;
    async fn traffic(&self) -> Result<TrafficStats, ControllerError>;

    /// /logs 的 NDJSON 流。必须处理两种字段布局（§8.1）。
    fn log_stream(&self, req: LogStreamRequest) -> Result<LogStream, ControllerError>;
}

/// 四层健康判定。必须显式接收"期望状态"输入（L2 需要）。
#[async_trait]
pub trait HealthChecker: Send + Sync {
    async fn check(&self, expected: &ExpectedState) -> Result<HealthReport, HealthError>;
}

/// 对外的数据面生命周期 Port（对齐 R09 的 ServiceManager 命名）。
/// 内部委托给 ProcessManager；形态 B 下换成 systemd 实现。
#[async_trait]
pub trait ServiceManager: Send + Sync {
    async fn start(&self, cfg: ConfigPath) -> Result<RuntimeHandle, ServiceError>;
    async fn stop(&self, h: &RuntimeHandle) -> Result<(), ServiceError>;
    async fn reload(&self, h: &RuntimeHandle, cfg: ConfigPath) -> Result<(), ServiceError>;
    async fn status(&self, h: &RuntimeHandle) -> Result<RuntimeStatus, ServiceError>;
}

// ---------- 类型（示意）----------

pub struct HealthReport {
    pub process_alive: bool,        // L0
    pub controller_reachable: bool, // L1
    pub config_loaded: bool,        // L2 —— 与 ExpectedState 比对
    pub proxy_listening: bool,      // L3
    pub detail: Vec<HealthDetail>,  // 每层的失败原因
}
```

### 10.3 三条硬性实现约束（每条都有实测依据）

1. **`signal()` 的允许集合必须显式枚举 `{Term, Kill, Hup}`**，绝不透出 `USR1`/`USR2`。[实测缺失 + 上游源码：未注册；§5.3 的保守结论]
2. **`spawn()` 的 argv 模板编译期固定**，不允许 `-config`/`-f -`/`-post-up`/`-post-down`。[上游源码：`-post-*` 用 `/bin/sh -c` 执行 → 任意命令执行；`-config` 破坏 SIGHUP reload]
3. **`reload()` 必须能区分三类结果**：`Ok`（HTTP 204/200）、`InvalidConfig`（HTTP 4xx，**旧配置仍生效，不得触发 rollback 之外的动作**）、`Unreachable`（传输错误，**这时必须走 L0/L1 降级判定，而不是认为配置坏了**）。[实测 `sig_hup_broken.out` 证明"配置坏了但进程好好的"是真实状态]

---

## 11. 生命周期状态机与并发串行化

### 11.1 状态机

```text
                    ┌──────────────────────────────────────────┐
                    │                                          │
                    ▼                                          │
   ┌─────────┐  start   ┌──────────┐  ready(L1 ok)  ┌─────────┐ │
   │ Stopped │─────────▶│ Starting │───────────────▶│ Running │ │
   └─────────┘          └──────────┘                └─────────┘ │
        ▲                    │                         │    ▲    │
        │              timeout│/spawn fail        L2/L3│    │    │
        │                    ▼                      fail│    │L2/L3│
        │              ┌─────────┐                     ▼    │恢复│
        │              │ Failed  │              ┌──────────┐  │    │
        │              └─────────┘              │ Degraded │──┘    │
        │                   ▲                   └──────────┘       │
        │                   │                         │           │
        │              exit(≠0)┌────────────────────────┐        │
        └───────────────────────│      Stopping          │◀───────┘
                                └────────────────────────┘   stop
                                       │            │
                                  exit(0)      timeout→SIGKILL
                                       ▼            ▼
                                  ┌─────────┐  ┌────────┐
                                  │ Stopped │  │ Failed │
                                  └─────────┘  └────────┘
```

| 状态 | 含义 | 进入条件 | 证据/依据 |
|---|---|---|---|
| `Stopped` | 无进程 | 初始；或 stop 完成且退出码正常 | [推测] |
| `Starting` | 已 spawn，未就绪 | `spawn()` 成功 | [实测：就绪前 controller 不可达，`m_port.out`] |
| `Running` | L0+L1+L2+L3 全绿 | `GET /version` 200 且 L2/L3 通过 | [实测 4 层判据 §9] |
| `Degraded` | 进程活着但至少一层失败 | L1 或 L2 或 L3 失败而 L0 通过 | [实测：`m_port.out`（L1 挂）、`m_port2.out`（L3 挂）、`sig_hup_broken.out`（L2 挂）] |
| `Stopping` | 已发 SIGTERM，等退出 | `stop()` 调用 | [实测：`SIGTERM` → `Mihomo shutting down`] |
| `Failed` | 进程已死或信号完全无效 | 非正常退出；或 SIGKILL 都无效（需人工） | [实测：`SIGKILL` 后无 shutdown 日志 = 崩溃] |

**关键设计点：`Degraded` 是必需的，不是可选的。** 实测反复证明"进程活着但某层坏了"是**常态而非异常**（controller 被占、mixed-port 被占、reload 失败）。若只有 `Running`/`Failed` 二元态，上面三种情况都会被迫归成 `Running`（掩盖故障）或 `Failed`（触发不必要的重启，而重启解决不了端口被占）。[实测 + 推测]

### 11.2 非法转换必须被拒绝

| 转换 | 行为 | 理由 |
|---|---|---|
| `Starting → Starting` | **拒绝**（返回 `AlreadyStarting`，不 spawn 第二个进程） | AGENTS.md 硬性要求；[实测 `y_dup.out` 证明重复实例会撞端口 + 损坏 cache.db] |
| `Stopping → Stopping` | 幂等（第二次是 no-op，不重复发 SIGTERM） | [推测] |
| `Stopped → Stopping` | **拒绝** | [推测] |
| `Running → Starting` | **拒绝**（必须先 `Stopping`/`Stopped`） | 防止 reload 与 restart 竞争 |
| `Failed → Running` | 只能经 `Starting` | [推测] |
| `Degraded → Starting`（隐式重启） | **拒绝**：必须先显式 `stop()` | [实测：`m_port2.out` 的场景重启无用（端口仍被占），盲目重启只会制造 crash loop] |
| 并发 `reload()` | **串行化** | [实测 `x_logs.out`：连续 reload 产生**叠加**的多组初始化日志段，说明 mihomo 不拒绝并发 reload] |

### 11.3 并发串行化方案

```text
per-instance `tokio::sync::Mutex<LifecycleState>`（或等价 Command Queue）：
  所有 start/stop/restart/reload 必须先拿锁再判状态再动作。
  锁内状态机是唯一的权威；外部只能观测，不能直接改。

理由（实测）：
  - mihomo 自己不互斥（y_dup.out：两个实例同时跑）
  - mihomo 不拒绝并发 SIGHUP（x_logs.out：日志出现多组叠加的初始化段）
  - 共享 -d 目录会撞 cache.db（y_dup.out：CacheFile timeout）
  ⇒ 串行化必须由 Agent 承担，不能委托给 mihomo。
```

[实测 `y_dup.out`、`x_logs.out`][推测：锁的具体形态]

### 11.4 状态持久化

```text
/var/lib/proxy-agent/state/mihomo.json
  { pid, started_at, config_version, config_checksum, state }
```
用途：Agent 自身重启（systemd `Restart=`）后接管。**注意**：单 unit 形态下 systemd 会把整个 cgroup 收掉，不会有遗留进程；但 `direct-process` fallback 形态下会有。持久化状态必须能被校验（checksum + pid 存活检查），不能盲信。[推测，对齐 `09-linux-runtime.md` §11.2]

---

## 12. 与 systemd 的重启分工

### 12.1 上游事实（来自 `/tmp/r03-runtime/systemd-research.md`，已并入本节）

该文件是原调研者完成的 systemd 上游证据研究（634 行，只做 web 抓取，未做实测）。与本文直接相关的结论：

| # | 结论 | 来源等级 |
|---|---|---|
| S1 | mihomo **不支持 `sd_notify`**：`go.mod`/`go.sum` 无 `go-systemd`；`main.go` 完全自管 `os/signal`；官方两套 unit 都写 `Type=simple`。⇒ **就绪探测不能等 `READY=1`，必须轮询 `GET /version`。** | [上游源码][上游文档] |
| S2 | mihomo **不写 PID 文件**（全仓库文件清单检索 `pid` 命中 0）。⇒ 以 systemd `MainPID` 或自己 spawn 的 handle 为准。 | [上游源码：缺席证据] |
| S3 | 官方 unit 的 `Restart=on-failure` + `RestartSec=10`（仓库版）或 `Restart=always`（wiki 版）；`ExecReload=/bin/kill -HUP $MAINPID`。 | [上游源码][上游文档] |
| S4 | `POST /restart` 与 `/upgrade` 在 Linux 上是 `syscall.Exec`，**PID 不变，systemd 不感知**（`NRestarts` 不增）。 | [上游源码 `hub/route/restart.go`] |
| S5 | `SIGHUP` 重解析的是**启动时确定的路径**；`configBytes` 非空（`-config`/`-f -`）时 SIGHUP **静默无效**。 | [上游源码 `hub/hub.go`] |
| S6 | `iptables` 配置失败会 `os.Exit(2)` → 配 `Restart=on-failure` 会造成**重启循环**。必须在校验层拦截 `tun.enable` + `iptables.enable` 并存。 | [上游源码 `hub/executor/executor.go`] |
| S7 | unix socket 目录由 mihomo `MkdirAll(dir, 0o755)`；已存在则不覆盖权限 ⇒ Agent 可预建 `0750` 收紧。socket 文件硬编码 `chmod 0666`。 | [上游源码] |
| S8 | `Meta` 分支与 `v1.19.20` 的 CLI flag 有漂移 ⇒ 能力探测按 `-v` 版本分支，不硬编码。 | [上游源码] |

**S1/S2/S3 与本文实测方向完全一致**（本文实测到前台单进程、无 PID 文件、`Type=simple` 语义的就绪行为），可作为互证。[实测 + 上游]

### 12.2 谁负责重启（推荐）

**MVP 形态 A（单 unit：`systemd → proxy-agent → mihomo`）——由 Agent 负责重启 mihomo：**

```text
systemd 的 Restart= 只负责 **proxy-agent 自己**；
mihomo 的崩溃恢复由 Agent 内部的 supervisor task 负责（退避重启）。
```

| 角色 | 负责对象 | 手段 |
|---|---|---|
| Agent supervisor task | mihomo 进程 | 内部退避重启（1s → 2s → 4s → … 上限 60s，+0–500ms jitter） |
| systemd | `proxy-agent.service` | `Restart=on-failure` + `RestartSec=5s` |

**理由（本文证据支撑）**：
- 实测 `y_dup.out` 证明**盲目重启会与端口冲突耦合**成 crash loop：若 mihomo 因为端口被占而"看起来挂了"，Agent 必须能识别这是 `Degraded` 而非 `Failed`，**不做重启**（重启无法解决端口占用）。这正是 `Degraded` 状态（§11.1）存在的第二个理由。[实测 `y_dup.out`、`m_port2.out`]
- 单 unit 形态下 mihomo 与 Agent 同 cgroup，系统保证不会有遗留进程，Agent 的 supervisor 是唯一权威。[推测，对齐 `09-linux-runtime.md` §8.3]

**形态 B（双 unit）——由 systemd 负责重启 mihomo**：`mihomo.service` 用 `Restart=always`，Agent **只观察不插手**。[上游文档]

### 12.3 避免"双重重启 / 双重退避"

```ini
# proxy-agent.service（形态 A）
[Service]
Restart=on-failure
RestartSec=5s            # 注意：这是给 Agent 自己的，不是给 mihomo 的

[Unit]
StartLimitIntervalSec=0  # 关掉 systemd 的启动限流，避免与 Agent 内部退避双重计数
```

**硬性规则：绝不同时启用 Agent 内部退避与 systemd 的 `RestartSteps=`/`RestartMaxDelaySec=`。** 两套指数退避会**相乘**，恢复时间不可预测。[上游文档 `systemd.service`；对齐 `09-linux-runtime.md` §8.3]

**并且：退避重启本身必须是"有上限且有出口"的**：

```text
连续 N 次（建议 5）快速失败 → 停止自动重启，进入 Degraded/Failed 并写审计日志 + 发事件，
等人工介入。否则 -t 校验失败 / 端口永久被占这类"确定性失败"会造成无限重启。
```
[推测][实测支持：`y_geo.out` 的 `fatal`（geodata 下载失败）就是一类**确定性启动失败**——重启一万次也没用，只会重复 90 秒超时]

### 12.4 systemd 侧就绪判定

- **不能用 `Type=notify`**（mihomo 无 sd_notify）。[上游]
- Agent 自己的 `Type=notify` 是**可行的**（Rust 侧可自己实现 `sd_notify`），但**必须等 L1（controller 可达）通过后再 `READY=1`**，否则会重演"container 状态 = healthy 但代理不可用"的问题。[推测]

---

## 13. 对 Agent 架构的影响

1. **`ServiceManager`（生命周期）与 `ProcessManager`（子进程原语）必须分开**：前者是可替换的生命周期抽象（`DirectProcess` / `SystemdUnit`），后者是它的内部依赖。[推测 + 对齐 R09 §11.1]

2. **`MihomoController` 只暴露稳定子集**：`/version`、`/configs`、`PUT /configs`、`/proxies`、`/connections`、`/traffic`、`/logs`。**排除** `/upgrade`（R01 §1.9 风险）与 `/restart`（PID 不变陷阱，S4）。[上游 + 实测]

3. **reload 走 controller 优先、SIGHUP 降级**：这是"配置版本化 → 激活 → reload → health check → 失败回滚"事务能成立的前提，因为只有 controller 路径能给出结构化失败。[实测 §5.4 + R01 §1.6]

4. **必须实现 `Degraded` 状态**：实测反复出现"进程活着但功能坏了"，二元状态机无法表达。[实测 §9.1]

5. **per-instance lock 是安全要求而非优化**：不锁会撞端口 + 损坏 `cache.db`。[实测 `y_dup.out`]

6. **就绪探测用 `GET /version`，超时要短（≤500ms）但 `Starting` 的总超时要长**：因为 geodata 下载会让 controller 长时间不就绪（实测约 90s 后 fatal）。**`Starting` 超时建议按配置能力分档**：不含 GEOIP/GEOSITE 的配置用 15–20s；含 geodata 依赖的配置要么预置文件，要么给 120s+ 并在超时后明确报"geodata 下载超时"。[实测 `y_geo.out` + 推测]

7. **日志两条通道都要接**：`/logs` NDJSON（实时、结构化、粒度到秒）与进程 stdout（RFC3339 纳秒、logrus text）。**不要假设 stdout 是行刷新的**（§3.4 `[未验证]`），因此**实时日志功能必须基于 `/logs`**。[实测 + `[未验证]`]

8. **绝不用 `-config` / `-f -` / `-post-up` / `-post-down`**：前两者破坏 SIGHUP reload（S5），后两者是 `/bin/sh -c` 任意命令执行。[上游]

9. **audit 日志必须覆盖**：`mihomo.start / stop / restart / reload / crash_detected / degraded_enter`。[推测，对齐 AGENTS.md]

10. **`-t` 是离线校验的第一道闸**：`mihomo -d <dir> -f <file> -t`，exit 0/1。但它**不能覆盖运行时端口占用与 geodata 下载**这两类实测到的失败模式，因此 `-t` 通过 ≠ 能启动成功。[实测 + 上游 `-t`][对齐 R09 §8 第 4 条]

---

## 14. 证据与来源（文件清单 + 结论→文件 映射表）

### 14.1 证据目录：`/tmp/r03-runtime/`

```text
/tmp/r03-runtime/
├── mihomo                        实测用二进制（v1.19.30 darwin arm64）
├── help.txt → logs/help.txt      mihomo -h 输出
├── exp_startup.sh                §3.2 启动→就绪计时（3 次）
├── exp_signals.sh                §5 SIGTERM/INT/HUP×3/USR1/USR2
├── exp_signals2.sh               §5.3/§5.4 复验：SIGQUIT 对照、USR1/USR2、HUP reload 效果
├── exp_signals3.sh               §5.3 继承 sigignore、SIG_DFL 复验、§5.4 HUP 窗口计数
├── exp_misc.sh                   §4/§6/§7 daemon/PID、SIGKILL、unix socket、端口冲突、/logs
├── exp_more.sh                   §7.1/§8/§5.4 SIGKILL 后重启、/logs 流、原子替换+HUP、geodata
├── exp_final.sh                  §3.3/§3.4/§7 geodata 长轮询、stdout flush、重复实例、TERM×5
├── systemd-research.md           §12 的 systemd 上游证据（634 行，纯 web 抓取）
├── home/                         -d 目录（config.yaml / config.pristine.yaml / cache.db）
├── nodir/                        §4.2 自动创建的目录
├── rt/  rt2/                     §6 unix socket 目录（socket 文件至今残留）
├── t/                            配置校验样本（本次未使用）
└── logs/                         全部实测输出（67 个文件）
```

### 14.2 结论 → 证据文件映射表

| 结论（章节） | 证据文件 | 等级 |
|---|---|---|
| CLI flag 全量，无 daemon/pidfile（§2.2） | `logs/help.txt` | [实测] |
| 版本 v1.19.30（§2） | `logs/v1.txt`、`v2.txt`、`v3.txt` | [实测] |
| 就绪日志行序列（§3.1） | `logs/m_daemon.out`、`m_unix.out` 等全部 `*.out` | [实测] |
| 前台单进程、kill `$!` 有效（§4） | `logs/m_daemon.out` + `exp_misc.sh` 逻辑 | [实测] |
| 无 PID 文件；`-d` 产物仅 cache.db + config.yaml（§4/§4.1） | `/tmp/r03-runtime/home/`、`nodir/` 磁盘清单 | [实测] |
| `-d` 不存在 → 自动建目录 + 默认配置 + 撞 7890 端口（§4.2） | `logs/m_nodir.out`、`nodir/config.yaml`、`nodir/cache.db` | [实测] |
| **`SIGTERM` 优雅关闭**（§5.1/§5.2） | `logs/sig_term.out`、`y_t1.out`~`y_t5.out` | [实测] |
| **`SIGINT` 优雅关闭**（§5.1） | `logs/sig_int.out` | [实测] |
| **`SIGHUP` reload（配置未变）**（§5.4 A） | `logs/sig_hup_nop.out` | [实测] |
| **`SIGHUP` reload（配置有效变更）**（§5.4 B） | `logs/sig_hup_reload.out`（仅证流程，**mode 变化未能确证**） | [实测/证据不足] |
| **`SIGHUP` + 坏配置 → 失败安全**（§5.4 C） | `logs/sig_hup_broken.out` | [实测] |
| reload 中途 `Server closed` 是正常态（§5.4） | `sig_hup_nop.out`、`sig_hup_reload.out`、`s2_hup.out`、`s3_hupwin.out`、`x_logs.out`、`x_rename.out` | [实测] |
| `SIGUSR1`/`SIGUSR2` 不 reload 不退出（观察窗内）（§5.3） | `logs/s2_usr1.out`、`s2_usr2.out`、`s3_dfl.out`、`s3_dfl2.out`、`sig_usr1.out`、`sig_usr2.out` | [实测] |
| `SIGUSR1`/`SIGUSR2` 未注册（默认处置=终止） | 上游 `main.go`；本目录**未能决定性地验证** | [上游源码] + [未验证] |
| `SIGQUIT` → 栈转储（§5.1 对照） | `logs/s2_quit.err`（21989 B Go stack dump）、`s2_quit.out` | [实测] |
| HUP 窗口内 400 次探测 0 次非 200（§5.4） | `logs/s3_hupwin.count`（`0 400`） | [实测] |
| **unix socket 文件权限 0666**（§6.1） | 磁盘 `rt/mihomo.sock`、`rt2/mihomo.sock`（`srw-rw-rw-`） | [实测] |
| **unix socket 父目录不存在时自动创建 0755；预建 0750 不被覆盖**（§6.1） | 磁盘 `rt/`(0755)、`rt2/`(0750) | [实测] |
| unix socket 与 TCP 可同时监听（§6.1） | `logs/m_unix.out`（两行 listening） | [实测] |
| `SIGKILL` 后 socket 残留；重启可覆盖（§6.1） | 磁盘残留 + `logs/m_unix2.out` | [实测] |
| **`SIGKILL` 后端口立刻可复用（无 TIME_WAIT 阻塞）**（§7.1） | `logs/x_kill9a.out`、`x_kill9b.out`（间隔 ~15ms，`b` 完整启动无 bind error） | [实测] |
| `SIGKILL` 无 shutdown 日志（§7.1） | `logs/m_kill9.out`、`x_kill9a.out` | [实测] |
| **controller bind 失败不致命，proxy 照常**（§7.2） | `logs/m_port.out` | [实测] |
| **mixed-port bind 失败不致命，controller 照常**（§7.3） | `logs/m_port2.out` | [实测] |
| **重复实例不互斥 + 撞 cache.db**（§7.4） | `logs/y_dup.out`、`y_flush.out` | [实测] |
| geodata 自动下载 + 阻塞就绪（§3.3） | `logs/x_geo.out`（短窗未跑完）、`y_geo.out`（跑满，fatal） | [实测] |
| `/logs` NDJSON 标准格式（§8.1） | `logs/logs_std.txt` | [实测] |
| `/logs` structured 格式 + 秒级时间戳（§8.1） | `logs/logs_struct.txt` | [实测] |
| `?level=error` 过滤 → 0 字节（§8.2） | `logs/logs_err.txt`（0 B） | [实测] |
| 非法 level 的响应体 `{"message":"Body invalid"}`（§8.2） | `logs/ws2.txt` | [实测]（状态码 [未验证]） |
| 连续 reload 产生叠加日志段（§11.2） | `logs/x_logs.out` | [实测] |
| 原子替换 + HUP 后实例仍在工作（§5.4 B） | `logs/x_rename.out`（含 `[TCP] ... using DIRECT`） | [实测] |
| **所有退出码数字** | — | [未验证]（仅存在于未落盘的脚本 stdout） |
| **启动→controller 可用耗时（ms）** | — | [未验证]（同上） |
| **停机耗时（ms）** | — | [未验证]（同上） |
| **`curl --unix-socket` 的状态码矩阵** | — | [未验证]（引用 R01 §1.3 [实测]） |
| **stdout 是否行刷新** | — | [未验证]（`y_flush.out` 只证最终写入） |
| **WS 握手 101** | — | [未验证]（引用 R01 §1.5 [实测]） |

### 14.3 上游来源

- **`/tmp/r03-runtime/systemd-research.md`**（同目录，634 行）— systemd unit、sd_notify、capabilities、unix socket 源码、信号处理、API 端点的完整上游证据与精确 URL 清单。§10 列出的所有 `raw.githubusercontent.com` URL 均在该文件内可查。
- **`docs/research/01-mihomo.md`** — controller route 清单、鉴权矩阵（TCP 401/200、Unix 恒 200）、`PUT /configs` 原子语义、`/upgrade` 风险、`/restart` 的 `syscall.Exec`。[实测]
- **`docs/research/09-linux-runtime.md`** — systemd 集成策略、信号语义（§8.1）、停止序列（§8.2）、重启分工（§8.3）、Port 划分（§11.1）、argv 白名单约束（§11.3）。[上游源码 + 上游文档]
- 上游源码引用（本文正文中标注的路径）：`main.go`、`hub/hub.go`、`hub/executor/executor.go`、`hub/route/server.go`、`hub/route/restart.go`、`common/cmd`。

---

## 15. 未验证假设与开放问题

> 本节是本文最重要的部分之一：**原实验的汇总 stdout 全部丢失**，凡依赖它的结论都已标注为未验证。以下按"是否影响实现决策"排序。

### 15.1 因证据缺失导致无法确定的结论（P0，必须在 Linux 上重测）

| ID | 未验证项 | 为什么重要 | 建议的最小补充实验 |
|---|---|---|---|
| Q1 | **启动 → controller 可用的 wall-clock 耗时** | 决定 `Starting` 超时与 systemd `TimeoutStartSec`；也决定"启动后多久才该开始探测" | Linux 上重跑 `exp_startup.sh`，**把 stdout `tee` 到文件** |
| Q2 | **`SIGTERM` → 退出的耗时与退出码** | 决定 `TimeoutStopSec` / T1 预算（R09 §8.2 建议 10s，但那是推断） | 重跑 `exp_final.sh` C 段，落盘 stdout |
| Q3 | **`SIGHUP` 后 reload 是否真的生效（mode 值变化）** | 决定 Agent 是否能把 SIGHUP 当成可靠的降级 reload 路径 | 重跑 `exp_signals2.sh` 的 `s2_hup` case，**并额外把 `GET /configs` 的 mode 写入 `logs/`** |
| Q4 | **`curl --unix-socket` 的完整状态码矩阵（含错 secret）** | 是本项目的安全红线；目前只能引用 R01 的同环境实测 | 重跑 `exp_misc.sh` §3，落盘 stdout |
| Q5 | **`/logs` 的 WebSocket 握手（101）** | 决定 Agent 是否需要实现 WS 客户端 | 重跑 `exp_more.sh` B 段握手，落盘 stdout；或直接引用 R01 §1.5 |
| Q6 | **stdout 缓冲模式（行刷新 vs 块缓冲）** | 决定"实时日志"是否可能基于 tail stdout 文件 | 重跑 `exp_final.sh` B 段，落盘 stdout |
| Q7 | **`SIGTERM` 后 unix socket 文件是否被清理** | 决定 Agent 是否需要主动清理 socket | 单独测：起实例 → TERM → `ls -l` socket 路径 |
| Q8 | **`SIGKILL` 的 `reap_ms` 与端口释放的逐步探测序列** | 只在 `x_kill9b.out` 上间接验证了"可立刻重启"，没有数字 | 重跑 `exp_misc.sh` §2，落盘 stdout |
| Q9 | **是否真的没有子进程**（`pgrep -P`） | 影响进程组 kill 策略与 `KillMode` | 重跑 `exp_misc.sh` §1，落盘 stdout |
| Q10 | **非法 `level` 的 HTTP 状态码** | Agent 参数校验错误处理 | `curl -i` 重测（当前只有 body，无状态行） |

### 15.2 平台差异导致的未验证（P0，Linux/PVE LXC 必须复测）

| ID | 未验证项 | 说明 |
|---|---|---|
| Q11 | **socket 文件权限在 Linux 上是否仍为 0666** | macOS 与 Linux 的 `os.Chmod` 语义一致，且上游是显式 `os.Chmod(addr, 0o666)`，**预期一致**，但仍需断言。[`[未验证]`] |
| Q12 | **`SIGKILL` 后端口立刻复用** | 监听 socket 不受 `TIME_WAIT` 影响是通用语义，但 Linux 上若有 `SO_REUSEADDR` 之外的差异需实测。 |
| Q13 | **`bind: address already in use` 的时机与进程存活** | 与 Q12 同批复测。 |
| Q14 | **geodata 下载在 Linux 上的超时行为** | macOS 上实测 90s 后 fatal；Linux 上超时值可能不同。且**目标部署环境可能根本无外网** → 预置 geodata 是必需策略。 |
| Q15 | **`/logs` structured 时间戳的时区** | 实测只有 `HH:MM:SS`，未测时区。 |
| Q16 | **`Ctrl+C`（终端 SIGINT）与 `kill -INT` 是否有差异** | 实测用的是 `kill -INT`。 |

### 15.3 设计推断（已在正文标注，需 ADR 收口）

| ID | 推断 | 依据 |
|---|---|---|
| Q17 | `ServiceManager` + `ProcessManager` + `MihomoController` + `HealthChecker` 四者的层次划分 | §10.1；与 R09 §11.1 的命名冲突需要 ADR |
| Q18 | `Degraded` 状态的精确定义（哪几层失败算 Degraded 而非 Failed） | §9.1/§11.1 |
| Q19 | 退避参数（1s→60s，5 次失败停止） | §12.3；R09 §8.3 给的是同一套数字，但同为推断 |
| Q20 | `Starting` 超时按"是否含 geodata 依赖"分档 | §13 第 6 条 |
| Q21 | L2 用 checksum 还是关键字段比对 | §9.2 |
| Q22 | `PUT /configs` vs `SIGHUP` 的优先级 | §5.4；R09 §8.1 建议 controller 优先，本文证据（结构化错误）方向一致 |

### 15.4 明确不构成结论的观察（防止被误用）

- ❌ `sig_term.out` 里 `12:42:18.050` → `12:42:18.332` 的 282ms **不是停机耗时**（是脚本的固定 `sleep 0.4` + 轮询收尾）。**不要引用这个数字。**
- ❌ `sig_usr1.out` / `sig_usr2.out` 里的 10 秒**不是 USR1/USR2 的效应**（是脚本"仍存活则等 6s 再补 TERM"的逻辑）。**这两个文件不能用来论证 USR1/USR2 导致退出。**
- ❌ `x_geo.out` **没有跑完**（观测窗 6s ≪ 90s 下载超时），**不能**用它论证"geodata 下载最终失败"。以 `y_geo.out` 为准。
- ❌ `s3_hupwin.count` 的 `0 400` **不能**表述为"reload 零中断"，只能表述为"400 次串行探测未观测到非 200"。
- ❌ `logs/*.err` 除 `s2_quit.err` 外**全为 0 字节**——不能由"stderr 为空"推出"没有 stderr 输出"（脚本可能把 stderr 与 stdout 合并，如 `exp_misc.sh` 的 `2>&1`）。
