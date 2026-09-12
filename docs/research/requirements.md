# Requirements — Phase 0 可验证需求集

> 状态：v0.1（Phase 0 汇总，已与 `AGENTS.md`、设计文档、R01–R15 结论对齐）
> 调研日期：2026-09-12
> 说明：每条需求必须**可验证**，并在右侧给出验证方式；`[P0]` 表示 MVP 阻塞项。
> 约定：MUST = MVP 必须；SHOULD = MVP 尽量；LATER = 明确 defer。

本文件是 Phase 0 与实现阶段之间的契约：每一条 REQ 都应能在 `crates/` 中找到落地位置，并映射到至少一个测试。

---

## 1. 架构与依赖（REQ-ARCH）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-ARCH-001 | MUST `[P0]` | CLI、TUI、Web API 必须调用**同一组 Application Use Case**，接口层不得包含业务规则分支 | 代码评审 + 接口层无 `infrastructure` 依赖（`cargo tree`/CI 检查）；同一操作在 CLI 与 API 上产生等价结果 |
| REQ-ARCH-002 | MUST `[P0]` | 依赖方向必须为 interfaces → application → domain；infrastructure → application ports；禁止反向依赖 | CI 中对 `crates/domain`、`crates/application` 的依赖做静态检查（允许列表） |
| REQ-ARCH-003 | MUST `[P0]` | `crates/domain` 不得依赖 tokio / reqwest / sqlx / axum / systemd / 文件系统 / 进程 | `cargo tree -p proxy-domain` 白名单测试 |
| REQ-ARCH-004 | MUST `[P0]` | 所有进程执行（systemctl、mihomo、nft、iptables）只能出现在 infrastructure 层的 Port 实现中 | 全仓 `grep` 检查 `std::process::Command` / `tokio::process` 出现位置 + 评审 |
| REQ-ARCH-005 | MUST | Port 必须小而聚焦（单一能力），禁止把外部系统 1:1 映射成巨型接口 | 评审：每个 Port ≤ ~8 方法且围绕一个能力 |
| REQ-ARCH-006 | MUST | 不得新增 `POST /api/run-command` 或等价的任意命令执行入口 | API 路由清单测试（白名单） |
| REQ-ARCH-007 | MUST | 事件仅用于通知/协调；不得引入 Event Sourcing、外部消息代理（Kafka/NATS/RabbitMQ） | 依赖清单评审；`Cargo.toml` 依赖白名单 |
| REQ-ARCH-008 | SHOULD | 所有对外 DTO 与 Domain 实体分离，Domain 类型不得直接作为 API 响应 | API 层代码评审 + 序列化测试 |

---

## 2. Mihomo 生命周期（REQ-MIHOMO）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-MIHOMO-001 | MUST `[P0]` | Agent 必须能启动 Mihomo 进程 | 集成测试：启动后进程存活 |
| REQ-MIHOMO-002 | MUST `[P0]` | Agent 必须能停止 Mihomo 并等待其退出（超时后强杀） | 集成测试：停止后端口释放、进程消失 |
| REQ-MIHOMO-003 | MUST | 支持 restart / reload（reload 复用运行中的进程） | 集成测试：reload 后 PID 不变、配置生效 |
| REQ-MIHOMO-004 | MUST `[P0]` | Agent 必须能检测 Mihomo 进程异常退出（崩溃检测） | 测试：`kill -9` 后状态在 ≤ 2 个探测周期变为 Failed |
| REQ-MIHOMO-005 | MUST | 生命周期操作必须按实例串行化；非法状态转换必须被拒绝且不产生副作用（例如 `Starting → Starting` 不得二次 spawn） | 单元测试：并发调用 start 只 spawn 一次；状态机测试覆盖非法转换 |
| REQ-MIHOMO-006 | MUST | 必须能查询并报告 Mihomo 版本，且能区分"Agent 期望版本"与"实际运行版本" | 单元测试 + 集成测试 |
| REQ-MIHOMO-007 | MUST | 二进制更新必须是显式 Use Case，与配置更新完全独立（不得合并成一个 "Update" 操作）；**禁止依赖 Mihomo 的 `/upgrade` 与 `/restart`**（`/upgrade` 把"已是最新"当 500、无签名校验；`/restart` 是 `syscall.Exec` 自替换、绕过状态机） | API/CLI 路由与命令清单检查；源码中不出现 `/upgrade`、`/restart` 调用（R01 实测） |
| REQ-MIHOMO-013 | MUST | reload 必须使用 `PUT /configs` 且**必须携带 JSON body**（空 body 会 400）；`path` 模式必须受 `SAFE_PATHS` 约束 | 集成测试：空 body 发送必须被 Agent 阻止（R01 实测） |
| REQ-MIHOMO-014 | MUST | 健康检查必须包含代理端口可达性（L4），不得只依赖 `/version`（listener bind 失败时 `/version` 仍 200） | 集成测试：占用 mixed-port 后状态必须为 Degraded/Unhealthy（R01 实测） |
| REQ-MIHOMO-008 | SHOULD | 二进制更新必须支持回滚到上一个已知可用版本 | 集成测试：更新失败/健康检查失败后回退 |
| REQ-MIHOMO-009 | MUST | 更新后的二进制必须经过校验（校验和/签名策略见 ADR-006）后才可替换 | 单元测试：篡改的产物被拒绝 |
| REQ-MIHOMO-010 | MUST | Agent 必须能读取并流式输出 Mihomo 日志，且日志中的订阅 URL/secret/凭据必须脱敏 | 脱敏单元测试（含 URL query 中的 token/password） |
| REQ-MIHOMO-011 | MUST | 就绪判定不得仅依赖 sleep：必须基于 Controller 可达性或可解析的日志行 | 集成测试：启动后立即可查询状态 |
| REQ-MIHOMO-012 | SHOULD | 重启策略必须与 systemd 的 `Restart=` 不冲突（同一时刻只有一个重启决策者） | 配置评审 + 文档（ADR-003） |

---

## 3. 配置生命周期（REQ-CONFIG）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-CONFIG-001 | MUST `[P0]` | 每一个被激活的配置必须有唯一版本标识与 checksum | 单元测试 + 存储检查 |
| REQ-CONFIG-002 | MUST `[P0]` | 订阅更新/配置生成失败时，**不得替换当前激活配置** | Application 测试：转换失败、校验失败、reload 失败三种路径下 active version 不变 |
| REQ-CONFIG-003 | MUST `[P0]` | 配置写入必须原子化（临时文件 → flush/fsync → rename），不得原地修改激活配置 | 单元/集成测试：注入写入失败后 active 文件完好 |
| REQ-CONFIG-004 | MUST | 支持配置列表 / 查看 / 校验 / diff / 激活 / 回滚 | Use Case 与 CLI/API 覆盖测试 |
| REQ-CONFIG-005 | MUST | 三层校验 + 资源预检：L0 端口/geodata/provider 预检 → L1 YAML 语法 → L2 `mihomo -t` **⊕ Agent 字段白名单** → L3 启动/健康检查；每层失败必须给出可读原因。**`-t` 不检测未知字段**（拼错的 `mixed-portt` 会 exit 0 通过），故 L2 必须有字段白名单 | 单元测试 + 集成测试（含每类错误样例，含拼写错误案例）（R02 实测） |
| REQ-CONFIG-011 | MUST | **禁止使用 `PUT /configs?force=true`**：实测在"解析通过但端口绑定失败"时会拆除旧监听器、留下 `mixed-port: 0` 僵尸态，且后续 reload 无法恢复 | Application 源码检查 + 集成测试：force 参数不得出现（R02 实测） |
| REQ-CONFIG-012 | MUST | L2 的 `mihomo -t` 调用必须：① 在隔离临时 `-d` 目录执行；② **先确认目标配置文件存在**（否则 `-t` 会自动创建初始配置并返回 exit 0 假成功）；③ 对含 `GEOIP`/`GEOSITE` 的配置预置 geodata 或降级为告警（离线环境该调用会真实联网并阻塞约 90s 后失败） | 单元测试：不存在的文件不得通过校验（R02 实测，本会话已独立复现） |
| REQ-CONFIG-013 | MUST | 回滚必须以**重启进程**方式落地，不得用 reload（reload 无法从绑定失败的僵尸态恢复） | 集成测试：构造僵尸态后验证 restart 可恢复（R02 实测） |
| REQ-CONFIG-006 | MUST | 激活流程必须为：生成 → 校验 → 激活 → reload → 健康检查；reload 或健康检查失败必须尝试回滚 | Application 测试：健康检查失败触发回滚（成功与失败两条路径） |
| REQ-CONFIG-007 | MUST | 配置版本必须不可变（immutable），回滚通过"激活旧版本"实现，而不是改写历史 | 存储层设计评审 + 测试 |
| REQ-CONFIG-008 | MUST | 已激活配置必须可追溯来源（订阅 / 手动 / 回滚 / 导入）与生成时间 | 数据模型测试 |
| REQ-CONFIG-009 | SHOULD | 配置校验不得产生副作用（不得在只读校验时启动真实 TUN/改路由）；**注意 `mihomo -t` 并非无副作用**：含 GEOIP/GEOSITE 时会真实下载 geodata，且会自动创建缺失的配置文件 | 集成测试：校验路径下无网络副作用（R02 实测） |
| REQ-CONFIG-010 | SHOULD | 保留策略：配置版本数量上限/清理规则可配置，且不得删除 active 版本 | 单元测试 |

---

## 4. 订阅与转换（REQ-SUB）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-SUB-001 | MUST `[P0]` | Application 只依赖 `SubscriptionConverter` Port；Application 中不得出现 Sub-Store 的 URL、参数名或 JSON 结构 | `crates/application` 源码检查（禁止出现 `download/sub`、`sub-store` 等字符串） |
| REQ-SUB-002 | MUST | 必须支持订阅的增删改查、手动更新、定时更新 | Use Case 测试 + 集成测试 |
| REQ-SUB-003 | MUST `[P0]` | 订阅更新失败（订阅源不可达 / 转换失败 / 输出非法）时，旧配置保持激活 | Application 测试（三种失败注入） |
| REQ-SUB-004 | MUST | 必须防止同一订阅的并发更新（调度器不得产生重复并发任务） | 并发测试：同一订阅同时触发两次只执行一次 |
| REQ-SUB-005 | MUST | 订阅 URL 中的凭据不得出现在日志、API 响应、审计记录中 | 脱敏单元测试 + 日志断言 |
| REQ-SUB-006 | MUST | 转换器必须可替换（至少两种实现可切换，MVP 可为 Sub-Store + NotImplemented Native） | 编译期：切换 adapter 不改 application；启动配置可切换 |
| REQ-SUB-007 | SHOULD | 定时更新必须有明确的失败退避与最大重试策略，且不在失败时禁用代理 | 调度器测试 |
| REQ-SUB-008 | SHOULD | 订阅更新必须记录结果（成功/失败原因/耗时/产出配置版本），可查询 | 数据模型 + API 测试 |
| REQ-SUB-009 | MUST | 抓取订阅 URL 时必须防止 SSRF（拒绝环回、链路本地、元数据地址、RFC1918，或显式白名单）；**注意**：若抓取由 Sub-Store 发起，Agent 无法控制其出站，必须文档化风险（Q012） | 单元测试：`169.254.169.254`、`127.0.0.1`、`::1` 被拒绝 |
| REQ-SUB-010 | LATER | Native Converter 完整实现（URI 解析 + Mihomo 序列化） | — |
| REQ-SUB-011 | MUST | **不得依赖 `GET /download/sub`**（该端点不存在，实测 404）；必须实现 Sub-Store 的两段式模型：幂等写入（`POST /api/subs`）+ `GET /download/:name?target=<白名单>` | Application 源码中不出现 `download/sub`；集成测试（R04 实测） |
| REQ-SUB-012 | MUST | `target` 值必须白名单映射，禁止透传用户输入（`clash` 小写、`ALL` 均返回 500） | 单元测试：非法 target 被拒绝（R04 实测） |
| REQ-SUB-013 | MUST | **Agent 必须自己生成完整 Mihomo 配置**：Sub-Store 只输出 `proxies:` 段，不含 `mixed-port`/`external-controller`/`dns`/`tun`/`rules`/`proxy-groups` | 集成测试：转换产物无法直接启动 Mihomo，必须经 Agent 补全（R04 实测 433 B） |
| REQ-SUB-014 | MUST | 必须能从 Sub-Store 的 JSON 错误响应正确映射错误（错误 body 是 JSON 但 `Content-Type` 为 `text/plain`） | 单元测试：404/500/400 三类映射（R04 实测） |
| REQ-SUB-015 | MUST | **不得使用 sub-store-convert**：其失败时会成功 resolve 并输出空 `proxies:`（9 B / 0 节点），违反 REQ-CONFIG-002；且许可阻塞（内联 AGPL 无声明） | 依赖清单与设计评审（R06、R13） |

---

## 5. 系统能力与 PVE LXC（REQ-LXC / REQ-SYS）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-LXC-001 | MUST `[P0]` | Agent 必须检测 `/dev/net/tun` 是否存在**以及是否可打开**，并进一步验证 `ioctl(TUNSETIFF)` 能否成功（实测：capability 检查才是真门槛，设备存在且可 `open()` 仍可能 `EPERM`） | 单元测试（注入探测结果）+ Linux 集成测试（R10 实测） |
| REQ-LXC-002 | MUST `[P0]` | 能力状态必须是五值枚举 `Supported / Unsupported / Unavailable / Misconfigured / Unknown`，禁止用 bool 表达兼容性 | 类型系统（enum）+ 评审 |
| REQ-LXC-009 | MUST | `Misconfigured` 判定必须覆盖：有设备但 `EACCES`/`TUNSETIFF EPERM`；能建 tun 但 `ip_forward=0`；`route_localnet=0` 却启用 TProxy；Sub-Store 监听非 loopback；Mihomo controller 监听非 loopback；socket 权限未收紧 | 单元测试矩阵（R10 §6.3、R12 §10） |
| REQ-LXC-010 | MUST | TProxy 前置条件必须把 **sysctl 可写性**（`ip_forward`/`route_localnet`）单列为独立探测项（否则 TProxy 静默失效） | doctor 输出断言（R11 C5） |
| REQ-LXC-011 | MUST | 内核更新路径**禁止依赖** Mihomo 的 `/upgrade`；必须自研"下载 → 校验 checksum → 原子替换 → 重启 → 健康检查 → 回滚" | 架构检查 + 集成测试（R01、ADR-003 D2） |
| REQ-LXC-012 | SHOULD | 无 systemd 环境（容器内 `/proc/1/comm != systemd`）必须能回退到 direct-process 管理，且不得调用 `systemctl` | 集成测试（R10 实测容器内无 systemd） |
| REQ-LXC-003 | MUST | 必须检测：privileged/unprivileged LXC、容器环境（LXC/Docker/VM/bare metal）、CAP_NET_ADMIN、nftables、iptables、路由能力、systemd 可用性 | Docker/PVE 探测测试 + 单元测试（伪装的 `/proc` 输入） |
| REQ-LXC-004 | MUST | 单项能力不可用不得导致无关功能失败（例如 TUN 不可用仍可启动 HTTP/SOCKS） | 集成测试：无 TUN 环境下 basic proxy 仍 Healthy |
| REQ-LXC-005 | MUST | 探测操作必须无副作用（不得修改路由/防火墙/网络配置） | 代码评审 + strace/行为测试（探测路径不调用写操作） |
| REQ-LXC-006 | MUST | `doctor` 必须输出系统、Mihomo、网络、运行时的分组能力报告，并给出总体结论（Basic Proxy / TUN / Transparent） | CLI/API 快照测试 |
| REQ-LXC-007 | SHOULD | 探测结果必须携带证据（命令/系统调用/文件）与采集时间，便于排障 | 输出结构测试 |
| REQ-LXC-008 | SHOULD | doctor 必须可通过 API 与 CLI 以机器可读格式（`--json`）输出 | JSON schema 测试 |

---

## 6. 网络栈（REQ-NET）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-NET-001 | MUST `[P0]` | MVP 必须支持 HTTP/SOCKS/Mixed 入站代理（不依赖任何特权或内核特性） | 集成/E2E：本地端口可用 |
| REQ-NET-002 | MUST | TUN 属于**可选启用**能力：检测不可用时不启用、不报致命错误 | 集成测试 + doctor 断言 |
| REQ-NET-003 | MUST | Agent 不得在未被显式要求时修改宿主机防火墙或路由 | 代码评审 + 探测/默认路径测试 |
| REQ-NET-004 | MUST | 若实现防火墙规则应用，必须提供：规则集快照、显式回滚、超时自杀（watchdog）机制 | 集成测试（容器内）+ 设计评审 |
| REQ-NET-005 | SHOULD | 若实现防火墙规则应用，必须先支持 dry-run（只输出规则文本） | CLI/API 测试 |
| REQ-NET-006 | LATER | TProxy / redirect 自动配置、策略路由、DNS 劫持的自动化 | — |
| REQ-NET-007 | MUST | 必须检测防火墙后端（nftables / iptables-legacy / iptables-nft）并报告，而不是假定其一 | doctor 测试 |

---

## 7. 接口层（REQ-API / REQ-CLI / REQ-TUI / REQ-UI）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-API-001 | MUST | REST API 必须版本化前缀 `/api/v1`，并提供 system/health/doctor/mihomo/configs/subscriptions 端点 | 路由清单测试 |
| REQ-API-002 | MUST | 长时操作必须返回可查询的任务状态（不得阻塞 HTTP 请求直到完成） | 集成测试 |
| REQ-API-003 | MUST | WebSocket 事件端点 `/ws/v1/events` 必须提供状态、日志、任务事件 | 集成测试 |
| REQ-API-004 | MUST | 远程可达时必须启用认证（见 REQ-SEC-004），未认证请求返回 401 且不泄露内部错误 | 集成测试 |
| REQ-CLI-001 | MUST | CLI 必须覆盖 start/stop/restart/reload/status/logs/mihomo/subscription/config/doctor | CLI 帮助快照测试 |
| REQ-CLI-002 | MUST | CLI 必须支持机器可读输出（`--json`）且退出码区分成功与操作失败 | 集成测试（退出码断言） |
| REQ-CLI-003 | MUST | CLI 默认不得打印 secret/凭据 | 输出快照测试 |
| REQ-TUI-001 | MUST | TUI 必须通过 Application Use Case（或 Agent API）操作，不得直接管理 Mihomo 进程 | 代码评审 + 依赖检查 |
| REQ-TUI-002 | SHOULD | TUI 优先覆盖：overview、runtime status、proxy groups、logs、subscription update、config rollback、doctor | 功能清单 |
| REQ-UI-001 | MUST | Web Admin 与 Dashboard 集成必须复用上游 metacubexd 产物，不修改其核心代码 | 仓库结构 + 构建流程评审 |
| REQ-UI-002 | SHOULD | Agent Web 必须提供单一入口（同一端口）访问 Admin 与 Dashboard | E2E |

---

## 8. 安全（REQ-SEC）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-SEC-001 | MUST `[P0]` | Agent 生成的 Mihomo 配置必须设置非空 `secret`，或使用受权限保护的 unix socket；不得生成无鉴权的 `0.0.0.0` controller。**实测依据**：`secret` 为空时鉴权中间件根本不挂载（所有 route 200） | 配置生成单元测试 + 评审（R12 §1） |
| REQ-SEC-002 | MUST `[P0]` | 若使用 `external-controller-unix`，Agent 必须在 Mihomo 创建 socket 后收紧权限（Mihomo 自身硬编码 `chmod 0666`，且 **unix 模式完全不校验 secret**）；`/run/proxy-agent` 必须为 0750 | 集成测试：socket mode 为 0660；非授权用户连接失败（R01/R09/R12 三方确认） |
| REQ-SEC-011 | MUST `[P0]` | Agent 生成配置必须收窄 CORS：`allow-origins` 显式设为空/白名单，`allow-private-network: false`（实测默认 `["*"]` + `true`，且显式写 `[]` 仍返回 `*`） | 配置生成单元测试 + doctor 告警（R12 §1、源码 `config/config.go:595-598`） |
| REQ-SEC-012 | MUST | controller 的 host 必须非空且为 loopback；`:9090` 这类写法会绑 `[::]`（全网卡），必须在生成期拒绝 | 单元测试：`":9090"`、`"0.0.0.0:9090"`、空 host 被拒绝（R12 实测） |
| REQ-SEC-013 | MUST | 非 loopback 监听 Agent API 而无 token 时必须**拒绝启动**（而不是仅告警） | 集成测试（ADR-005 D3） |
| REQ-SEC-003 | MUST | Mihomo controller 默认绑定 `127.0.0.1` 或 unix socket；绑定 `0.0.0.0` 必须显式配置且给出告警 | 配置生成测试 |
| REQ-SEC-004 | MUST `[P0]` | Web 远程访问必须认证；token 必须存储为哈希、可轮换，且不出现在日志/API 响应 | 单元测试 + 审计测试 |
| REQ-SEC-005 | MUST | Agent 本地管理接口默认通过 Unix socket（`/run/proxy-agent/agent.sock`），权限受 OS 保护，并校验 peer 身份（UID/GID） | 集成测试：跨用户访问被拒绝 |
| REQ-SEC-006 | MUST | 日志与审计必须脱敏：订阅 URL 凭据、secret、token、代理凭据、完整敏感配置 | 脱敏单元测试（正则矩阵） |
| REQ-SEC-007 | MUST | 高风险操作必须写审计记录（who/when/action/target/result），且审计只追加不可改 | 集成测试 |
| REQ-SEC-008 | MUST | 特权操作必须映射到显式 Use Case + Port；接口层不得持有 root shell | 架构检查（REQ-ARCH-004/006） |
| REQ-SEC-009 | SHOULD | systemd 沙箱按能力分档（proxy-only / tun-enabled），且不得因沙箱导致 TUN 静默失效 | 单元文件评审 + 文档（ADR-005） |
| REQ-SEC-010 | SHOULD | 订阅抓取与转换必须限制出站目标（防 SSRF/内网探测） | 同 REQ-SUB-009 |

---

## 9. 部署与运维（REQ-OPS）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-OPS-001 | MUST | MVP 支持 Debian/Ubuntu + systemd 安装（deb 与 install.sh 至少其一，建议 deb 为主） | E2E 安装测试 |
| REQ-OPS-002 | MUST | 安装/升级不得覆盖用户配置（遵循 conffile 语义），且升级后可回滚 | E2E + 打包评审 |
| REQ-OPS-003 | MUST | `doctor` 必须在安装后作为健康门槛可运行（安装流程末尾执行） | E2E |
| REQ-OPS-004 | MUST | Agent 与 Mihomo 的升级、配置回滚必须是三条独立链路 | 评审 + E2E |
| REQ-OPS-005 | MUST | 数据目录职责明确：`/etc/proxy-agent`（配置）、`/var/lib/proxy-agent`（状态/版本/DB）、`/run/proxy-agent`（socket） | 打包评审 + 权限测试 |
| REQ-OPS-006 | SHOULD | 备份/恢复（订阅、配置版本、DB）在 MVP 提供最小可用实现 | 集成测试 |
| REQ-OPS-007 | SHOULD | aarch64 与 x86_64 构建产物一致可用 | CI 构建矩阵 |
| REQ-OPS-008 | LATER | OpenRC、多实例、远程多节点、官方容器镜像 | — |

---

## 10. 可观测性（REQ-OBS）

| ID | 级别 | 需求 | 验证方式 |
|---|---|---|---|
| REQ-OBS-001 | MUST | 使用 tracing 结构化日志，关键字段：`mihomo_instance_id`、`config_version`、`subscription_id`、`job_id` | 日志快照测试 |
| REQ-OBS-002 | MUST | 任何后台任务必须可取消、有错误处理、有明确 owner | 评审 + 测试 |
| REQ-OBS-003 | SHOULD | 健康检查必须区分进程存活 / Controller 可达 / 配置已加载 / 代理端口监听 | 集成测试 |

---

## 11. Requirement → 测试映射总览（MVP 必测路径）

```text
REQ-CONFIG-002 + REQ-SUB-003  订阅更新失败保留旧配置        → Application（mock Port）
REQ-CONFIG-006                激活/reload/健康检查失败回滚   → Application
REQ-MIHOMO-005                并发 start 只 spawn 一次       → Application/Domain
REQ-SUB-004                   同一订阅并发更新阻止           → Application
REQ-CONFIG-003                原子写入失败不破坏 active      → Infrastructure
REQ-LXC-001/002/004           TUN 探测 + 降级                 → Infrastructure（Linux）
REQ-SEC-001/002/003           controller 暴露面与 socket 权限 → Infrastructure
REQ-SEC-006                   日志脱敏                        → Unit
REQ-OPS-002                   升级不覆盖用户配置              → E2E（deb）
```

---

## 12. 待随调研收敛的条目

以下条目的最终级别/数值依赖后续实测（见 `docs/research/open-questions.md`）：

- REQ-MIHOMO-008 二进制回滚的实现形态（保留 N 个版本 vs 依赖发行版包管理）。
- REQ-CONFIG-005 的第三层（启动/健康检查）在 TUN 场景下是否可安全执行。
- REQ-NET-004 watchdog 的具体超时与回滚触发条件。
- REQ-SEC-005 peer 身份校验在 Rust/tokio 下的实现方式（`SO_PEERCRED` 可得性）。
- REQ-OPS-007 是否采用 musl 静态链接以兼容老发行版。
