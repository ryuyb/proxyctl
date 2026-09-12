# Mihomo Linux Management Agent
## 项目设计与开发规格 v0.1

> 状态：可开工  
> 架构：DDD-lite + Hexagonal Architecture + Modular Monolith  
> 目标平台：Linux Server / PVE LXC  
> 核心语言：Rust  
> UI：Web + TUI  
> CLI：`proxyctl`  
> 核心代理：Mihomo  
> 订阅转换：优先集成官方 Sub-Store，可插拔支持其他 Converter

---

# 1. 项目定位

本项目不是 ShellCrash 的 Rust 重写，而是一个：

> **面向 Linux Server / PVE LXC 的 Mihomo 管理 Agent。**

核心职责是：

```text
┌─────────────────────────────────────────────┐
│             Mihomo Management Agent         │
├─────────────────────────────────────────────┤
│ Mihomo 生命周期管理                         │
│ 配置版本管理                                 │
│ 订阅管理与转换                               │
│ 自动更新与调度                               │
│ 系统环境检测                                 │
│ TUN / nftables 等系统能力管理                │
│ Web API                                      │
│ CLI                                          │
│ TUI                                          │
└─────────────────────────────────────────────┘
                       │
                       ▼
                    Mihomo
```

本项目本身不重新实现代理内核，不重新实现整个 Sub-Store，不重新实现 Dashboard。

---

# 2. 产品目标

## 2.1 MVP 必须完成

### Mihomo

- 下载指定版本 Mihomo
- 检查当前版本
- 启动
- 停止
- 重启
- Reload
- Health Check
- 日志查看
- 版本更新
- 版本回滚

### Configuration

- 配置文件管理
- 配置语义校验
- 配置版本化
- 当前 Active Config
- 配置历史
- 配置 Diff
- Rollback
- 原子切换
- Reload Mihomo

### Subscription

- 添加订阅
- 删除订阅
- 更新订阅
- 多订阅
- User-Agent
- Converter 配置
- 定时更新
- 更新结果记录
- 失败保留旧配置
- 转换结果生成 Mihomo Config

Sub-Store 已支持多种客户端目标格式、过滤、节点操作等能力，因此第一阶段只把它视为 Converter/Subscription Backend，而不重复实现这些能力。

### Web

- Dashboard
- Mihomo 状态
- 系统状态
- Subscription 管理
- Config 管理
- Update
- Logs
- System Doctor
- Settings

Mihomo 详细代理管理页面直接集成官方 `metacubexd`。官方项目当前支持独立静态面板、独立容器以及与 Mihomo 集成的部署方式。

### CLI

```bash
proxyctl start
proxyctl stop
proxyctl restart
proxyctl reload

proxyctl status
proxyctl logs
proxyctl logs -f

proxyctl mihomo version
proxyctl mihomo update

proxyctl subscription list
proxyctl subscription update
proxyctl subscription update <id>

proxyctl config list
proxyctl config show
proxyctl config validate
proxyctl config rollback <version>

proxyctl doctor

proxyctl
```

没有参数执行：

```bash
proxyctl
```

进入 TUI。

---

# 3. 非目标

MVP 不做：

- 多节点远程管理平台
- 多租户 SaaS
- Kubernetes Operator
- 自研代理协议解析器
- 自研完整订阅转换器
- 完整替代 Sub-Store
- 自研 Mihomo Dashboard
- Event Sourcing
- CQRS 全套实现
- 微服务拆分

原则：

> 先做一个可靠的 Modular Monolith，再考虑拆分。

---

# 4. 总体架构

```text
                         ┌───────────────────┐
                         │      Web UI       │
                         └─────────┬─────────┘
                                   │
                         ┌─────────▼─────────┐
                         │      Web API      │
                         └─────────┬─────────┘
                                   │
                  ┌────────────────┼────────────────┐
                  │                │                │
                  ▼                ▼                ▼
                CLI               TUI          Future Client
                  │                │
                  └────────────────┼────────────────┘
                                   │
                         Application Layer
                                   │
                         ┌─────────▼─────────┐
                         │      Domain       │
                         └─────────┬─────────┘
                                   │
                         Ports / Interfaces
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                    │
              ▼                    ▼                    ▼
        Mihomo Adapter       Subscription Adapter   System Adapter
              │                    │                    │
              ▼                    ▼                    ▼
           Mihomo             Sub-Store             Linux
                                                     systemd
                                                     nftables
                                                     LXC
```

依赖方向：

```text
Interfaces
    ↓
Application
    ↓
Domain

Infrastructure
    ↓
Application Ports
```

严格禁止：

```text
Domain → Infrastructure
Domain → Axum
Domain → Tokio
Domain → Reqwest
Domain → SQLite
Domain → systemd
```

---

# 5. 架构原则

## 5.1 Domain Pure

Domain 不包含：

- HTTP
- SQL
- 文件系统
- Process
- systemd
- Linux command
- Mihomo HTTP API

Domain 只描述业务概念和业务规则。

---

## 5.2 Application 负责 Use Case

Application 负责：

```text
获取实体
    ↓
检查业务规则
    ↓
调用 Port
    ↓
保存结果
    ↓
返回结果
```

例如：

```text
UpdateSubscriptionUseCase

Subscription
      ↓
获取远程订阅
      ↓
Converter
      ↓
Generated Config
      ↓
Validate
      ↓
Create ConfigVersion
      ↓
Activate
      ↓
Reload Mihomo
```

---

## 5.3 Infrastructure 负责脏活

Infrastructure 实现：

- Mihomo HTTP Client
- Mihomo Unix Socket Client
- Process Supervisor
- File System
- SQLite
- Sub-Store Adapter
- Scheduler
- systemd
- nftables
- LXC 检测

---

# 6. Workspace 目录结构

推荐直接使用 Cargo Workspace。

```text
proxy-manager/
│
├── Cargo.toml
│
├── crates/
│   │
│   ├── domain/
│   │   └── src/
│   │       ├── lib.rs
│   │       │
│   │       ├── mihomo/
│   │       │   ├── mod.rs
│   │       │   ├── entity.rs
│   │       │   ├── value_objects.rs
│   │       │   └── error.rs
│   │       │
│   │       ├── subscription/
│   │       │   ├── mod.rs
│   │       │   ├── entity.rs
│   │       │   ├── value_objects.rs
│   │       │   └── error.rs
│   │       │
│   │       ├── configuration/
│   │       │   ├── mod.rs
│   │       │   ├── entity.rs
│   │       │   ├── value_objects.rs
│   │       │   └── error.rs
│   │       │
│   │       └── system/
│   │           ├── mod.rs
│   │           └── value_objects.rs
│   │
│   ├── application/
│   │   └── src/
│   │       ├── lib.rs
│   │       │
│   │       ├── ports/
│   │       │   ├── mod.rs
│   │       │   ├── mihomo.rs
│   │       │   ├── subscription.rs
│   │       │   ├── process.rs
│   │       │   ├── config_repository.rs
│   │       │   ├── subscription_repository.rs
│   │       │   ├── filesystem.rs
│   │       │   ├── scheduler.rs
│   │       │   └── system.rs
│   │       │
│   │       ├── commands/
│   │       │   ├── mihomo/
│   │       │   ├── subscription/
│   │       │   └── configuration/
│   │       │
│   │       └── queries/
│   │           ├── mihomo/
│   │           ├── subscription/
│   │           ├── configuration/
│   │           └── system/
│   │
│   ├── infrastructure/
│   │   └── src/
│   │       ├── lib.rs
│   │       │
│   │       ├── mihomo/
│   │       │   ├── client.rs
│   │       │   ├── unix_socket.rs
│   │       │   └── process.rs
│   │       │
│   │       ├── subscription/
│   │       │   ├── sub_store.rs
│   │       │   ├── sub_store_convert.rs
│   │       │   └── native.rs
│   │       │
│   │       ├── persistence/
│   │       │   ├── sqlite/
│   │       │   └── filesystem/
│   │       │
│   │       ├── process/
│   │       │   └── tokio.rs
│   │       │
│   │       ├── scheduler/
│   │       │   └── tokio.rs
│   │       │
│   │       └── system/
│   │           ├── linux.rs
│   │           ├── systemd.rs
│   │           ├── nftables.rs
│   │           └── lxc.rs
│   │
│   ├── interfaces/
│   │   └── src/
│   │       ├── api/
│   │       │   ├── router.rs
│   │       │   ├── state.rs
│   │       │   ├── handlers/
│   │       │   ├── dto/
│   │       │   └── middleware/
│   │       │
│   │       ├── cli/
│   │       │   ├── mod.rs
│   │       │   ├── commands/
│   │       │   └── output.rs
│   │       │
│   │       └── tui/
│   │           ├── app.rs
│   │           ├── event.rs
│   │           ├── state.rs
│   │           ├── components/
│   │           └── screens/
│   │
│   └── bootstrap/
│       └── src/
│           ├── main.rs
│           ├── container.rs
│           ├── config.rs
│           └── logging.rs
│
├── frontend/
│   ├── admin/
│   └── metacubexd/
│
├── migrations/
│
├── packaging/
│   ├── systemd/
│   ├── deb/
│   └── install.sh
│
├── tests/
│
├── docs/
│   ├── architecture.md
│   ├── api.md
│   ├── deployment.md
│   └── development.md
│
└── README.md
```

---

# 7. Cargo Workspace

根目录：

```toml
[workspace]
resolver = "2"

members = [
    "crates/domain",
    "crates/application",
    "crates/infrastructure",
    "crates/interfaces",
    "crates/bootstrap",
]
```

依赖关系：

```text
domain
  ↑
application
  ↑
infrastructure
interfaces
  ↑
bootstrap
```

注意：

`application` 定义 Trait，`infrastructure` 实现 Trait。

---

# 8. Domain Model

## 8.1 MihomoInstance

```rust
pub struct MihomoInstance {
    pub id: MihomoInstanceId,
    pub name: String,
    pub version: MihomoVersion,
    pub status: MihomoStatus,
    pub active_config: ConfigVersionId,
}
```

状态：

```rust
pub enum MihomoStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    Unknown,
}
```

---

# 9. Subscription

```rust
pub struct Subscription {
    pub id: SubscriptionId,
    pub name: String,
    pub source: SubscriptionSource,
    pub converter: ConverterId,
    pub enabled: bool,
    pub schedule: Option<Schedule>,
}
```

Source：

```rust
pub enum SubscriptionSource {
    Url {
        url: Url,
        user_agent: Option<String>,
    },
}
```

后续可以增加：

```text
LocalFile
ManualContent
Generated
```

---

# 10. Converter Port

这是整个架构最重要的 Port 之一。

```rust
#[async_trait]
pub trait SubscriptionConverter: Send + Sync {
    async fn convert(
        &self,
        request: ConvertRequest,
    ) -> Result<ConvertedSubscription>;
}
```

模型：

```rust
pub struct ConvertRequest {
    pub source: SubscriptionSource,
    pub target: TargetFormat,
    pub options: ConvertOptions,
}
```

目标格式：

```rust
pub enum TargetFormat {
    Mihomo,
    SingBox,
    Surge,
    Clash,
}
```

MVP 只要求：

```text
Mihomo
```

---

# 11. Converter Adapter

Infrastructure：

```text
subscription/
├── sub_store.rs
├── sub_store_convert.rs
└── native.rs
```

实现：

```rust
pub struct SubStoreConverter;

pub struct SubStoreConvertAdapter;

pub struct NativeConverter;
```

Application 永远只依赖：

```rust
SubscriptionConverter
```

不会依赖：

```rust
SubStoreConverter
```

---

# 12. Sub-Store 集成策略

优先级：

```text
1. Official Sub-Store HTTP interface
2. sub-store-convert
3. Native Rust fallback
```

官方 Sub-Store 的 Wiki 公开记录了 `/download/sub` 链接参数，支持 `target`、`url`、`content`、`ua`、`proxy`、`mergeSources` 等参数，并能够根据目标生成对应订阅格式。

因此 MVP 不依赖 Sub-Store 的内部实现细节，而通过公开的订阅生成接口适配。

例如：

```text
Rust
 │
 ▼
SubStoreConverter
 │
 ▼
GET /download/sub
    ?target=ClashMeta
    &url=...
    &ua=...
 │
 ▼
Mihomo Config
```

由于 Sub-Store 官方仓库为 AGPL-3.0，本项目应保持 Sub-Store 为独立外部组件，而不是第一阶段把其源码直接合并进 Rust 核心。

---

# 13. Mihomo Port

```rust
#[async_trait]
pub trait MihomoController: Send + Sync {
    async fn status(&self) -> Result<MihomoRuntimeStatus>;

    async fn reload(
        &self,
        config: ConfigPath,
    ) -> Result<()>;

    async fn proxies(
        &self,
    ) -> Result<ProxyList>;

    async fn connections(
        &self,
    ) -> Result<ConnectionList>;

    async fn traffic(
        &self,
    ) -> Result<TrafficStats>;
}
```

Mihomo 官方提供 REST API，并支持 Unix socket API。官方文档明确说明 Unix socket API 不验证 secret，因此若采用该模式，socket 文件权限本身就是安全边界。

默认：

```yaml
external-controller: 127.0.0.1:9090
```

或者使用：

```yaml
external-controller-unix: /run/proxy-agent/mihomo.sock
```

MVP 优先实现 Unix socket。

---

# 14. Process Port

```rust
#[async_trait]
pub trait ProcessManager: Send + Sync {
    async fn start(
        &self,
        spec: ProcessSpec,
    ) -> Result<ProcessHandle>;

    async fn stop(
        &self,
        process: ProcessHandle,
    ) -> Result<()>;

    async fn status(
        &self,
        process: ProcessHandle,
    ) -> Result<ProcessStatus>;
}
```

MVP 实现：

```text
TokioProcessManager
```

以后可以：

```text
SystemdProcessManager
```

---

# 15. Config Domain

核心实体：

```rust
pub struct ConfigVersion {
    pub id: ConfigVersionId,
    pub mihomo_instance_id: MihomoInstanceId,
    pub version: u64,
    pub source: ConfigSource,
    pub checksum: String,
    pub created_at: DateTime<Utc>,
}
```

ConfigSource：

```rust
pub enum ConfigSource {
    Subscription(SubscriptionId),
    Manual,
    Imported,
    Generated,
}
```

---

# 16. Configuration 生命周期

必须采用 immutable version。

```text
Subscription
      │
      ▼
Generated Config
      │
      ▼
Validate
      │
      ▼
Config v12
      │
      ▼
Activate
      │
      ▼
Mihomo Reload
```

目录：

```text
/var/lib/proxy-agent/
└── configs/
    ├── v001.yaml
    ├── v002.yaml
    ├── v003.yaml
    └── v004.yaml

active -> configs/v004.yaml
```

禁止：

```text
直接覆盖 active config
```

必须：

```text
write temp
   ↓
fsync
   ↓
rename
   ↓
activate
```

---

# 17. 更新失败策略

订阅更新必须满足：

```text
旧配置 Running
        │
        ▼
更新订阅
        │
        ▼
转换
        │
        ▼
Validate
        │
        ├── FAIL ──→ 保留旧 Config
        │
        ▼
创建新 Version
        │
        ▼
切换
        │
        ▼
Reload
        │
        ├── FAIL ──→ rollback
        │
        ▼
SUCCESS
```

绝不能出现：

```text
订阅失败
    ↓
config.yaml 被清空
    ↓
Mihomo 起不来
```

---

# 18. Subscription Use Case

```rust
pub struct UpdateSubscriptionUseCase {
    subscription_repo: Arc<dyn SubscriptionRepository>,
    converter: Arc<dyn SubscriptionConverter>,
    config_repo: Arc<dyn ConfigRepository>,
    mihomo: Arc<dyn MihomoController>,
}
```

业务流程：

```text
1. Load Subscription
2. Check enabled
3. Convert
4. Parse / validate
5. Save ConfigVersion
6. Activate
7. Reload Mihomo
8. Health Check
9. Commit result
10. Rollback on failure
```

---

# 19. Scheduler

不依赖系统 Cron 作为核心。

Application：

```rust
pub trait Scheduler {
    async fn schedule(
        &self,
        job: ScheduledJob,
    ) -> Result<JobId>;
}
```

Infrastructure：

```text
TokioScheduler
```

MVP 支持：

```text
every N minutes
hourly
daily
cron-like expression
```

后续可以让 systemd timer 成为可选 Adapter。

---

# 20. System Domain

系统环境：

```rust
pub struct SystemEnvironment {
    pub os: OperatingSystem,
    pub arch: Architecture,
    pub kernel: KernelVersion,
    pub init: InitSystem,
    pub container: ContainerEnvironment,
}
```

Container：

```rust
pub enum ContainerEnvironment {
    BareMetal,
    VirtualMachine,
    Lxc,
    Docker,
    Unknown,
}
```

能力：

```rust
pub struct NetworkCapabilities {
    pub tun: CapabilityStatus,
    pub nftables: CapabilityStatus,
    pub iptables: CapabilityStatus,
    pub net_admin: CapabilityStatus,
}
```

---

# 21. Doctor

这是产品的核心特色之一。

```bash
proxyctl doctor
```

输出：

```text
System
────────────────────────
OS              Debian 13
Arch            amd64
Kernel          6.x
Init            systemd
Container       PVE LXC

Mihomo
────────────────────────
Binary          ✓
Version         ✓
Config          ✓
Controller      ✓

Network
────────────────────────
/dev/net/tun    ✓
CAP_NET_ADMIN   ✓
nftables        ✓
iptables        ✓

Runtime
────────────────────────
Mihomo process  ✓
API             ✓

Result
────────────────────────
Basic Proxy     ✓
TUN             ✓
Transparent     ⚠
```

每一个 capability 必须能够表达：

```text
Supported
Unsupported
Unavailable
Misconfigured
Unknown
```

不能使用简单 bool。

---

# 22. Linux / PVE LXC 策略

MVP 目标：

```text
Debian/Ubuntu
+
systemd
+
PVE LXC
```

不一开始兼容所有 init system。

系统功能分层：

```text
Basic Proxy
    ↓
Mihomo process
    ↓
HTTP/SOCKS/Mixed Port
```

高级能力：

```text
TUN
    ↓
CAP_NET_ADMIN
/dev/net/tun
routing
```

透明代理：

```text
nftables
iptables
routing
```

LXC 的能力可能取决于容器配置，因此不得假定：

```text
LXC == full network capabilities
```

而应该由 `doctor` 动态检测。

---

# 23. systemd

MVP 使用：

```text
proxy-agent.service
```

服务职责：

```text
proxy-agent
    │
    ├── Web API
    ├── CLI local API
    ├── TUI local API
    ├── Scheduler
    └── Mihomo Manager
```

Mihomo 不一定作为独立 systemd unit。

默认模型：

```text
systemd
   ↓
proxy-agent
   ↓
Mihomo process
```

这样 Agent 掌握：

```text
Mihomo 生命周期
配置切换
Health Check
Rollback
```

以后再提供：

```text
proxy-agent.service
mihomo.service
```

双 unit 模式。

---

# 24. Web API

基础：

```text
GET    /api/v1/system
GET    /api/v1/mihomo
POST   /api/v1/mihomo/start
POST   /api/v1/mihomo/stop
POST   /api/v1/mihomo/restart
POST   /api/v1/mihomo/reload

GET    /api/v1/configs
GET    /api/v1/configs/:id
POST   /api/v1/configs/:id/activate
POST   /api/v1/configs/:id/rollback
POST   /api/v1/configs/validate

GET    /api/v1/subscriptions
POST   /api/v1/subscriptions
PATCH  /api/v1/subscriptions/:id
DELETE /api/v1/subscriptions/:id

POST   /api/v1/subscriptions/:id/update

GET    /api/v1/logs
GET    /api/v1/health
GET    /api/v1/doctor
```

实时信息：

```text
WebSocket
/ws/v1/events
```

事件：

```text
mihomo.status
mihomo.traffic
mihomo.log
subscription.update
config.changed
system.capability
```

---

# 25. Web API DTO

禁止：

```rust
return DomainEntity;
```

必须：

```text
Domain Entity
    ↓
Application Result
    ↓
API DTO
```

例如：

```rust
pub struct MihomoStatusResponse {
    pub state: String,
    pub version: String,
    pub uptime_seconds: u64,
}
```

---

# 26. TUI

技术栈：

```text
ratatui
crossterm
```

TUI 是：

```text
Interface Adapter
```

不是第二套业务逻辑。

启动：

```bash
proxyctl
```

进入：

```text
┌─────────────────────────────────────────────────────────────┐
│ Proxy Manager                         Mihomo ● RUNNING      │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│ Mihomo                                                      │
│ Version      v1.x.x                                         │
│ Uptime       3d 12h                                         │
│ CPU          1.4%                                           │
│ Memory       82 MB                                          │
│                                                             │
│ Traffic                                                     │
│ ↑ 12.4 MB/s      ↓ 84.1 MB/s                              │
│                                                             │
│ Proxy Group                                                 │
│ ┌─────────────────────────────────────────────────────────┐ │
│ │ Proxy                                                   │ │
│ │ ├── HK-01     32ms                                      │ │
│ │ ├── JP-01     48ms                                      │ │
│ │ └── SG-01     12ms                                      │ │
│ └─────────────────────────────────────────────────────────┘ │
│                                                             │
│ [1] Status [2] Proxy [3] Config [4] Logs                  │
│ [5] Sub   [6] Doctor [7] Update [q] Quit                  │
└─────────────────────────────────────────────────────────────┘
```

TUI 重点：

```text
状态
操作
日志
故障排查
代理组
订阅
配置 rollback
```

复杂配置编辑仍交给 Web。

---

# 27. TUI 通信模式

推荐：

```text
proxyctl
   │
   ▼
Unix Socket
   │
   ▼
proxy-agent
```

而不是：

```text
TUI
 ↓
直接访问 Mihomo
```

这样：

```text
TUI
CLI
Web
```

都通过同一套 Agent API 操作。

推荐 Socket：

```text
/run/proxy-agent/agent.sock
```

权限：

```text
root
+
proxy-agent group
```

---

# 28. CLI / TUI / Web 统一业务入口

例如：

```text
CLI:
proxyctl subscription update foo

TUI:
[Update]

Web:
POST /api/v1/subscriptions/foo/update
```

三者最终：

```text
UpdateSubscriptionUseCase
```

---

# 29. Mihomo Dashboard 集成

不重新开发完整代理 Dashboard。

使用官方：

```text
metacubexd
```

方式：

```text
Browser
   │
   ▼
proxy-agent
   │
   ├── /admin/*
   └── /dashboard/*
               │
               ▼
           metacubexd
```

或者第一阶段更简单：

```text
http://agent/dashboard
```

直接提供静态文件。

官方 `metacubexd` 支持 standalone hosted panel，并且能够连接已有 Mihomo；官方仓库当前也包含独立面板与服务器模式。

---

# 30. 安全模型

默认原则：

```text
Mihomo API
    ↓
127.0.0.1 / Unix Socket

Agent API
    ↓
127.0.0.1
```

远程访问：

```text
Reverse Proxy
    +
Authentication
```

不要默认：

```text
Mihomo
0.0.0.0:9090
```

Mihomo 官方文档明确支持本地监听、Unix socket，并提醒远程暴露时应处理 secret/CORS；Unix socket 模式不校验 secret，需要依赖 socket 权限保护。

---

# 31. Authentication

MVP：

```text
Local:
Unix Socket → OS permission

Web:
username/password
```

之后：

```text
API Token
Session
Role
```

推荐至少：

```text
ADMIN
READ_ONLY
```

---

# 32. 配置与数据存储

Root：

```text
/etc/proxy-agent/
```

运行数据：

```text
/var/lib/proxy-agent/
```

运行时：

```text
/run/proxy-agent/
```

日志：

```text
/var/log/proxy-agent/
```

建议：

```text
/etc/proxy-agent/
├── config.toml
└── mihomo.defaults.yaml

/var/lib/proxy-agent/
├── configs/
├── subscriptions/
├── cache/
├── state/
└── database.sqlite

/run/proxy-agent/
├── agent.sock
└── mihomo.sock
```

---

# 33. SQLite

SQLite 只存元数据：

```text
subscriptions
config_versions
jobs
job_runs
settings
audit_logs
mihomo_versions
```

不把完整 YAML 强制塞入 SQLite。

YAML：

```text
/var/lib/proxy-agent/configs/
```

SQLite：

```text
metadata
```

这样备份和恢复更直观。

---

# 34. 数据模型

## subscriptions

```text
id
name
url
user_agent
converter
enabled
schedule
created_at
updated_at
last_update_at
last_update_status
last_error
```

## config_versions

```text
id
version
mihomo_instance_id
source_type
source_id
path
sha256
created_at
activated_at
```

## job_runs

```text
id
job_type
target_id
status
started_at
finished_at
error
```

## audit_logs

```text
id
action
actor
target
result
metadata
created_at
```

---

# 35. 错误模型

不要直接：

```rust
anyhow::Error
```

贯穿所有层。

Domain：

```rust
DomainError
```

Application：

```rust
ApplicationError
```

Infrastructure：

```rust
InfrastructureError
```

最终：

```text
API Error
CLI Error
TUI Error
```

例如：

```rust
pub enum ApplicationError {
    NotFound,
    InvalidState,
    ValidationFailed,
    ConverterUnavailable,
    ConfigActivationFailed,
    MihomoReloadFailed,
    RollbackFailed,
}
```

---

# 36. Logging

统一使用：

```text
tracing
tracing-subscriber
```

日志：

```text
ERROR
WARN
INFO
DEBUG
TRACE
```

结构化字段：

```text
subscription_id
config_version
mihomo_instance_id
job_id
```

例如：

```text
INFO subscription_id=sub_01
     config_version=42
     event="subscription_updated"
```

---

# 37. 配置格式

Agent 自己使用：

```toml
```

例如：

```toml
[server]
listen = "127.0.0.1:8765"

[storage]
data_dir = "/var/lib/proxy-agent"
config_dir = "/etc/proxy-agent"

[mihomo]
binary = "/usr/lib/proxy-agent/mihomo/current"
controller = "unix:///run/proxy-agent/mihomo.sock"

[subscription]
default_converter = "sub-store"

[scheduler]
enabled = true
```

Mihomo 原始配置继续使用：

```yaml
```

两者不要混用。

---

# 38. Mihomo Binary 管理

目录：

```text
/usr/lib/proxy-agent/mihomo/
├── mihomo-v1.x.x
├── mihomo-v1.x.x
└── current -> mihomo-v1.x.x
```

更新：

```text
Query release
      ↓
Download
      ↓
Verify SHA256
      ↓
Install new binary
      ↓
Smoke test
      ↓
Switch symlink
      ↓
Restart
```

失败：

```text
保持 current
```

---

# 39. Config Update 与 Mihomo Update 必须独立

两个概念：

```text
Mihomo Update
    = kernel update

Config Update
    = proxy/config update
```

UI 不应该混成：

```text
Update
```

而应该：

```text
Kernel
 └── Check Update

Subscription
 └── Update Now
```

---

# 40. Health Check

Agent 必须定义自己的健康检查：

```text
Process alive?
       ↓
Controller reachable?
       ↓
Mihomo API responsive?
       ↓
Current config loaded?
       ↓
Proxy port listening?
       ↓
Optional traffic test
```

状态：

```text
Healthy
Degraded
Unhealthy
```

---

# 41. Config Validation

至少做三层。

### Level 1

YAML Syntax

### Level 2

Mihomo Config Schema

### Level 3

Mihomo 实际启动测试

最终：

```text
generate
 ↓
syntax validate
 ↓
semantic validate
 ↓
temporary startup
 ↓
health check
 ↓
activate
```

---

# 42. Rollback

CLI：

```bash
proxyctl config rollback 41
```

TUI：

```text
Config History
──────────────────
v44 ● active
v43
v42
v41

[Enter] Activate
```

Web：

```text
Version History
 ├── Diff
 ├── View
 ├── Validate
 └── Rollback
```

---

# 43. Sub-Store Adapter 详细策略

Adapter：

```rust
pub struct SubStoreConverter {
    base_url: Url,
    client: reqwest::Client,
}
```

请求：

```text
GET /download/sub
```

根据统一内部模型生成参数：

```text
target
url
ua
content
proxy
mergeSources
...
```

官方 Wiki 当前明确记录了这些参数及编码规则，因此 Adapter 层应该集中处理参数序列化，不让 API/UI 直接拼 URL。

---

# 44. Sub-Store 可用性策略

启动时：

```text
Sub-Store configured?
    │
    ├── YES → Health Check
    │
    └── NO
          ↓
      Native / Disabled
```

运行时：

```text
Sub-Store unavailable
       ↓
Subscription Update Failed
       ↓
Old Config stays active
```

绝不能：

```text
Sub-Store timeout
   ↓
disable Mihomo
```

---

# 45. Native Converter

Native converter 不在 MVP 实现完整版本。

但从第一天保留接口：

```rust
pub struct NativeConverter;
```

状态：

```text
NotImplemented
```

未来可能承担：

```text
URI parser
Node model
Mihomo serializer
```

用途：

> 当外部 Converter 失效时，至少保留基础转换能力。

---

# 46. Web 与 Agent 分离

推荐部署模型：

```text
                Browser
                   │
                   ▼
             proxy-agent :8765
                   │
          ┌────────┴────────┐
          ▼                 ▼
       Admin UI          Mihomo API
                            │
                            ▼
                          Mihomo
```

Agent 自己提供：

```text
Admin UI
REST API
WebSocket
```

并代理：

```text
Mihomo Dashboard
```

这样用户只记一个地址：

```text
http://server:8765
```

---

# 47. Frontend

目录：

```text
frontend/
├── admin/
└── metacubexd/
```

Admin：

```text
React / Vue / Svelte
```

第一版功能：

```text
Overview
Mihomo
Subscriptions
Configs
System
Settings
Logs
```

`metacubexd` 独立为：

```text
frontend/metacubexd/
```

不修改其核心代码。

---

# 48. 推荐 Rust 技术栈

## Runtime

```text
tokio
```

## Web

```text
axum
tower
tower-http
```

## HTTP

```text
reqwest
```

## Serialization

```text
serde
serde_json
toml
serde_yaml / serde_yml
```

## CLI

```text
clap
```

## TUI

```text
ratatui
crossterm
```

## Logging

```text
tracing
tracing-subscriber
```

## Async Trait

```text
async-trait
```

## Database

```text
sqlx
sqlite
```

## Error

```text
thiserror
```

Application 层尽量：

```text
thiserror
```

Infrastructure 边界再根据需要：

```text
anyhow
```

---

# 49. Application Use Cases

MVP：

```text
mihomo/
├── start
├── stop
├── restart
├── reload
├── status
└── update

subscription/
├── create
├── update
├── delete
├── list
└── test

configuration/
├── list
├── show
├── validate
├── activate
├── rollback
└── diff

system/
├── doctor
└── capabilities
```

---

# 50. Query / Command

概念上分：

```text
commands/
queries/
```

但不实现复杂 CQRS。

Command：

```text
StartMihomo
UpdateSubscription
ActivateConfig
RollbackConfig
```

Query：

```text
GetMihomoStatus
ListConfigs
GetSystemOverview
ListSubscriptions
```

---

# 51. Testing Strategy

## Domain

纯单元测试：

```text
100% business rule focus
```

不需要：

```text
Tokio runtime
SQLite
Linux
```

---

## Application

Mock Port：

```text
MockMihomoController
MockSubscriptionConverter
MockConfigRepository
```

验证：

```text
Subscription Update
 ↓
Validate
 ↓
Save
 ↓
Reload
```

以及：

```text
Reload fail
 ↓
Rollback
```

---

## Infrastructure

真实：

```text
Linux
Mihomo
Sub-Store
SQLite
```

进行 integration test。

---

## E2E

最终：

```text
install
 ↓
doctor
 ↓
start
 ↓
subscription update
 ↓
config activate
 ↓
mihomo health
 ↓
dashboard
```

---

# 52. CI

第一阶段：

```text
cargo fmt --check
cargo clippy
cargo test
cargo build
```

后期：

```text
cargo nextest
cargo audit
cargo deny
```

构建：

```text
x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu
```

后续：

```text
armv7
```

---

# 53. Release

建议：

```text
GitHub Releases
```

Artifacts：

```text
proxy-agent-linux-amd64.tar.gz
proxy-agent-linux-arm64.tar.gz
proxyctl-linux-amd64
proxyctl-linux-arm64
```

MVP 可以让：

```text
proxy-agent
proxyctl
```

暂时共用同一 binary：

```bash
proxy-agent daemon
proxy-agent tui
proxy-agent start
```

后期再拆：

```text
proxy-agent
proxyctl
```

---

# 54. 安装流程

```bash
curl -fsSL https://example.com/install.sh | sh
```

安装器：

```text
Detect OS
   ↓
Detect Arch
   ↓
Install binary
   ↓
Create user/group
   ↓
Create directories
   ↓
Install systemd unit
   ↓
Install default config
   ↓
proxy-agent doctor
   ↓
Start
```

---

# 55. 用户权限

推荐：

```text
proxy-agent
```

作为独立 system user。

需要高权限的能力：

```text
nftables
TUN
systemd
network
```

尽量通过：

```text
Linux capabilities
```

最小授权。

不要让整个 Web UI 直接拥有 root shell 权限。

---

# 56. Security Boundary

```text
                 Internet
                    │
                    ▼
                Reverse Proxy
                    │
                    ▼
                Web API
                    │
              Authentication
                    │
                    ▼
              proxy-agent
                    │
         ┌──────────┼──────────┐
         ▼          ▼          ▼
      Mihomo     systemd    nftables
```

Web 请求绝不能直接：

```text
exec("whatever")
```

所有操作：

```text
HTTP
 ↓
DTO
 ↓
Application UseCase
 ↓
Port
```

---

# 57. Audit Log

高风险操作记录：

```text
mihomo.start
mihomo.stop
mihomo.restart
mihomo.update
config.activate
config.rollback
subscription.update
system.nftables.apply
```

例如：

```text
2026-09-12 10:32
actor=local-admin
action=config.rollback
target=config:v41
result=success
```

---

# 58. TUI 与 Web 数据统一

Application Query：

```rust
pub struct SystemOverview {
    pub mihomo: MihomoOverview,
    pub subscription: SubscriptionOverview,
    pub system: SystemOverview,
}
```

Web：

```text
SystemOverviewResponse
```

TUI：

```text
SystemOverview
```

CLI：

```text
TextSystemOverview
```

避免三套状态查询实现。

---

# 59. Event Bus

MVP 不引入 Kafka 等。

只使用：

```rust
tokio::sync::broadcast
```

例如：

```text
AgentEvent
├── MihomoStatusChanged
├── MihomoLog
├── TrafficUpdated
├── SubscriptionUpdated
├── ConfigActivated
└── JobFinished
```

用途：

```text
WebSocket
TUI
Logs
```

这是 Application 层内部事件机制，不是 Event Sourcing。

---

# 60. Repository

Application：

```rust
#[async_trait]
pub trait ConfigRepository {
    async fn list(&self) -> Result<Vec<ConfigVersion>>;
    async fn get(&self, id: ConfigVersionId) -> Result<Option<ConfigVersion>>;
    async fn save(&self, config: ConfigVersion) -> Result<()>;
    async fn activate(&self, id: ConfigVersionId) -> Result<()>;
}
```

Infrastructure：

```text
SqliteConfigRepository
FilesystemConfigStore
```

两者职责分离：

```text
SQLite
= metadata

Filesystem
= YAML
```

---

# 61. 初期不要过度抽象

MVP 明确禁止：

```text
GenericRepository<T>
GenericService<T>
GenericCrudUseCase<T>
```

每个 Domain 用明确类型：

```text
ConfigRepository
SubscriptionRepository
MihomoController
SubscriptionConverter
```

可读性优先。

---

# 62. 第一阶段 Git Commit 顺序

建议严格按照下面顺序开始。

```text
1. workspace skeleton

2. domain models

3. application ports

4. mihomo process adapter

5. mihomo controller adapter

6. config repository

7. start/stop/status

8. config validation

9. config versioning

10. rollback

11. subscription repository

12. Sub-Store converter

13. subscription update use case

14. scheduler

15. REST API

16. CLI

17. TUI

18. metacubexd integration

19. system doctor

20. systemd packaging

21. installer

22. E2E tests
```

---

# 63. Milestone M0 — Skeleton

目标：

```text
cargo build
cargo test
```

项目：

```text
domain
application
infrastructure
interfaces
bootstrap
```

验收：

```text
Dependency direction correct
No Infrastructure dependency in Domain
```

---

# 64. Milestone M1 — Mihomo

目标：

```text
proxyctl start
proxyctl stop
proxyctl restart
proxyctl status
```

完成：

```text
ProcessManager
MihomoController
Health Check
```

验收：

```text
Mihomo can start
Mihomo can stop
Mihomo API reachable
```

---

# 65. Milestone M2 — Config

完成：

```text
ConfigVersion
ConfigRepository
Validate
Activate
Rollback
Diff
```

验收：

```text
v1
v2
v3

active=v3

rollback → v2
```

---

# 66. Milestone M3 — Subscription

完成：

```text
Subscription
SubscriptionRepository
SubscriptionConverter
SubStoreAdapter
UpdateSubscription
```

验收：

```text
URL
 ↓
Sub-Store
 ↓
Mihomo Config
 ↓
Validate
 ↓
Activate
 ↓
Reload
```

失败必须：

```text
old config remains active
```

---

# 67. Milestone M4 — Web

完成：

```text
Axum
REST
WebSocket
Admin UI
```

验收：

```text
Browser
 ↓
Agent
 ↓
View status
 ↓
Update subscription
 ↓
View logs
 ↓
Rollback config
```

---

# 68. Milestone M5 — TUI

完成：

```text
ratatui
```

页面：

```text
Overview
Mihomo
Proxy Groups
Configs
Subscriptions
Logs
Doctor
```

验收：

```bash
ssh server
proxyctl
```

完全可操作。

---

# 69. Milestone M6 — Linux

完成：

```text
systemd
doctor
nftables detection
TUN detection
LXC detection
```

验收：

```bash
proxyctl doctor
```

能够准确回答：

```text
为什么 TUN 能用/不能用
为什么 nftables 能用/不能用
```

---

# 70. Milestone M7 — Release

完成：

```text
GitHub release
install.sh
upgrade
rollback
backup
uninstall
```

目标：

```bash
curl -fsSL ... | sh
```

安装完成后：

```bash
proxyctl
```

直接可用。

---

# 71. MVP 完整用户体验

安装：

```bash
curl -fsSL https://example.com/install.sh | sh
```

诊断：

```bash
proxyctl doctor
```

启动：

```bash
proxyctl start
```

状态：

```bash
proxyctl status
```

进入 TUI：

```bash
proxyctl
```

浏览器：

```text
http://server:8765
```

Web：

```text
Overview
   │
   ├── Mihomo
   ├── Subscription
   ├── Config
   ├── Logs
   └── System
```

Dashboard：

```text
/dashboard
```

直接进入 `metacubexd`。

---

# 72. 后续版本

## v0.2

```text
TUN
nftables
Config Editor
Backup
More subscription operators
```

## v0.3

```text
Multi-instance Mihomo
OpenRC
More architectures
Remote management
API Token
```

## v0.4

```text
PVE integration
Node health
Remote agent
Multi-server
```

---

# 73. 一个重要的架构决策：Agent vs CLI

最终建议：

```text
proxy-agent
```

负责：

```text
daemon
HTTP API
WebSocket
scheduler
Mihomo lifecycle
system management
```

而：

```text
proxyctl
```

负责：

```text
CLI
TUI
```

通信：

```text
proxyctl
    ↓
Unix socket
    ↓
proxy-agent
```

以后：

```text
Web
CLI
TUI
Remote API
```

全部成为 Client。

---

# 74. 最终系统图

```text
                         User
                          │
             ┌────────────┼─────────────┐
             │            │             │
             ▼            ▼             ▼
            Web           CLI           TUI
             │            │             │
             └────────────┼─────────────┘
                          │
                     Unix Socket
                     / HTTP API
                          │
                   ┌──────▼──────┐
                   │ proxy-agent │
                   ├─────────────┤
                   │ Application │
                   │ Domain      │
                   │ Scheduler   │
                   │ Event Bus   │
                   └──────┬──────┘
                          │
         ┌────────────────┼────────────────┐
         │                │                │
         ▼                ▼                ▼
      Mihomo          Subscription      System
      Adapter            Adapter         Adapter
         │                │                │
         ▼                ▼                ▼
      Mihomo          Sub-Store        systemd
                                       nftables
                                       LXC
```

---

# 75. 最终核心原则

整个项目只需要牢牢记住下面十条：

```text
1. Mihomo 是 Data Plane
2. proxy-agent 是 Control Plane
3. Web / CLI / TUI 都是 Interface Adapter
4. Domain 不依赖 Infrastructure
5. Application 不依赖具体实现
6. Sub-Store 是可插拔 Converter
7. Config 必须版本化
8. 更新失败必须保留旧配置
9. Linux 能力必须通过 Doctor 动态检测
10. MVP 使用 Modular Monolith，不做微服务
```

最终产品形态：

```text
             ┌─────────────────────┐
             │    proxy-agent      │
             │                     │
             │ DDD + Hexagonal     │
             │                     │
             │ Mihomo Management   │
             │ Subscription        │
             │ Config Versioning   │
             │ System Management   │
             │ Scheduler           │
             │ API                 │
             └──────────┬──────────┘
                        │
          ┌─────────────┼──────────────┐
          ▼             ▼              ▼
       Mihomo       Sub-Store        Linux
                                      │
                                 systemd/LXC
                        │
                 ┌──────┴──────┐
                 ▼             ▼
                Web          proxyctl
                               │
                          ┌────┴────┐
                          ▼         ▼
                         CLI       TUI
```

这个架构可以直接进入编码阶段，不需要先再做一次大重构。