# ADR-001 — 总体架构：DDD-lite + Hexagonal Architecture + Modular Monolith

| 字段 | 值 |
|---|---|
| Status | Accepted（Phase 0 定稿） |
| Date | 2026-09-12 |
| Deciders | 项目架构负责人 |
| Supersedes | — |
| Related | ADR-002 ~ ADR-006、`AGENTS.md`、`docs/research/RESEARCH-SUMMARY.md` |

---

## 1. Context

### 1.1 产品形态

本项目是**面向 Linux Server / PVE LXC 的 Mihomo 管理 Agent**：

```text
Mihomo  = 数据平面（代理运行时）
Agent   = 控制平面（生命周期、配置、订阅、系统集成、API/CLI/TUI）
```

Agent 不重新实现代理内核、完整 Sub-Store、完整 Mihomo Dashboard。

### 1.2 约束条件

| 约束 | 来源 | 对架构的影响 |
|---|---|---|
| 外部依赖多且**必须可替换** | 订阅转换（Sub-Store / sub-store-convert / Native）、Dashboard（metacubexd）、init（systemd）、防火墙（nftables/iptables）均属外部系统 | 必须用 Port/Adapter 隔开，不能让外部 API 细节渗入业务逻辑 |
| 单机部署、单进程可用 | MVP 面向单台 Debian/Ubuntu 服务器或 PVE LXC | 不需要进程间 RPC、服务发现、消息中间件 |
| 失败必须安全降级 | 核心不变量：更新/转换/reload 失败不得破坏当前可用配置 | 需要明确的 Use Case 编排点，便于插入校验、激活、回滚与健康检查 |
| 运行环境能力不确定 | PVE LXC privileged/unprivileged 能力差异大 | 系统能力必须是**被检测的值对象**，而不是假设；需要独立的 System 领域与 Capability Port |
| 团队与生命周期 | 单人/小团队、MVP 优先可靠性 | 拒绝微服务、Event Sourcing、完整 CQRS |
| 既有约束 | `AGENTS.md` 已冻结依赖方向与领域划分 | 本 ADR 是对既有约束的正式记录，而非新决策 |

### 1.3 Phase 0 调研输入

- **R07（metacubexd）**：上游已从"纯 Dashboard"演进为 monorepo，包含 `packages/ui`、`apps/server`、`apps/desktop`，并在 `apps/desktop` 中依赖 `@metacubexd/agent` 完成 mihomo 内核下载（`kernel/fetch-kernel`）。结论：**上游正在覆盖"桌面端内核管理"，但不覆盖 Linux 服务端的编排、配置版本化与系统能力检测**——这正是本项目存在的理由（详见 ADR-006、`docs/research/07-metacubexd.md`）。
- **R08（ShellCrash）**：同类最成熟的 Linux 方案，但其能力以**脚本 + 菜单**形式组织，缺乏领域边界、配置版本化与原子回滚（详见 `docs/research/08-shellcrash.md`）。结论：可以做 feature-level 对标，但**不可复用其结构**。
- **R15（竞品）**：未发现同时覆盖"配置版本化 + 原子回滚 + PVE LXC 能力检测 + 统一 Use Case 的多前端（CLI/TUI/Web）"的服务端项目。

结论：不需要为差异化而改变架构风格；需要的是**把编排逻辑集中在一个可测试的应用层**。

---

## 2. Decision

采用 **DDD-lite + Hexagonal Architecture（Ports & Adapters）+ Modular Monolith**，以单个 Cargo workspace 交付。

### 2.1 Crate 划分与依赖方向

```text
crates/
├── domain/          纯业务模型与规则（无 async、无 IO、无框架）
├── application/     Use Case + Port 定义
├── infrastructure/  Port 的适配器实现
├── interfaces/      REST API / WebSocket / CLI / TUI 适配器
└── bootstrap/       唯一的组装点（wiring）
```

依赖方向（**不可逆**）：

```text
domain          ← application ← interfaces
                  ↑
            infrastructure
                  ↑
              bootstrap → 全部
```

允许：

```text
domain
application        -> domain
infrastructure     -> application, domain
interfaces         -> application, domain（只读类型）
bootstrap          -> 全部
```

禁止（`AGENTS.md` 已冻结，本 ADR 再次确认）：

```text
domain      -> application / infrastructure / interfaces
domain      -> tokio / reqwest / serde 实现 / 文件系统 / 进程 / 网络
application -> infrastructure / axum / reqwest / sqlx / systemd / nftables
```

### 2.2 关键决策点

| # | 决策 | 说明 |
|---|---|---|
| D1 | Domain 为**同步纯函数**库 | 不使用 `async`、不使用 `tokio`；领域不变量、值对象、领域错误在此。时间、随机数、ID 生成通过参数或 Port 注入，不在 Domain 内部取。 |
| D2 | Port 定义在 **application** | Port 是应用层对外的能力需求（`MihomoController`、`ProcessManager`、`SubscriptionConverter`、`ConfigRepository`、`CapabilityProbe`…），不是基础设施接口的镜像。 |
| D3 | Use Case 是**唯一业务入口** | CLI、TUI、Web API、WebSocket 全部调用同一组 Use Case，禁止在接口层写业务分支（对应 REQ-ARCH-001）。 |
| D4 | Port 小而聚焦 | 拒绝 `SystemManager` 式 40 方法巨型接口；按能力拆分（见 ADR-003、ADR-004）。 |
| D5 | 事件使用 `tokio::sync::broadcast` | 仅用于通知与协调（状态变化、日志、任务完成），**不做 Event Sourcing**。 |
| D6 | 持久化：SQLite 存元数据 + 文件系统存配置版本 | 生成后的完整 YAML 不写入 SQLite（见 ADR-004）。 |
| D7 | bootstrap 是唯一组装点 | `main` 只做参数解析 → 组装 → 启动 adapter；业务对象不在 `main` 里 new 出来到处传。 |
| D8 | 模块化单体优先 | 进程内模块边界靠 crate 依赖方向强制，而不是靠网络边界。 |

---

## 3. Alternatives

### 3.1 传统分层单体（Controller → Service → Repository）

- **描述**：`api/` → `service/` → `repository/`，业务逻辑写在 Service 中，直接依赖具体数据库/HTTP 客户端。
- **拒绝理由**：
  1. Service 直接依赖 `sqlx`/`reqwest`，订阅转换器与 Mihomo 集成无法在不改 Service 的情况下替换，违反核心目标"外部集成可替换"。
  2. 无 Compile-time 依赖方向约束，Domain 容易被数据库/框架类型污染。
  3. 难以对"失败保留旧配置"这类编排做纯 mock 测试。

### 3.2 微服务（agent / kernel-manager / subscription-service）

- **拒绝理由**：
  1. 部署目标是**单台服务器/PVE LXC**，多进程会引入服务发现、网络认证、跨服务事务（配置激活 + reload + 回滚）等纯成本。
  2. 配置文件与进程状态天然是本地资源，远程化后失败模式急剧增加。
  3. `AGENTS.md` 明确 MVP 不做微服务；调研也未发现需要独立扩展的组件。

### 3.3 纯库 + 薄 CLI（无常驻 Agent）

- **描述**：只提供 `proxyctl`，不做常驻服务、不做 Web/TUI 实时状态。
- **拒绝理由**：
  1. 需求包含 Web Admin、TUI 实时状态、WebSocket 事件、定时订阅更新——都需要常驻进程。
  2. 无法集中串行化生命周期操作（per-instance lock）与并发激活协调。
- **保留部分**：CLI 仍可独立运行（通过 Unix socket 连本机 Agent），这是 Interfaces 层的能力，不是架构退化。

### 3.4 Actor 模型 / supervisor 树

- **拒绝理由**：Mihomo 已是独立进程，Agent 侧只需要**串行化的命令处理 + 状态机**，Rust 生态中 actor 框架会引入不必要的运行时与迁移成本；用 per-instance `Mutex`/任务队列即可满足 `AGENTS.md` 的串行化要求。

### 3.5 直接复用 metacubexd agent 作为核心

- **拒绝理由**：
  1. metacubexd 的 `@metacubexd/agent` 面向**桌面/单机场景**（Electron），其目标平台与权限模型（Windows/macOS 桌面）与 Linux 服务端 + systemd + PVE LXC 不一致。
  2. 它不提供配置版本化、回滚策略、doctor 能力矩阵、审计与权限分离。
  3. 直接把它作为核心会让我们的架构跟随上游 TypeScript/Node 运行时，违背"Rust Agent + 最小依赖"。
- **保留部分**：作为**前端静态资源**集成其 Dashboard（ADR-006）。

---

## 4. Rationale

1. **可替换性**：Port 在 application、Adapter 在 infrastructure，使 Sub-Store → sub-store-convert → Native 的切换不改业务代码（REQ-SUB-001）。
2. **可测试性**：Domain 纯单元测试；Application 用 mock Port 覆盖订阅失败、激活失败、reload 失败、回滚成功/失败、非法状态转换、并发更新阻止（`AGENTS.md` Testing 章节）。
3. **失败安全**：编排集中在 Use Case，才有唯一位置实现"生成 → 校验 → 激活 → reload → 健康检查 → 失败回滚"。
4. **单机可运维**：单进程 + 单 SQLite + 文件版本目录，安装/备份/迁移路径简单。
5. **约束可执行**：依赖方向由 crate 边界 + CI（`cargo deny`/clippy）强制，不依赖开发者自觉。

---

## 5. Consequences

### 5.1 正面

- 业务规则与 Linux/Mihomo/HTTP/SQL 解耦，重构 adapter 不影响领域。
- CLI/TUI/Web 行为天然一致（同一 Use Case），减少三套实现漂移。
- 单元测试不需要真实 Mihomo/systemd/网络。

### 5.2 负面 / 成本

- 需要写额外映射代码（Domain ↔ DTO ↔ 存储行），小功能也要跨 4 层。
- 需要纪律：容易图省事在接口层直接调用 `reqwest`/`Command`；必须靠 review + clippy 规则守住。
- `async_trait` 在 Port 上有少量运行时开销（可接受）。

### 5.3 必须遵守的规则（违反即视为架构回归）

1. `crates/domain` 的 `Cargo.toml` 不允许出现 `tokio`、`reqwest`、`sqlx`、`axum`、`serde` 的实现依赖（只允许纯数据/错误类型的最小依赖）。
2. 任何 `std::process::Command`、`tokio::process`、`nft`/`systemctl`/文件写入只能出现在 `infrastructure`。
3. 新增 Port 必须回答"它是哪个 Use Case 需要的能力"，禁止按外部系统 1:1 建接口。
4. 事件只做通知；引入 Event Sourcing、CQRS 全套、外部消息代理需要新的 ADR。
5. 拆分 crate 或引入新分层需要新的 ADR。

### 5.4 后续动作

```text
Phase 0 完成（本 ADR + ADR-002~006）
      ↓
Domain Model（Aggregate / Entity / Value Object）
      ↓
Application Ports & Use Cases
      ↓
crates/ 骨架 + CI 质量门
```

---

## 6. Evidence

| 结论 | 证据 |
|---|---|
| 依赖方向与领域划分 | `AGENTS.md`（Non-Negotiable Architecture Rules、Repository Layout、Domain Boundaries） |
| Crate 划分与总体架构图 | `docs/Mihomo Linux Management Agent — 项目设计文档.md` §4–§6、§49–§50 |
| 配置不写入 SQLite 的原则 | 同上 §32–§34；`AGENTS.md` Persistence |
| 事件机制限 `broadcast`、禁止 Event Sourcing | `AGENTS.md` Events |
| 上游 Dashboard 现状与内核管理能力 | `docs/research/07-metacubexd.md`；`raw.githubusercontent.com/MetaCubeX/metacubexd/main/apps/desktop/package.json`（`@metacubexd/agent`、`fetch:mihomo`） |
| ShellCrash 结构不可复用 | `docs/research/08-shellcrash.md` |
| 竞品空白 | `docs/research/15-competitors.md` |
