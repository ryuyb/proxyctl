# Phase 0：Architecture Discovery

## 现状调研清单与产出物

> 状态：可开工
>
> 项目方向：Linux / PVE LXC Mihomo Management Agent
>
> 架构目标：DDD-lite + Hexagonal Architecture + Modular Monolith
>
> Phase 0 目标：在正式编写 Domain Model 和 Application Port 之前，确认现有生态能力、依赖边界、运行环境约束与产品范围。

---

## 1. Phase 0 的目标

Phase 0 不是泛泛的“市场调研”，而是一次 **Architecture Discovery**。

最终需要回答四个问题：

1. 现有项目已经解决了什么问题？
2. 哪些能力应该直接复用，而不是重新实现？
3. 哪些能力必须由我们的 Agent 自己负责？
4. 在这些结论基础上，Domain、Application Port 和 Infrastructure Adapter 应该如何划分？

最终流程：

```text
Phase 0 Research
       ↓
Capability Matrix
       ↓
Product Scope
       ↓
Architecture Decisions
       ↓
Domain Model
       ↓
Application Ports
       ↓
开始 Rust 实现
```

---

# 2. 调研任务清单

| 编号 | 调研项 | 核心问题 | 产出物 | 完成标准 |
|---|---|---|---|---|
| R01 | Mihomo 能力 | 哪些能力已有 API？哪些必须 Agent 自己实现？ | `01-mihomo.md` | API/能力矩阵完成 |
| R02 | Mihomo 配置 | 配置校验、Reload、Profile、版本兼容怎么做？ | `02-mihomo-config.md` | Config Lifecycle 明确 |
| R03 | Mihomo Runtime | 进程、日志、Controller、Unix Socket 如何管理？ | `03-mihomo-runtime.md` | Process/Controller Port 明确 |
| R04 | Sub-Store API | 哪些 API 是公开行为，哪些属于内部实现？ | `04-sub-store.md` | Adapter 策略明确 |
| R05 | Sub-Store 部署 | Node/Bun/Docker/Server 怎么部署？资源需求如何？ | `05-sub-store-deployment.md` | 默认部署方案明确 |
| R06 | sub-store-convert | 当前维护状态、功能覆盖、与 Sub-Store 的关系 | `06-sub-store-convert.md` | 是否纳入明确 |
| R07 | metacubexd | Agent、Server、Dashboard 已经做到什么？ | `07-metacubexd.md` | 明确不重复实现的能力 |
| R08 | ShellCrash | 它解决了哪些 Linux/PVE 问题？ | `08-shellcrash.md` | Feature Reverse Engineering 完成 |
| R09 | Linux Runtime | systemd/OpenRC/process/network 权限如何处理？ | `09-linux-runtime.md` | Runtime Adapter 设计明确 |
| R10 | PVE LXC | privileged/unprivileged/TUN/nftables 哪些能用？ | `10-pve-lxc.md` | 能力矩阵 + 实测结论 |
| R11 | nftables/iptables | 透明代理具体依赖什么？ | `11-network-stack.md` | MVP 网络范围明确 |
| R12 | Security | API、Unix Socket、root、CAP、Secret 如何隔离？ | `12-security.md` | Threat Model + 安全边界 |
| R13 | License | Mihomo/Sub-Store/metacubexd 等如何组合？ | `13-licenses.md` | License Matrix 完成 |
| R14 | Deployment | 单 binary、外部服务、容器如何组合？ | `14-deployment-model.md` | 默认部署模型明确 |
| R15 | Competitors | 还有哪些类似项目？差异在哪里？ | `15-competitors.md` | Feature Matrix 完成 |

---

# 3. 最重要的五项产出

## 3.1 Capability Matrix

文件：

```text
`docs/research/capability-matrix.md`
```

目标：用一张表回答“谁已经会什么、我们的项目应该负责什么”。

示例：

| Capability | Mihomo | Sub-Store | metacubexd | ShellCrash | Our Agent |
|---|---|---|---|---|---|
| Process Management | 部分 |  | 部分 | ✓ | ✓ |
| Config Lifecycle | ✓ | 部分 | 部分 | ✓ | ✓ |
| Config Versioning |  |  |  | 部分 | ✓ |
| Config Rollback |  |  |  | 部分 | ✓ |
| Subscription Management |  | ✓ |  | ✓ | ✓ |
| Subscription Conversion | 部分 | ✓ |  | ✓ | Adapter |
| Dashboard | API |  | ✓ | ✓ | Integrate |
| CLI |  |  |  | ✓ | ✓ |
| TUI |  |  |  |  | ✓ |
| Doctor |  |  |  | 部分 | ✓ |
| LXC Detection |  |  |  | ✓ | ✓ |
| TUN | ✓ |  | 部分 | ✓ | Manage |
| nftables |  |  |  | ✓ | Manage |
| systemd |  |  |  | ✓ | ✓ |

最终表必须基于实际调研结果修订，不能把上表示例直接当作最终结论。

---

## 3.2 Architecture Decision Record

至少建立：

```text
`docs/adr/ADR-001-architecture.md`
`docs/adr/ADR-002-subscription-provider.md`
`docs/adr/ADR-003-mihomo-integration.md`
`docs/adr/ADR-004-config-lifecycle.md`
`docs/adr/ADR-005-security-model.md`
`docs/adr/ADR-006-deployment-model.md`
```

每份 ADR 至少包含：

```text
Context
Decision
Alternatives
Rationale
Consequences
Status
```

---

## 3.3 Product Scope

文件：

```text
`docs/research/product-scope.md`
```

划分：

```text
                 Product Scope
                      │
          ┌───────────┼───────────┐
          ▼           ▼           ▼
         MUST        SHOULD       WON'T
```

### MUST

- Mihomo 生命周期管理
- Config Versioning
- Config Rollback
- Subscription 管理
- Subscription Conversion Adapter
- Web API
- Web Admin
- CLI
- TUI
- systemd
- PVE LXC 环境检测
- System Doctor
- Mihomo Dashboard 集成

### SHOULD

- TUN
- nftables
- iptables
- 多实例 Mihomo
- OpenRC
- Backup / Restore
- API Token
- Remote Management

### WON'T（MVP）

- 自研代理核心
- 自研完整 Sub-Store
- 自研完整 Mihomo Dashboard
- 微服务
- Kubernetes Operator
- Event Sourcing
- 完整 CQRS

---

## 3.4 Requirements

文件：

```text
`docs/research/requirements.md`
```

需求必须可验证，例如：

```text
REQ-MIHOMO-001
Agent MUST start Mihomo.

REQ-MIHOMO-002
Agent MUST detect Mihomo process failure.

REQ-CONFIG-001
Every activated config MUST have a version.

REQ-CONFIG-002
A failed subscription update MUST NOT replace the active config.

REQ-SUB-001
Subscription conversion MUST be replaceable by another provider.

REQ-LXC-001
Agent MUST detect whether /dev/net/tun is available.

REQ-UI-001
CLI/TUI/Web MUST use the same Application Use Case.
```

这些 Requirement 后续可以直接映射到测试用例。

---

## 3.5 Open Questions

文件：

```text
`docs/research/open-questions.md`
```

用于记录尚未确定的问题，不允许把未经验证的假设当成架构事实。

示例：

```text
Q001
Mihomo Config 是否存在适合机器校验的稳定 Schema？

Q002
Sub-Store /download/sub 是否存在长期兼容保证？

Q003
metacubexd agent 是否值得直接复用？

Q004
PVE unprivileged LXC + TUN 的最小权限组合是什么？

Q005
Mihomo binary update 是否应该完全由 Agent 管理？

Q006
MVP 是否直接实现 nftables apply，还是只做 detection？
```

---

# 4. 调研方向详细清单

## R01 — Mihomo 能力

### 研究内容

- Controller API
- Unix Socket
- Proxy API
- Proxy Group API
- Connection API
- Traffic API
- Log API
- Config/Profile API
- Reload
- Runtime 信息
- TUN
- DNS
- Rule
- Health Check

### 要回答

```text
哪些能力 Mihomo 已经提供？
哪些能力 Agent 不应该重复实现？
哪些能力 API 可以稳定封装？
```

### 直接影响

```text
MihomoController Port
MihomoProcess Port
Config Port
HealthCheck Port
```

---

## R02 — Mihomo Config

### 研究内容

- YAML Schema
- Config validation
- Reload 行为
- Config path
- Profile
- Cache
- 运行时配置修改
- 版本兼容
- 启动失败行为

### 最终确定

```text
Generated Config
      ↓
Syntax Validation
      ↓
Semantic Validation
      ↓
Temporary Start
      ↓
Health Check
      ↓
Activate
```

---

## R03 — Mihomo Runtime

研究：

- Process lifecycle
- stdout/stderr
- exit code
- graceful shutdown
- Controller startup timing
- Unix Socket
- HTTP Controller
- PID 管理
- crash detection
- restart policy

最终确定：

```text
ProcessManager
MihomoController
HealthChecker
```

是否分成三个 Port。

---

## R04 — Sub-Store API

这是 Phase 0 的重点之一。

### 必须研究

- `/download/sub`
- 参数
- `target`
- `url`
- `content`
- `ua`
- `proxy`
- `mergeSources`
- 其他公开参数
- 错误响应
- 重定向
- 超时
- 缓存
- 认证方式
- API 版本稳定性

### 核心问题

```text
哪些是公开可依赖的行为？
哪些是内部 API？
```

### 目标

不能让 Application 依赖 Sub-Store 的 JSON 内部结构。

Application 只认识：

```rust
trait SubscriptionConverter
```

---

## R05 — Sub-Store Deployment

研究：

- Node
- Bun
- Docker
- Server mode
- Resource consumption
- Persistence
- Update strategy
- Configuration
- Network dependency

最终回答：

```text
MVP 默认：
官方 Sub-Store 外部服务

或：
sub-store-convert

或：
两者均可选
```

---

## R06 — sub-store-convert

研究：

- repository activity
- release frequency
- maintainer activity
- issue/PR activity
- vendor update mechanism
- supported targets
- API compatibility
- license
- dependency chain

最终结论需要是明确的：

```text
Primary
Secondary
Optional
Rejected
```

不能只写“看起来维护不错”。

---

## R07 — metacubexd

这是当前最值得重新评估的一项。

研究：

```text
packages/ui
packages/agent
apps/server
apps/desktop
```

重点关注：

- Dashboard
- Agent
- Mihomo kernel 管理
- API proxy
- all-in-one server
- 部署模式
- 是否与我们的目标重叠

核心问题：

> 我们和 metacubexd 的不可替代价值是什么？

预期答案应更多集中在：

```text
Linux-native orchestration
PVE LXC
Config lifecycle
Subscription lifecycle
Doctor
TUI
CLI
systemd
nftables
Rollback
```

而不是重新做 Dashboard。

---

## R08 — ShellCrash

不要直接学习 ShellCrash 代码结构，而是做 Feature Reverse Engineering。

研究：

- 支持平台
- 安装逻辑
- Init system
- Mihomo 生命周期
- Kernel 更新
- Subscription
- Config
- TUN
- TProxy
- iptables
- nftables
- Docker
- PVE
- 故障恢复

产出一张：

```text
ShellCrash Feature Matrix
```

明确哪些能力：

```text
Reuse
Improve
Ignore
```

---

## R09 — Linux Runtime

研究：

```text
systemd
OpenRC
direct process
capabilities
filesystem permissions
service user
signals
restart policy
```

MVP 默认：

```text
Debian / Ubuntu
+
systemd
```

OpenRC 后续加入。

---

## R10 — PVE LXC

必须进行资料研究 + 实机测试。

环境至少考虑：

```text
Privileged LXC
Unprivileged LXC
VM
Bare Metal
```

检查：

```text
/dev/net/tun
CAP_NET_ADMIN
iptables
nftables
routing
DNS
TUN
TProxy
```

最终产出：

```text
PVE LXC Capability Matrix
```

---

## R11 — Network Stack

研究：

```text
iptables
nftables
TUN
TProxy
redirect
route
policy routing
DNS interception
```

重点不是“所有功能都支持”，而是确定：

```text
MVP Supported
MVP Detection Only
Later
Unsupported
```

---

## R12 — Security

研究：

- Mihomo Controller exposure
- Secret
- CORS
- Unix Socket permissions
- Agent Unix Socket
- API Authentication
- Root privileges
- CAP_NET_ADMIN
- systemd sandboxing
- Web security
- Subscription URL secrets
- Logs 中的敏感数据

最终需要形成：

```text
Threat Model
Trust Boundary
Permission Model
```

---

## R13 — License

至少检查：

```text
Mihomo
Sub-Store
sub-store-convert
metacubexd
前端依赖
Rust dependencies
```

最终形成：

```text
License Matrix
```

并明确：

```text
Bundled
External Service
Dynamic Dependency
Source Reuse
```

特别注意：Sub-Store 当前仓库标注为 AGPL-3.0，因此第一阶段建议将其保持为独立外部组件，而不是直接把源码并入 Rust 核心。

---

## R14 — Deployment Model

比较：

### Model A

```text
Rust Agent
+
Mihomo
+
metacubexd
```

### Model B

```text
Rust Agent
+
Mihomo
+
metacubexd
+
Sub-Store
```

### Model C

```text
Rust Agent
+
Mihomo
+
Native Converter
+
metacubexd
```

### Model D

```text
Rust Agent
+
Mihomo
+
Optional Converter
+
metacubexd
```

最终选一个默认模型，并说明为什么。

---

## R15 — Competitors

至少比较：

```text
ShellCrash
metacubexd
Sub-Store
sub-store-convert
其他 Linux proxy manager
其他 Rust TUI manager
```

比较：

- Installation
- Update
- Config
- Subscription
- Dashboard
- CLI
- TUI
- LXC
- TUN
- nftables
- Rollback
- Doctor
- Security

---

# 5. Phase 0 文件结构

建议最终仓库形成：

```text
 docs/
 ├── architecture.md
 │
 ├── research/
 │   ├── 01-mihomo.md
 │   ├── 02-mihomo-config.md
 │   ├── 03-mihomo-runtime.md
 │   ├── 04-sub-store.md
 │   ├── 05-sub-store-deployment.md
 │   ├── 06-sub-store-convert.md
 │   ├── 07-metacubexd.md
 │   ├── 08-shellcrash.md
 │   ├── 09-linux-runtime.md
 │   ├── 10-pve-lxc.md
 │   ├── 11-network-stack.md
 │   ├── 12-security.md
 │   ├── 13-licenses.md
 │   ├── 14-deployment-model.md
 │   ├── 15-competitors.md
 │   ├── capability-matrix.md
 │   ├── product-scope.md
 │   ├── requirements.md
 │   ├── open-questions.md
 │   └── RESEARCH-SUMMARY.md
 │
 └── adr/
     ├── ADR-001-architecture.md
     ├── ADR-002-subscription-provider.md
     ├── ADR-003-mihomo-integration.md
     ├── ADR-004-config-lifecycle.md
     ├── ADR-005-security-model.md
     └── ADR-006-deployment-model.md
```

---

# 6. Phase 0 Definition of Done

## Research

```text
[ ] Mihomo API inventory
[ ] Mihomo Config lifecycle
[ ] Mihomo Runtime strategy

[ ] Sub-Store public API inventory
[ ] Sub-Store deployment strategy
[ ] sub-store-convert maintenance evaluation

[ ] metacubexd capability analysis
[ ] ShellCrash capability analysis

[ ] Linux runtime strategy
[ ] PVE LXC capability matrix
[ ] TUN test
[ ] nftables test
[ ] iptables test

[ ] Security threat model
[ ] License matrix
[ ] Deployment model
[ ] Competitor feature matrix
```

## Product

```text
[ ] Capability Matrix
[ ] Product Scope
[ ] Requirements
[ ] Open Questions
```

## Architecture

```text
[ ] ADR-001 Architecture
[ ] ADR-002 Subscription
[ ] ADR-003 Mihomo
[ ] ADR-004 Config Lifecycle
[ ] ADR-005 Security
[ ] ADR-006 Deployment
```

## Summary

```text
[ ] RESEARCH-SUMMARY.md
```

---

# 7. Phase 0 完成后的决策流程

```text
                 Research
                    │
                    ▼
          Capability Matrix
                    │
                    ▼
            Product Scope
                    │
        ┌───────────┴───────────┐
        ▼                       ▼
  Build / Reuse             Defer / Reject
        │                       │
        └───────────┬───────────┘
                    ▼
                 ADRs
                    │
                    ▼
              Domain Model
                    │
                    ▼
            Application Ports
                    │
                    ▼
              Rust Workspace
```

---

# 8. 推荐执行顺序

优先级不是简单按照编号顺序，而是按照对架构影响排序：

```text
P0
R07  metacubexd
R04  Sub-Store API
R01  Mihomo API
R02  Mihomo Config
R10  PVE LXC

P1
R06  sub-store-convert
R03  Mihomo Runtime
R09  Linux Runtime
R11  Network Stack
R12  Security

P2
R08  ShellCrash
R13  License
R14  Deployment
R15  Competitors
R05  Sub-Store Deployment
```

原因：

首先确认我们是不是在重复实现已有能力；然后确认最核心的外部接口；再确认 PVE/Linux 的现实约束；最后完善部署、竞品和许可证细节。

---

# 9. Phase 0 最终必须回答的问题

Phase 0 结束时，团队应该能够不用“我觉得”，直接回答：

### 产品

```text
我们到底解决什么问题？
我们的核心价值是什么？
我们不解决什么问题？
```

### Mihomo

```text
Mihomo 哪些能力直接调用？
哪些能力由 Agent 封装？
哪些能力由 Linux Adapter 提供？
```

### Subscription

```text
Sub-Store 是否作为外部组件？
是否依赖 /download/sub？
sub-store-convert 是否作为备用实现？
Native Converter 是否进入 MVP？
```

### Dashboard

```text
metacubexd 复用到什么程度？
我们自己的 Web UI 只做什么？
```

### Runtime

```text
systemd 是否为 MVP 唯一 init？
PVE LXC 最小支持范围是什么？
TUN / nftables MVP 做到什么程度？
```

### Security

```text
谁可以操作 Mihomo？
谁可以修改 nftables？
谁可以更新 binary？
Web API 如何鉴权？
Unix Socket 如何保护？
```

### Architecture

```text
Domain 有哪些 Aggregate / Entity / Value Object？
Application 有哪些 Use Case？
有哪些 Port？
每个 Port 的 Adapter 是什么？
```

如果以上问题还存在重大未知项，就不应该冻结 Domain Model。

---

# 10. Phase 0 结束标志

满足以下条件后，Phase 0 才算结束：

```text
Research conclusions are evidence-based
        ↓
Major unknowns are documented
        ↓
Product scope is frozen for MVP
        ↓
External dependency boundaries are frozen
        ↓
ADRs are accepted
        ↓
Domain Model can be designed
        ↓
Start implementation
```

最终唯一的开发入口文件：

```text
`docs/research/RESEARCH-SUMMARY.md`
```

开发阶段优先阅读：

```text
RESEARCH-SUMMARY.md
        ↓
ADRs
        ↓
requirements.md
```

其余调研文档作为依据和追溯资料。
