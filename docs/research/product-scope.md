# Product Scope — Phase 0 冻结版

> 状态：v0.1（Phase 0 定稿，随 R01–R15 结论微调）
> 日期：2026-09-12
> 关联：`docs/research/requirements.md`、`docs/research/capability-matrix.md`、`docs/adr/ADR-001`~`ADR-006`

---

## 1. 我们解决什么问题

**一句话定位**

> 面向 Linux Server / PVE LXC 的 Mihomo 管理 Agent：把"Mihomo 能不能跑起来、配置改了能不能回滚、订阅挂了会不会断网、这个容器到底支持什么"这件事，做成**可检测、可回滚、可自动化、可远程管理**的工程能力。

**目标用户**

| 用户 | 场景 | 痛点 |
|---|---|---|
| PVE LXC 自建用户 | 在 PVE 上跑 Mihomo 做透明代理 | 不知道容器里 TUN/nftables/CAP 到底能不能用；改配置断网后无法回滚 |
| Debian/Ubuntu 服务器运维 | 用 Mihomo 做出站代理/网关 | 手动改 YAML、手动 reload、无版本管理、无审计 |
| 自动化使用者 | 用脚本/Ansible 管理多台机器 | 现有方案是交互式菜单脚本，难以自动化、无机器可读输出 |

**核心价值（竞品未覆盖或覆盖不足）**

```text
1. 配置版本化 + 原子回滚（失败保留旧配置）      ← R15 确认：无人提供 list/show/diff/activate/rollback
2. PVE LXC 能力检测与显式降级（五值能力状态）    ← R15 确认：无人做容器能力分层
3. Doctor：把"能不能用"变成可验证报告
4. 统一 Use Case：CLI / TUI / Web 行为一致
5. 可替换的订阅转换器（Sub-Store / Native）
6. 最小权限与审计（特权操作映射到显式 Use Case）
7. 内核更新与配置更新彻底分离
```

**竞品现状（R15 实测修正）**

> ⚠️ "Linux 服务器端 Mihomo 管理器"**不是空白市场**。已确认存在直接竞品：`mihari`（Go，daemon + CLI/TUI/Web）、`mihomo-tui`（Go，明确面向 headless Linux server）、`Proxy-RS`（Rust/Ratatui）、`clashtui`（Rust，670 star）、`flclash-tui`（Dart）。其中 `flclash-tui` 已宣称 *"validates, writes atomically, reloads, rolls back on failure"*，`mihari` 有 *validated atomic config generation with rollback*。

因此差异化**不能**建立在"我们做配置原子写与回滚"这一单点上，而必须落在：

```text
① 不可变配置版本库（多版本历史 + list/show/diff/activate <id>/rollback <id>）
② PVE LXC / 容器能力的五值分层检测与降级语义
③ 统一 Doctor（跨系统/Mihomo/网络/运行时）
④ 审计与最小权限
⑤ 统一 Use Case（三端同源）与可替换转换器
```

---

## 2. 范围划分

```text
                 Product Scope
                      │
          ┌───────────┼───────────┐
          ▼           ▼           ▼
        MUST        SHOULD       WON'T
```

### 2.1 MUST（MVP 必须交付）

| 能力 | 说明 | 关键需求 |
|---|---|---|
| Mihomo 生命周期管理 | start / stop / restart / reload / status / logs，崩溃检测，串行化 | REQ-MIHOMO-001~006 |
| Mihomo 版本管理与更新 | 查询版本、受控更新、校验、（可选）回滚；与配置更新分离 | REQ-MIHOMO-006~009 |
| 配置版本化 | 不可变版本 + checksum + 来源追溯 | REQ-CONFIG-001/007/008 |
| 配置校验 | YAML 语法 → Mihomo 语义（`-t`）→ 启动/健康检查 | REQ-CONFIG-005 |
| 配置原子激活与回滚 | temp+rename、激活/reload/健康检查失败自动回滚 | REQ-CONFIG-003/006 |
| 订阅管理 | CRUD、手动/定时更新、失败保留旧配置、并发抑制 | REQ-SUB-002~004/007/008 |
| 订阅转换 Adapter | 只依赖 `SubscriptionConverter` Port，Sub-Store 为默认外部实现；**sub-store-convert 判定 Rejected** | REQ-SUB-001/006/011~013/015 |
| Web API | `/api/v1/*` + `/ws/v1/events`，DTO 化，版本化 | REQ-API-001~004 |
| Web Admin | Overview / Mihomo / Subscriptions / Configs / System / Settings / Logs | REQ-UI-002 |
| Mihomo Dashboard 集成 | **内嵌 metacubexd 静态产物**（同源 Clash API 反代），不修改其代码，不启用其 agent/all-in-one 形态 | REQ-UI-001/002、ADR-006 |
| CLI `proxyctl` | 覆盖全部核心操作，`--json`，退出码语义化 | REQ-CLI-001~003 |
| TUI | overview / status / groups / logs / subscription / rollback / doctor | REQ-TUI-001/002 |
| systemd 集成 | 单 unit、静态服务用户 `proxy-agent`、capability 分档、沙箱 | REQ-SEC-009、ADR-005 |
| PVE LXC 环境检测 | `ioctl(TUNSETIFF)` 级判定、CAP_NET_ADMIN、容器类型、五值状态 + `Misconfigured` 判定矩阵 | REQ-LXC-001~004/009/010 |
| System Doctor | 系统/Mihomo/网络/运行时能力报告，`--json`，31+ 探测项 | REQ-LXC-006~008 |
| 安全基线 | controller 不暴露、强制 secret、收窄 CORS、socket 权限收紧、Web 认证、日志脱敏、审计 | REQ-SEC-001~013 |

### 2.2 SHOULD（MVP 内尽量，可延期到 v0.2）

| 能力 | 判定依据 |
|---|---|
| TUN 启用（可选开关 + 检测失败降级） | REQ-NET-002；不可用不得影响 basic proxy |
| nftables / iptables **检测** | REQ-NET-007；检测是 MUST 的一部分，apply 不是 |
| 防火墙规则 apply（带 dry-run + watchdog 回滚） | REQ-NET-004/005；风险高，默认关闭 |
| 多实例 Mihomo | 领域模型预留实例 ID，但 MVP 只保证单实例可用 |
| OpenRC / 非 systemd | REQ-OPS-008；仅保留 `InitSystem` 抽象 |
| Backup / Restore | REQ-OPS-006；最小实现（导出/导入配置版本 + DB） |
| API Token 与角色（ADMIN / READ_ONLY） | REQ-SEC-004；MVP 至少单管理员 |
| Remote Management | 通过反代 + 认证访问单机 Agent，不做多节点控制平面 |

### 2.3 WON'T（MVP 明确不做）

| 不做 | 原因 |
|---|---|
| 自研代理核心 | Mihomo 已是数据平面；重写无收益 |
| 自研完整 Sub-Store / 完整订阅转换器 | 上游成熟；Native Converter MVP 仅保留接口（`NotImplemented`） |
| 自研完整 Mihomo Dashboard | metacubexd 已是官方 Dashboard；重做是纯浪费 |
| 修改 metacubexd 核心代码 | 跟随上游升级的前提 |
| 微服务 / Kubernetes Operator | 单机部署无收益，运维成本高 |
| Event Sourcing / 完整 CQRS | 无审计回放需求；事件仅用于通知（ADR-001） |
| 多租户 SaaS / 多节点远程控制平面 | 超出 MVP 定位 |
| 自研 OpenWrt 路由器方案 | 与 ShellCrash/OpenClash 正面竞争且场景不同 |
| 桌面客户端 | 与 Mihomo Party / Clash Verge 场景不同；metacubexd 已有 desktop |
| 一键脚本式"静默改系统网络" | 与"能力检测 + 显式授权"原则冲突 |

---

## 3. 明确不解决的问题（非目标）

```text
✗ 不做通用 Linux 运维面板
✗ 不做节点测速/CDN 优选等代理运营功能
✗ 不做机场管理后台 / 用户计费
✗ 不做跨机器的统一控制平面（Phase 0 只做单机 Agent + 远程访问）
✗ 不保证所有发行版 / 所有 init system 兼容（MVP: Debian/Ubuntu + systemd）
✗ 不保证 LXC 下 TUN 一定可用（只保证"检测出来并降级"）
```

---

## 4. 降级模型（产品级承诺）

产品必须能表达以下**合法状态**，而不是"要么全好要么坏"：

| 环境 | 合法产品状态 |
|---|---|
| unprivileged LXC，无 TUN | HTTP/SOCKS/Mixed 可用；TUN = Unavailable；Transparent = Unavailable |
| 有 `/dev/net/tun` 但打不开 | TUN = Misconfigured（给出具体原因与修复建议） |
| 无 nftables 有 iptables | Transparent 走 iptables 或标记为 Detection Only |
| 没有 systemd（容器内） | 进程由 Agent 直接管理（direct process 模式） |
| 未部署 Sub-Store | 订阅转换不可用，但已有配置继续运行；更新订阅报明确错误 |
| 订阅源不可达 | 当前激活配置保持不变；失败原因可见、可重试 |

**核心不变量**

> 失败的更新、转换、reload、能力探测，只能导致"能力降级"，绝不能导致"当前可用配置被破坏"或"Mihomo 被停机"。

---

## 5. 与竞品的能力边界

| 能力 | 谁覆盖 | 我们的定位 |
|---|---|---|
| Dashboard / 代理组切换 / 连接查看 | metacubexd（官方） | **集成**，不自研 |
| 订阅转换 / 节点操作 | Sub-Store | **Adapter 调用**，不自研 |
| Linux 侧一键安装 + 规则生成 | ShellCrash | 概念对标（Reuse/Improve），不复制实现 |
| 桌面端内核下载与管理 | metacubexd desktop、Mihomo Party 等 | 不做（场景不同） |
| 配置版本化 + 原子回滚 | 无成熟方案 | **自研（核心价值）** |
| PVE LXC 能力检测 + 五值状态 + Doctor | 部分脚本有零散检测 | **自研（核心价值）** |
| 统一 Use Case 的 CLI/TUI/Web | 无 | **自研（核心价值）** |
| 可替换订阅转换器 | 无（多为硬编码 subconverter/Sub-Store） | **自研（核心价值）** |
| 最小权限 + 审计 | 无 | **自研（核心价值）** |

---

## 6. 版本节奏（Phase 0 后的范围演进）

```text
v0.1 (MVP)   MUST 全部 + SHOULD 中低风险项（TUN 可选、检测类能力、最小备份）
v0.2         nftables/TProxy apply（dry-run + watchdog）、API Token/角色、OpenRC
v0.3         Native Converter 基础能力、多实例管理
v0.4         远程多节点（需求出现才做）
```

---

## 7. 冻结声明

本文件在 Phase 0 结束时冻结以下内容：

- MUST 列表即 MVP 的验收范围。
- WON'T 列表即 MVP 期间**不得**被当作"顺手实现"的扩展（引入需要新 ADR）。
- 降级模型是产品级承诺，任何实现不得把降级状态变成致命错误。
