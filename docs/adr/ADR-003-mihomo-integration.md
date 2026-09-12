# ADR-003 — Mihomo 集成与进程管理

| 字段 | 值 |
|---|---|
| Status | Accepted（Phase 0 定稿） |
| Date | 2026-09-12 |
| Related | ADR-001、ADR-004、ADR-005、`docs/research/01-mihomo.md`、`03-mihomo-runtime.md`、`09-linux-runtime.md` |

---

## 1. Context

Agent 是控制平面，Mihomo 是数据平面。需要在"直接调用 Mihomo API"与"自己实现"之间划出边界，并决定进程管理、控制通道与就绪判定方式。

### 1.1 关键实测结论

| # | 结论 | 证据 |
|---|---|---|
| C1 | 实测版本 `v1.19.30`；`/version` 返回 `{"meta":true,...}`，可据此识别 Meta 内核 | R01 §1 |
| C2 | route 清单与 tag-pinned 源码逐条吻合（~45 条）；**`/script`、`/profile` 不存在**（404，属误传） | R01 §1 |
| C3 | 设了 `secret` 时 TCP controller **所有** route 都要求 `Authorization: Bearer`（含 `/version`） | R01 §1 |
| C4 | **Unix socket 完全不校验 `secret`**：`startUnix()` 传入 `router(cfg.IsDebug, "", ...)`，且 socket 被硬编码 `chmod 0666` | R01 §1、R12 §1、R09 C8（三方独立确认） |
| C5 | **`/restart` 是 `syscall.Exec` 进程自替换**（PID 不变、绕过 Agent 状态机）；返回值与文档所称 `204` 不符 | R01 §1 |
| C6 | **`/upgrade` 不可依赖**：无更新时返回 500（把"已是最新"当错误）、无签名校验、`force=true` 有替换成半截二进制的风险 | R01 §1 |
| C7 | `PUT /configs` **先解析后应用**：非法 YAML → 400 且实例不受影响（旧配置继续生效）；**空 body 会 400，必须发 `{}`**；`path` 受 SAFE_PATHS 白名单约束 | R01 §1 |
| C7b | **⚠️ reload 不是事务性的**：解析通过但 listener 绑定失败时，`ApplyConfig` 无返回值、不回滚、HTTP 仍 204。`force=false` 安全降级；**`force=true` 会拆除旧监听器留下 `mixed-port: 0` 僵尸态，后续 reload 不可恢复 → 必须禁止 `force=true`** | R02 §1 C5/C6（实测） |
| C8 | `GET /configs` 仅 33 字段，**不含** `dns`/`proxies`/`rules`/`providers` → 不能当作完整配置真相 | R01 §1 |
| C9 | `/traffic`、`/memory`、`/connections`、`/logs` 是 **NDJSON + WebSocket 双模** | R01 §1 |
| C10 | `/configs/geo`、`/upgrade/geo` 是 **fire-and-forget**（204 不代表成功） | R01 §1 |
| C11 | **listener bind 失败不致命**：仅记 error 日志，`/version` 仍返回 200，但代理端口可能没起来 | R01 §1 |
| C12 | reload 失败是**失败安全**的（SIGHUP 与 API reload 均保留旧配置） | R03 §5、R01 C7 |
| C13 | **Mihomo 不支持 `sd_notify`**；上游官方 unit 用 `Type=simple` + SIGHUP reload | R09 C5 |
| C14 | 容器内常常**没有 systemd**（`/proc/1/comm=sh`）→ 进程管理必须能回退 | R10 §1 |
| C15 | 内核进程崩溃后重启需退避；systemd `Restart=` 与 Agent 自身重启策略不得双重决策 | R09、R03 |
| C16 | **Mihomo 自带的 legacy iptables 自动化是反面教材**（写全局 PREROUTING/OUTPUT、硬编码 `172.17.0.0/16`、失败 `os.Exit(2)`），绝不可复用 | R11 C9 |

### 1.2 需求约束

REQ-MIHOMO-001~012：启动/停止/重启/reload、崩溃检测、串行化、版本管理、**内核更新与配置更新分离**、日志脱敏、就绪不得靠 sleep。

---

## 2. Decision

### D1. 拆成三个聚焦 Port（不写巨型 `SystemManager`）

```rust
// 命令面：有副作用，需要串行化
#[async_trait]
pub trait MihomoController: Send + Sync {
    async fn version(&self) -> Result<MihomoVersion>;
    async fn runtime_config(&self) -> Result<RuntimeConfigSummary>;
    async fn reload(&self, config: &ConfigPath) -> Result<()>;   // PUT /configs {path}
    async fn proxies(&self) -> Result<ProxyList>;
    async fn select_proxy(&self, group: &str, proxy: &str) -> Result<()>;
    async fn test_delay(&self, name: &str, opts: DelayOptions) -> Result<DelayOutcome>;
    async fn rules(&self) -> Result<RuleList>;
    async fn health_check(&self) -> Result<HealthReport>;
}

// 观测面：只读流，不参与状态机
#[async_trait]
pub trait MihomoObserver: Send + Sync {
    async fn traffic(&self) -> Result<TrafficStream>;      // NDJSON 或 WS
    async fn logs(&self, level: LogLevel) -> Result<LogStream>;
    async fn memory(&self) -> Result<MemoryStream>;
}

// 连接运维：低耦合、隐私敏感
#[async_trait]
pub trait MihomoConnectionOps: Send + Sync {
    async fn connections(&self) -> Result<ConnectionList>;
    async fn close_connection(&self, id: &str) -> Result<()>;
    async fn close_all(&self) -> Result<()>;
}

// 生命周期权威
#[async_trait]
pub trait ProcessManager: Send + Sync {
    async fn start(&self, opts: StartOptions) -> Result<ProcessHandle>;
    async fn stop(&self, timeout: Duration) -> Result<ExitStatus>;
    async fn status(&self) -> Result<ProcessStatus>;
    async fn signal(&self, sig: Signal) -> Result<()>;
}
```

**拆三个的理由**：观测面与命令面的失败语义完全不同（流断开不应影响生命周期状态）；连接明细含 `uid`/`process`/`processPath` 隐私字段（C9/R01），需要独立的访问控制与"不进 Domain"的边界。

### D2. 生命周期权威 = `ProcessManager`；**禁止**依赖 `/restart` 与 `/upgrade`

- `/restart`（C5）绕过状态机、PID 不变、状态全丢 → **禁止调用**。
- `/upgrade`（C6）语义不可靠 → **Agent 自研"下载 + 校验 + 原子替换 + 健康检查 + 回滚"**（与 ADR-004 的配置链路对称）。
- reload 用 **`PUT /configs` 带 JSON body**（有 HTTP 错误反馈），**不用 SIGHUP**（无反馈）；**禁止 `force=true`**（C7b，ADR-004 D3）。

### D3. 控制通道：MVP 优先 Unix socket，但必须补权限

```text
external-controller-unix: /run/proxy-agent/mihomo.sock
```

- 上游硬编码 `chmod 0666` 且不校验 secret（C4）→ Agent **必须在 socket 创建后收紧权限**（`chmod 0660` + 专用 group），并把 `/run/proxy-agent` 建成 `0750`。
- **绝不**把 Mihomo controller 绑到 `0.0.0.0`；TCP 模式必须设非空 `secret` 且只绑 loopback（ADR-005）。
- 该 socket 权限收紧必须纳入 health check（REQ-SEC-002）。

### D4. 进程模型：Agent 作为 supervisor，systemd 管 Agent

- **单 unit**：systemd 只管理 `proxy-agent.service`，Mihomo 是 Agent 的 fork/exec 子进程、同用户、靠 ambient capability 传递所需权限（详见 ADR-005 / R09）。
- **无 systemd 环境**（部分 LXC/容器，C14）回退 `SupervisedChildProcess` 适配器；Application 不得假设 systemd 存在。
- **重启分工**：MVP 由 Agent 做退避重启，`proxy-agent.service` 用 `Restart=on-failure` 兜底；**Mihomo 子进程不由 systemd 单独 restart**（避免双重决策，C15）。

### D4b. 进程身份必须可从 `/proc` 恢复（2026-09-12 修订）

**发现的缺口**：原设计让 Application 只持有内存中的 `ProcessState { handle: Option<ProcessHandle> }`，
而 `ProcessHandle { pid: u32 }` 只有一个 pid。R03 的结论是"Agent 必须以自己 spawn 时的 `Child` handle
为唯一真相来源"——但该结论**在 Agent 重启后失效**。

真机实测（Debian aarch64）确认：

| 事实 | 结果 |
|---|---|
| 子进程在父进程退出后是否存活 | **存活**（reparent 到 init） |
| 未 spawn 它的进程能否按 pid 发信号 | **能** |
| mihomo 是否写 pid 文件 | **不写**（R03） |

**后果**：`systemctl restart proxy-agent` 之后，`current_handle()` 返回 `None` 而内核仍在运行，
于是 `StartMihomo` 看到 `Stopped` 并 **spawn 第二个内核**（端口冲突），或 `StopMihomo`
认为无事可做而**永远停不掉内核**。

**决定**：

1. `ProcessHandle` 增加 `start_time: u64`，取自 `/proc/<pid>/stat` 第 22 字段。
   **必要性**：pid 会被复用，仅凭 pid 可能在复用后把信号发给无关进程；`(pid, start_time)`
   才构成稳定身份。
2. `ProcessManager` 新增 `discover(&self, options: &StartOptions) -> Result<Option<ProcessHandle>, PortError>`，
   按可执行文件路径 + 工作目录 + cmdline 重新发现一个未由本进程 spawn 的内核。
3. `ProcessManager` 新增 `is_alive(&self, handle: &ProcessHandle) -> Result<bool, PortError>`，
   按 `(pid, start_time)` 校验，而非只查 pid。

**替代方案（已否决）**：让 adapter 内部维护 pid 文件来隐藏该缺口。否决理由：
`start` 返回的 handle 在 Agent 重启后依然失效，缺口仍在；且 adapter 会与 Application 的
`ProcessState` 形成**两份真相**，违反"状态归 Application、机制归 Infrastructure"的边界。

**降级行为**：`/proc` 不可读时（hidepid、`/proc` masked），`discover` 返回
`PortError::PermissionDenied`，Application 必须**拒绝启动并给出明确原因**，绝不能
假装内核不存在然后 spawn 第二个。

### D5. 就绪与健康检查分层（不得靠 sleep）

```text
L1 进程存活      → pid 存在 / 子进程未退出
L2 Controller 可达 → GET /version 200（含 secret 校验成功）
L3 配置已加载    → GET /configs 的 mode/ports 与期望一致（注意 C8：字段有限）
L4 代理端口监听  → TCP connect 到 mixed-port（这是 C11 的兜底：bind 失败时 /version 仍 200）
L5 可选数据面    → delay 测试（需显式触发，默认关闭以免产生真实流量）
```

状态：`Healthy` / `Degraded`（L2 通但 L4 不通等）/ `Unhealthy`。

> **实测依据**：L4 不可省略。Mihomo 在 listener bind 失败时**不致命**（仅 error 日志、`/version` 仍 200）——"进程存活 + controller 200" 可能是**僵尸态**（R01 C11、R02 §1 C5）。

### D6. Domain 边界：哪些**不进** Domain

| 不进 Domain | 去处 |
|---|---|
| 连接明细（`uid`/`process`/`processPath`） | Adapter DTO / 事件流（隐私敏感） |
| 逐秒 traffic 采样、memory | 事件流 |
| 日志原文 | 事件流（脱敏后） |
| `/storage` KV、`/dns/query` 结果 | Adapter |
| `test_delay` 的 504 | **业务结果** `DelayOutcome::Timeout`，不是基础设施错误 |

### D7. `MihomoObserver` 的实现形态（2026-09-12，阶段 C 前半）

**实现状态：`KernelObserver` 已实现并接线**（`crates/infrastructure/src/mihomo/observer.rs`）。
原 `UnavailableObserver` 占位与 `OBSERVER_REASON` 已删除。

**用 NDJSON，不用 WebSocket。** 原占位说明写的是「观测流需要 websocket transport」——**这句是错的**，
且方向性地误导。实测（R01 §3.3.2）四个观测端点在不带 `Upgrade: websocket` 时即为 NDJSON，
WS 只在需要 `?token=` 的浏览器场景才需要。故无新增 WS 依赖。

**`/logs` 使用 `format=structured`。** 两种格式实测都是 JSON，差别在字段名与时间：
默认格式是 `type`/`payload`，structured 是 `level`/`message`/`fields`。选 structured 是因为字段名稳定；
代价是 **`time` 只有 `HH:MM:SS`、没有日期**，因此 `LogEntry.at` 一律为 `None`，
**不补当天日期**——跨午夜的补全会静默给出错误时间，而「不知道绝对时间」是诚实的。

**传输层新增 `Transport::open_stream`，不改 `send`。** 两者完成语义不同：`send` 等 body 结束，
而观测 body 永不结束（`Transfer-Encoding: chunked`）。折叠成一个带 mode flag 的方法会让每个调用方
都要考虑自己可能拿到哪种 body。unix 侧手写 chunked 解码（`mihomo/framing.rs`），HTTP 侧用
`reqwest` 的解码。**两个超时是独立的**：连接建立有界，流空闲无界——`/logs` 在空闲实例上
长时间不刷响应头，把空闲当超时会把「安静」误报为故障。

**脱敏在 Adapter 层完成**，用 `proxy_application::redaction`。它与既有两个 `redact` 语义不同：
错误信息里的 `redact` 替换整个 URL（读者只需知道「涉及了一个 URL」），而日志需要**保留 URL 结构**
（host、path 是诊断信息），只替换凭据值。三个调用方三个问题，是三个函数而非重复。
已真机验证：内核自身日志含 `token=SUPERSECRETVALUE`，经 Agent 流出的同一行变为 `token=<redacted>`，
URL 其余部分完整。

**「未采样」与「零」必须区分。** `/memory` 首帧 `{"inuse":0}` 是占位（尚未采样），被跳过；
`/traffic` 空闲时 `up:0` 是**真实增量**，被保留。两者形状相同、含义相反。

**观测不进状态机**（D1 的既有结论）：流断开不产生 `last_failure`、不触发回滚、不写审计。

### D8. `MihomoConnectionOps` 的实现形态（2026-09-12）

**实现状态：`KernelConnections` 已实现并接线**（`crates/infrastructure/src/mihomo/connections.rs`）。
原 `UnavailableConnections` 占位已删除。

**用快照，不用流。** `/connections` 支持 `?interval=` 推送，但「列出当前连接」是一个有答案的问题，
不是订阅：用流会让每个调用方读一帧就丢弃，并为一次已完成的操作保持连接。

**`close_connection` 返回 `CloseOutcome`，不返回 `()`。** 实测：`DELETE /connections/:id`
对**不存在的 id 也返回 204**。因此「已关闭」与「本来就不存在」**无法区分**，API 若声称知道就是在编造。
原 port 文档写的「unknown id → `InvalidResponse`」是**做不到的承诺**，已删除。
`close_all` 返回值改为实测的连接数（请求前后各读一次列表取差值），因为内核不报数，
而只有「已全部关闭」的审计记录价值极低。

**隐私按角色裁剪，用显式函数而非「字段没填」。** `ConnectionView::redact_for(role)` 是必须被调用的
转换：若只靠不填充字段来隐藏，将来新增 `uid` 消费点时不会有任何提示还有权限这回事。
ADMIN 见全部；其他角色丢失 `uid`/`process`/`processPath`——这三者合起来回答「本机哪个程序访问了什么」，
不是他们的权限。**不可配置**（角色决定，少一个旋钮少一种误配）。

**`close_all` 需要二次确认**（body 带 `{"confirm": true}`，CLI 侧 `--yes`）。
它是唯一「一个请求影响全部连接」的端点，误调的后果是中断所有用户的传输。

**空串一律归一化为 `None`。** 内核用空串表示「不适用」或「读不到」——`rule: ""` 是未命中规则，
`process: ""` 是读不到进程（实测：无 `CAP_NET_ADMIN` 时 `uid=0`、`process`/`processPath` 均为空）。
在边界归一化一次，消费方不必各自判断。

**`start` 是 RFC 3339 带偏移量，必须应用偏移。** 实测 `2026-09-12T21:40:03.325015696+08:00`；
忽略偏移会让每个时间戳静默偏移一个时区，而值看起来仍然合理——这是最难发现的一类错误。

**审计写入必须与读取对称。** `AuditAction::ConnectionClose` 与 `AuditTarget::Connection` 加入时，
写入路径加了而读取路径（`from_label` / `from_parts`）漏了，导致记录**写得进去、读不出来**，
整个审计列表报 500。已补，并加了双向 round-trip 测试覆盖**每个** action 与 target 变体。
连接 id 是内核生成的 UUID 而非凭据，可安全入库；target 的重建**不做格式校验**，
因为该格式不是本 Agent 定义的，不能因为内核改了格式就判定记录不可读。

---

## 3. Alternatives

| 方案 | 拒绝理由 |
|---|---|
| 用 `/restart` 做重启 | C5：`syscall.Exec` 自替换，绕过状态机、丢失 Agent 侧状态 |
| 用 `/upgrade` 做内核更新 | C6：错误语义、无签名校验、半截二进制风险 |
| 单一巨型 `MihomoPort`（~40 方法） | 违反 AGENTS.md"Port 小而聚焦"；观测/命令/连接失败语义不同 |
| SIGHUP 做 reload | 无错误反馈（R01/R03），失败不可观测 |
| Agent 不做 supervisor，交给 systemd 管 mihomo | 容器内无 systemd（C14）；且会引入双 supervisor/双 unit 复杂度（R09 C2/C3） |
| 复用 Mihomo 自带 iptables 自动化 | C16：全局规则污染 + 失败即退出 |

---

## 4. Consequences

### 4.1 正面

- 生命周期只有一个权威（可测试、可串行化、非法转换可拒绝）。
- 观测面断开不影响数据面状态判定。
- 内核更新与配置更新彻底解耦（REQ-MIHOMO-007）。
- 无 systemd 环境可降级运行（PVE LXC 常见）。

### 4.2 负面 / 成本

- Agent 需自研内核更新链路（下载/校验/替换/回滚），工作量转移到我方。
- 需要处理 socket 权限收紧时序（Mihomo 创建 socket 后 Agent 立即 chmod，存在短暂窗口）。
- L4 健康检查需要额外端口探测逻辑（C11 才不会被漏掉）。

### 4.3 必须遵守

```text
1. 禁止依赖 /restart 与 /upgrade（REQ-MIHOMO-007/009/011）
2. unix socket 必须收紧为 0660（REQ-SEC-002）
3. TCP controller 必须 secret 非空 + 仅 loopback（REQ-SEC-001/003）
4. reload 必须用 PUT /configs 带 JSON body；**禁止 force=true**；204 不代表生效
5. 生命周期操作按实例串行化；非法转换直接拒绝（REQ-MIHOMO-005）
6. 就绪判定必须基于 API 可达或可解析日志行，禁止 sleep（REQ-MIHOMO-011）
7. 健康检查必须含代理端口可达性，不得以"进程存活 + controller 200"判成功
```

---

## 5. Evidence

- `docs/research/01-mihomo.md` §1/§3–§8（route 清单、鉴权、reload 语义、流式接口、`/restart`/`/upgrade` 实测）
- `docs/research/03-mihomo-runtime.md`（信号语义、就绪耗时、unix socket、崩溃与端口）
- `docs/research/09-linux-runtime.md` C3/C5/C8（ambient cap、无 `sd_notify`、socket 权限边界、单 unit 决策）
- `docs/research/10-pve-lxc.md` §1 C1–C2（TUN 判定、容器内无 systemd）
- `docs/research/11-network-stack.md` C9（Mihomo legacy iptables 反面教材）
- `docs/research/12-security.md` §1（secret 空值/CORS 默认值/`":9090"` 全网卡）
