# Application 层详细设计

> 状态：Accepted（设计冻结，待实现）
> 日期：2026-09-12
> 依据：`docs/adr/ADR-001..006`、`docs/design/domain-ports-bootstrap.md`、`docs/research/requirements.md`、`AGENTS.md`
> 前置：`crates/domain` 已实现并通过全部质量门（146 tests）
> 范围：`crates/application`（Ports、Use Cases、jobs、error）
> 非范围：`infrastructure`（仅列契约）、`interfaces`（REST/CLI/TUI）

---

## 0. 设计原则与已实证的工具链约束

### 0.1 三条硬约束

| # | 约束 | 来源 | 后果 |
|---|---|---|---|
| 1 | Application 依赖 Port，不依赖实现 | `AGENTS.md` | 只出现 `Arc<dyn Port>`，不出现 `reqwest`/`sqlx`/`systemd` |
| 2 | Port 必须可 `dyn` 装配 | domain 设计 §0.2 实证 | 使用 `#[async_trait]` |
| 3 | 生命周期与激活必须串行化 | `AGENTS.md` State and Concurrency | per-instance 锁在 Application，不在 adapter |

### 0.2 本轮新增的四项实证（决定设计形态）

在 `/tmp/probe-app` 用 rustc 1.100-nightly 实测，全部通过：

| # | 命题 | 结果 | 对设计的影响 |
|---|---|---|---|
| P1 | Use Case 持 `Arc<dyn Port>` 且自身 `Send + Sync + 'static` | 通过 | Use Case 可被 `Arc` 共享给 axum handler / TUI 任务 / CLI |
| P2 | per-instance 锁可在 `.await` 期间持有，且不同实例不互相阻塞 | 通过 | 锁用 `Arc<tokio::sync::Mutex<()>>` + `lock_owned()` |
| P3 | **取得锁时必须先释放 map 锁再 await 实例锁** | 通过（按此写法） | 若在持有 map 锁时 `await`，会与其他实例的取锁路径互锁 |
| P4 | 回滚基于**观测**而非记忆，在存储全挂时仍能报告真实状态 | 通过 | `rollback` 后必须重新 `active()` 校验，不信任调用方传入的 `previous` |

P3 是一个真实陷阱：`InstanceLocks::acquire` 若写成在 `map.lock().await` 的作用域内 `entry.lock_owned().await`，则实例 A 的取锁会阻塞实例 B 的 map 访问。正确写法是**先在短作用域内 clone 出 `Arc<Mutex<()>>`，drop map guard 后再 await 实例锁**。

### 0.3 职责边界：决策 vs 机制

| 归 Application（决策 + 顺序） | 归 Infrastructure（机制） |
|---|---|
| reload 后必须做什么健康检查 | 怎么发 `PUT /configs` |
| 健康检查失败必须 restart 回滚（不是 reload） | 怎么 kill/spawn 进程 |
| 端口冲突在预检阶段拦截 | 怎么探测端口占用 |
| 审计失败不阻断但告警 | 审计写到哪里 |
| 同一实例同时只能有一个激活在飞 | 文件怎么原子 rename |
| 订阅失败必须保留旧配置 | 怎么调 Sub-Store 的 HTTP 接口 |

判据：**"如果换一个 adapter，这个判断还成立吗？"** 成立 → Application；不成立 → Infrastructure。

---

## 1. crate 结构

```text
crates/application/
├── Cargo.toml
└── src/
    ├── lib.rs                 crate 文档 + 纯度属性
    ├── error.rs               ApplicationError / Degradation
    ├── context.rs             AppContext（共享依赖集合）
    ├── ports/
    │   ├── mod.rs             Port 一览 + PortError
    │   ├── error.rs           PortError（不泄漏 adapter 类型）
    │   ├── mihomo_controller.rs
    │   ├── mihomo_observer.rs
    │   ├── mihomo_connection_ops.rs
    │   ├── process_manager.rs
    │   ├── service_manager.rs
    │   ├── config_repository.rs
    │   ├── config_validator.rs
    │   ├── subscription_converter.rs
    │   ├── subscription_repository.rs
    │   ├── capability_probe.rs
    │   ├── secret_store.rs
    │   ├── audit_sink.rs
    │   ├── kernel_installer.rs
    │   ├── job_registry.rs
    │   └── event_publisher.rs
    ├── jobs.rs                JobId / JobState / JobKind
    ├── events.rs              DomainEvent 定义
    ├── locks.rs               InstanceLocks / SubscriptionGuards
    ├── commands/
    │   ├── mod.rs
    │   ├── activate_config.rs     ★ 唯一激活路径
    │   ├── rollback_config.rs
    │   ├── start_mihomo.rs
    │   ├── stop_mihomo.rs
    │   ├── restart_mihomo.rs
    │   ├── reload_mihomo.rs
    │   ├── update_kernel.rs
    │   ├── update_subscription.rs  ★ 保留旧配置的不变量
    │   ├── create_subscription.rs
    │   └── delete_subscription.rs
    └── queries/
        ├── mod.rs
        ├── get_mihomo_status.rs
        ├── list_configs.rs
        ├── list_subscriptions.rs
        ├── run_doctor.rs
        └── get_capabilities.rs
```

**依赖**：`proxy-domain`、`thiserror`、`async-trait`、`tokio`（仅 `sync` 特性，用于锁与 broadcast）。**不含** `reqwest`/`sqlx`/`axum`/`serde`。

---

## 2. Port 层

### 2.1 统一错误类型

```rust
// application/src/ports/error.rs
/// 所有 Port 的错误。**不向上泄漏 reqwest/sqlx/std::io 的具体类型。**
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("not reachable: {0}")]
    Unreachable(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("transport failure: {0}")]
    Transport(String),

    #[error("unexpected remote status: {status}")]
    UnexpectedStatus { status: u16 },

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("storage failure: {0}")]
    Storage(String),

    #[error("io failure: {0}")]
    Io(#[source] std::io::Error),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error(transparent)]
    Converter(#[from] ConverterError),

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

impl PortError {
    /// 只读/瞬时失败可重试；非法请求与未实现不可重试。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Unreachable(_) | Self::Transport(_) | Self::Timeout(_) | Self::Io(_)
        )
    }

    /// 是否属于"能力不可用"类降级（而非真错误）。
    #[must_use]
    pub fn is_degradation(&self) -> bool {
        matches!(self, Self::NotImplemented(_) | Self::PermissionDenied(_))
    }
}
```

> **为什么 `Unreachable` 里放 `Box<dyn Error>`**：保留原始错误链用于诊断，同时不让 adapter 的具名类型出现在 Application 的公开签名里。这是"保链但不泄漏"的最小代价方案。

### 2.2 控制面 Ports

```rust
// application/src/ports/mihomo_controller.rs
#[async_trait::async_trait]
pub trait MihomoController: Send + Sync {
    async fn version(&self) -> Result<MihomoBuild, PortError>;
    async fn runtime_config(&self) -> Result<RuntimeConfigSummary, PortError>;

    /// 唯一 reload 入口。
    /// ⚠ `ReloadRequest` 中**不存在 force 变体**（REQ-CONFIG-011）。
    /// ⚠ 返回 `Applied` 不代表生效 —— 调用方必须随后 health_check（R02 C1b）。
    async fn reload(&self, request: ReloadRequest) -> Result<ReloadOutcome, PortError>;

    async fn proxies(&self) -> Result<ProxyList, PortError>;
    async fn select_proxy(&self, group: &str, proxy: &str) -> Result<(), PortError>;
    async fn test_delay(&self, name: &str, opts: DelayOptions) -> Result<DelayOutcome, PortError>;
    async fn rules(&self) -> Result<RuleList, PortError>;
    async fn health_check(&self) -> Result<HealthReport, PortError>;
    async fn shutdown(&self) -> Result<(), PortError>;
}

/// payload 模式优先（绕过 SAFE_PATHS 与文件存在性两个失败点，R02 §11）
#[derive(Debug, Clone)]
pub enum ReloadRequest {
    /// 直接投递配置正文。
    Payload(ConfigBody),
    /// 走内核的路径白名单；要求路径已在 `SAFE_PATHS` 内。
    Path(ConfigPath),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// 内核接受了请求（**不等于已生效**）。
    Applied,
    /// 内核拒绝；带 HTTP 状态码用于诊断。
    Rejected { http_status: u16 },
}

/// 504 必须映射为业务结果，而不是基础设施错误（ADR-003 D6）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelayOutcome {
    Measured { millis: u32 },
    Timeout,
    Unavailable { reason: String },
}
```

```rust
// application/src/ports/process_manager.rs
/// 语义：管理 **Agent 自己的子进程**（MVP 权威形态）。
/// systemd 只负责拉起 proxy-agent 本身；管理独立 unit 属 ServiceManager。
#[async_trait::async_trait]
pub trait ProcessManager: Send + Sync {
    async fn start(&self, opts: StartOptions) -> Result<ProcessHandle, PortError>;
    async fn stop(&self, handle: &ProcessHandle, timeout: Duration) -> Result<ExitStatus, PortError>;
    async fn status(&self, handle: &ProcessHandle) -> Result<ProcessStatus, PortError>;
    /// 只允许白名单信号；实现必须拒绝 SIGUSR1/SIGUSR2（R03 实测：未注册 → 默认处置=终止）
    async fn signal(&self, handle: &ProcessHandle, signal: AllowedSignal) -> Result<(), PortError>;
}

/// 白名单信号枚举使得"误发 SIGUSR1"无法表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedSignal { Term, Kill }
```

### 2.3 配置 Ports

```rust
// application/src/ports/config_repository.rs
#[async_trait::async_trait]
pub trait ConfigRepository: Send + Sync {
    /// 必须带 limit，避免无界返回（规模爆炸防护）。
    async fn list(&self, instance: &MihomoInstanceId, limit: usize) -> Result<Vec<ConfigVersion>, PortError>;
    async fn get(&self, id: &ConfigVersionId) -> Result<Option<ConfigVersion>, PortError>;
    async fn next_sequence(&self, instance: &MihomoInstanceId) -> Result<u64, PortError>;

    /// **必须幂等**：重复保存同一 (id, checksum) 不得产生第二份记录。
    async fn save(&self, version: &ConfigVersion, body: &ConfigBody) -> Result<(), PortError>;

    async fn active(&self, instance: &MihomoInstanceId) -> Result<Option<ConfigVersion>, PortError>;

    /// **必须幂等**：重复设为同一 id 是成功的空操作。原子切换（temp+fsync+rename）。
    async fn set_active(&self, instance: &MihomoInstanceId, id: &ConfigVersionId) -> Result<(), PortError>;

    async fn read_body(&self, version: &ConfigVersion) -> Result<ConfigBody, PortError>;
}
```

> **幂等是本设计的前提（见 §6 风险）**。契约写进 trait 文档，因为 `ActivateConfig` 的回滚正确性依赖它。

```rust
// application/src/ports/config_validator.rs
/// 独立成 Port 的理由（R02 实测）：
/// ① `mihomo -t` 有副作用（含 GEOIP/GEOSITE 时真实下载 geodata，阻塞约 90s）
/// ② `mihomo -t` 对不存在的文件返回 exit 0 假成功
/// ③ `mihomo -t` 不检测未知字段 → 必须有独立的字段白名单校验
#[async_trait::async_trait]
pub trait ConfigValidator: Send + Sync {
    /// L0 资源预检：端口可用性 / geodata 就绪 / provider 可达。
    async fn preflight(
        &self,
        body: &ConfigBody,
        ctx: &PreflightContext,
    ) -> Result<LevelOutcome, PortError>;

    /// L1 YAML 语法。
    async fn validate_syntax(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError>;

    /// L2 语义 = `mihomo -t` ⊕ Agent 字段白名单。
    /// 实现必须：隔离临时 `-d` 目录、先确认文件存在、离线时跳过 geodata 相关。
    async fn validate_semantic(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError>;

    /// 供 L0 使用的端口占用观测。
    async fn observe_port_usage(&self, ports: &[u16]) -> Result<Vec<u16>, PortError>;
}
```

### 2.4 订阅 Ports

```rust
// application/src/ports/subscription_converter.rs
#[async_trait::async_trait]
pub trait SubscriptionConverter: Send + Sync {
    async fn convert(&self, request: ConvertRequest) -> Result<ConvertedProxies, PortError>;
    async fn capabilities(&self) -> Result<ConverterCapabilities, PortError>;
    async fn health(&self) -> Result<ConverterHealth, PortError>;
}

/// 只表达业务意图；**不含任何 Sub-Store 参数名**（REQ-SUB-001）
#[derive(Debug, Clone)]
pub struct ConvertRequest {
    pub source: SubscriptionSource,
    pub target: TargetFormat,
    pub proxy: Option<String>,
    pub merge_sources: bool,
    pub cache: CachePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy { PreferCache, Bypass }
```

```rust
// application/src/ports/subscription_repository.rs
#[async_trait::async_trait]
pub trait SubscriptionRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<Subscription>, PortError>;
    async fn get(&self, id: &SubscriptionId) -> Result<Option<Subscription>, PortError>;
    /// 幂等：按 id upsert。
    async fn save(&self, sub: &Subscription) -> Result<(), PortError>;
    async fn delete(&self, id: &SubscriptionId) -> Result<(), PortError>;
    /// 返回到期订阅；**不负责去重**（去重由 `SubscriptionGuards` 完成）。
    async fn due_for_update(&self, now: Timestamp) -> Result<Vec<SubscriptionId>, PortError>;
}
```

### 2.5 系统、安全与观测 Ports

```rust
// application/src/ports/capability_probe.rs
#[async_trait::async_trait]
pub trait CapabilityProbe: Send + Sync {
    async fn environment(&self) -> Result<SystemEnvironment, PortError>;
    /// 探测必须无副作用（REQ-LXC-005）
    async fn probe_all(&self, opts: ProbeOptions) -> Result<CapabilitySet, PortError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProbeOptions {
    /// 默认为 false：写类探测（nft 试写、ip rule）必须显式开启。
    pub allow_write_probes: bool,
}
```

```rust
// application/src/ports/secret_store.rs
#[async_trait::async_trait]
pub trait SecretStore: Send + Sync {
    async fn mihomo_secret(&self) -> Result<String, PortError>;
    async fn rotate_mihomo_secret(&self) -> Result<String, PortError>;
    async fn verify_api_token(&self, presented: &str) -> Result<Option<Principal>, PortError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal { pub id: String, pub role: Role }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role { Admin, ReadOnly }
```

```rust
// application/src/ports/audit_sink.rs
#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    /// append-only（REQ-SEC-007）
    async fn record(&self, entry: AuditEntry) -> Result<(), PortError>;
    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, PortError>;
}
```

```rust
// application/src/ports/job_registry.rs
/// 长操作的可观测句柄（方案 A）。实现负责存储与查询，Application 只读写状态。
#[async_trait::async_trait]
pub trait JobRegistry: Send + Sync {
    async fn create(&self, kind: JobKind, target: JobTarget) -> Result<JobId, PortError>;
    async fn update(&self, id: &JobId, state: JobState) -> Result<(), PortError>;
    async fn get(&self, id: &JobId) -> Result<Option<JobRecord>, PortError>;
    async fn recent(&self, limit: usize) -> Result<Vec<JobRecord>, PortError>;
}
```

```rust
// application/src/ports/event_publisher.rs
/// 事件发布抽象。MVP 实现是 `tokio::sync::broadcast`；抽象成 Port 使 Application
/// 不直接依赖 tokio 类型，也便于测试断言事件序列。
pub trait EventPublisher: Send + Sync {
    /// 无订阅者时静默丢弃（事件是通知，不是消息队列）。
    fn publish(&self, event: DomainEvent);
}
```

```rust
// application/src/ports/kernel_installer.rs
/// 自研内核更新链路（禁止依赖 `/upgrade`，ADR-003 D2）
#[async_trait::async_trait]
pub trait KernelInstaller: Send + Sync {
    async fn current(&self) -> Result<Option<KernelInstallation>, PortError>;
    async fn fetch(&self, version: &MihomoVersion) -> Result<DownloadedArtifact, PortError>;
    /// 校验强度见 open-questions Q005/Q015（至少 checksum）
    async fn verify(&self, artifact: &DownloadedArtifact, expected: &ConfigChecksum) -> Result<(), PortError>;
    /// 原子替换并保留上一版本
    async fn install(&self, artifact: &DownloadedArtifact) -> Result<KernelInstallation, PortError>;
    async fn rollback_previous(&self) -> Result<KernelInstallation, PortError>;
}
```

### 2.6 Port 附属数据类型（定义位置与归属）

Port 签名引用的类型必须显式定义，否则实现时无法对齐。归属判据同 §0.3：
**表达业务概念 → domain；只服务 adapter 的数据形状 → application**。

| 类型 | 归属 | 理由 |
|---|---|---|
| `CapabilityStatus`、`CapabilitySet`、`SystemEnvironment` | **domain**（已实现） | 业务概念，含判定规则 |
| `ConfigBody`、`ConfigVersion`、`ConfigCandidate`、`ValidationReport` | **domain**（已实现） | 业务概念 + 不变量载体 |
| `MihomoStatus`、`MihomoInstance`、`UpdateOutcome` | **domain**（已实现） | 业务概念 + 状态机 |
| `HealthReport` | **application**（见下） | 形状取决于内核 API 的可得字段，是 adapter 数据的收敛结果 |
| `ProxyList`、`RuleList`、`RuntimeConfigSummary` | **application**（见下） | 同上；**不进 domain**（R01：`/configs` 仅 33 字段，不足以表达业务状态） |
| `DelayOptions`、`DelayOutcome` | **application** | 探测参数与结果，非业务概念 |
| `ConverterCapabilities`、`ConverterHealth` | **application** | adapter 能力声明 |
| `LogLevel` | **application** | 观测面枚举 |

> **为什么不把这些放进 domain**：它们描述的是"从内核 API 拿到的字段集合"。domain 不应为了容纳 adapter 的字段形状而增长；ADR-003 D6 已明确 connections/traffic/logs 明细不进 Domain，此处遵循同一判据。

```rust
// application/src/ports/types.rs —— 以下全部为 Port 附属类型

/// 分层健康检查结果。L4（代理端口）不可省略：内核在 listener bind 失败时
/// 不致命、`/version` 仍返回 200（R01 C11 / R02 C1b）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    pub process_alive: bool,
    pub controller_reachable: bool,
    pub config_loaded: bool,
    pub proxy_port_listening: bool,
}

impl HealthReport {
    /// 全部为真才算健康；任何一层缺失都是 Degraded 或 Unhealthy。
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.process_alive
            && self.controller_reachable
            && self.config_loaded
            && self.proxy_port_listening
    }

    /// 进程或 controller 不可达 = Unhealthy；仅数据面缺失 = Degraded。
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.process_alive && self.controller_reachable && !self.proxy_port_listening
    }
}

/// 运行期配置摘要。仅包含 `/configs` 实际返回且我们关心的字段（R01 C8）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfigSummary {
    pub mode: String,
    pub mixed_port: Option<u16>,
    pub socks_port: Option<u16>,
    pub http_port: Option<u16>,
    pub log_level: Option<String>,
}

/// 代理节点与策略组视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyList {
    pub groups: Vec<ProxyGroupView>,
    pub proxies: Vec<ProxyView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyGroupView {
    pub name: String,
    pub kind: String,
    pub now: Option<String>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyView {
    pub name: String,
    pub kind: String,
    /// 最近一次延迟测试结果，若有。
    pub delay_millis: Option<u32>,
}

/// 规则列表视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleList {
    pub rules: Vec<RuleView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleView {
    pub kind: String,
    pub payload: String,
    pub target: String,
}

/// 延迟测试参数。默认不产生真实流量，需显式启用。
#[derive(Debug, Clone)]
pub struct DelayOptions {
    /// 测试 URL；由调用方提供，避免 adapter 内置默认值。
    pub test_url: String,
    pub timeout: Duration,
}

/// 转换器的能力声明（含 target 白名单，R04 C4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConverterCapabilities {
    pub id: ConverterId,
    pub supports_targets: Vec<TargetFormat>,
    pub supports_merge_sources: bool,
    pub version: Option<String>,
}

/// 转换器可达性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConverterHealth {
    Healthy { version: Option<String> },
    Unreachable { reason: String },
    Misconfigured { reason: String },
}

/// 日志级别。用于 `MihomoObserver::logs` 过滤与事件载荷。
///
/// 独立的枚举而非字符串：内核的日志级别是有限集合，用字符串会让
/// "传入 `warn` 还是 `warning`"变成运行期问题。adapter 负责与内核取值互转。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    /// 供 adapter 与内核取值互转，以及 CLI/TUI 展示。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}
```

**`ProxyGroupView` / `ProxyView` 刻意不叫 `ProxyGroup`/`Proxy`**：domain 里没有这两个概念，未来若要提升为业务实体（例如策略组选择持久化），届时再经 ADR 引入 domain，而不是让 adapter 形状悄悄变成领域模型。

---

### 2.7 Port 清单与"不建什么"

| Port | 方法数 | 为什么独立 |
|---|---|---|
| `MihomoController` | 10 | 命令面，有副作用，需串行化 |
| `MihomoObserver` | 3 | 只读流，失败语义不同（流断不影响生命周期） |
| `MihomoConnectionOps` | 3 | 连接明细含 uid/process 隐私字段，需独立授权 |
| `ProcessManager` | 4 | 生命周期权威 |
| `ServiceManager` | 3 | 仅探测 init 与 Agent 自身状态（不托管 mihomo） |
| `ConfigRepository` | 7 | 版本存储 |
| `ConfigValidator` | 4 | `-t` 有副作用且不完整，需可分离替换 |
| `SubscriptionConverter` | 3 | 可替换的转换后端 |
| `SubscriptionRepository` | 5 | 订阅存储 |
| `CapabilityProbe` | 2 | 无副作用能力探测 |
| `SecretStore` | 3 | 凭据生成/校验/轮换 |
| `AuditSink` | 2 | append-only 审计 |
| `JobRegistry` | 4 | 长操作可观测 |
| `EventPublisher` | 1 | 事件通知 |
| `KernelInstaller` | 5 | 内核更新链路 |

**明确不建的 Port**（避免 `SystemManager` 式巨型接口）：

```text
✗ FirewallManager / NetfilterManager —— MVP 不做 apply（REQ-NET-006 归 Later）
✗ NetworkProbe 与 CapabilityProbe 分开 —— 能力探测已覆盖只读网络检查
✗ Scheduler —— 调度是 Use Case 的编排职责，不是 Port
✗ LockManager —— 锁是进程内原语，不是外部能力
```

---

## 3. Job 与事件模型

### 3.1 Job（方案 A）

```rust
// application/src/jobs.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    ConfigActivate, ConfigRollback, SubscriptionUpdate, KernelUpdate,
    MihomoStart, MihomoStop, MihomoRestart, MihomoReload, DoctorRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobTarget { Instance(MihomoInstanceId), Config(ConfigVersionId), Subscription(SubscriptionId) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running { step: JobStep },
    Succeeded { summary: String },
    Failed { reason: String, degradation: Option<Degradation> },
}

/// 激活流程的步骤，顺序与 ADR-004 D3 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStep {
    Preflight, Syntax, Semantic, Persist, Activate, Reload, HealthCheck, Rollback,
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: JobKind,
    pub target: JobTarget,
    pub state: JobState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
```

### 3.2 事件

```rust
// application/src/events.rs
#[derive(Debug, Clone)]
pub enum DomainEvent {
    MihomoStatusChanged { instance: MihomoInstanceId, from: MihomoStatus, to: MihomoStatus },
    /// 日志已脱敏（REQ-SEC-006）
    MihomoLog { level: LogLevel, message: String },
    TrafficUpdated { up: u64, down: u64 },
    SubscriptionUpdated { id: SubscriptionId, outcome: UpdateOutcome },
    /// 在审计写入成功之后发布（见 §4.2）
    ConfigActivated { instance: MihomoInstanceId, version: ConfigVersionId },
    ConfigRolledBack { instance: MihomoInstanceId, to: ConfigVersionId },
    JobProgress { id: JobId, step: JobStep },
    JobFinished { id: JobId, state: JobState },
    CapabilityChanged { kind: CapabilityKind, status: CapabilityStatus },
}
```

**发布规则**：

1. **状态变更事件在状态确实变更之后发布**，且审计已落盘。
2. **不做事件溯源**：事件是通知，订阅者漏读后应重新查询，不重放。
3. **慢消费者**：`broadcast` 满时订阅者收到 `Lagged`，必须重新查询而非补全。这是 Port 文档中的契约。

---

## 4. 核心 Use Cases

### 4.1 `ActivateConfig` —— 唯一激活路径

```rust
// application/src/commands/activate_config.rs
pub struct ActivateConfig;

#[derive(Debug)]
pub struct ActivateConfigInput {
    /// 未校验候选；校验在 Use Case 内部完成。
    pub candidate: ConfigCandidate<Unvalidated>,
    /// 期望占用的端口，用于 L0 预检。
    pub desired_ports: Vec<u16>,
    /// 是否要求 geodata（影响 L0 与 L2 语义）。
    pub requires_geodata: bool,
    /// 回滚目标；通常为当前 active。
    pub rollback_to: Option<ConfigVersionId>,
}

#[derive(Debug)]
pub struct ActivateConfigOutput {
    pub activated: ConfigVersionId,
    pub reload: ReloadOutcome,
    pub health: HealthReport,
    pub report: ValidationReport,
    /// 是否发生了回滚
    pub rolled_back: bool,
    /// 回滚落点（观测所得，不是推断）
    pub restored_to: Option<ConfigVersionId>,
}

impl ActivateConfig {
    /// # Errors
    /// 仅在"连回滚都无法确立状态"时返回 Err。可预期的失败以
    /// `ApplicationError::ConfigActivationFailed` 或降级结果表达。
    pub async fn execute(
        ctx: &AppContext,
        input: ActivateConfigInput,
    ) -> Result<ActivateConfigOutput, ApplicationError>;
}
```

**编排序列（与 ADR-004 D3 逐条对应）**：

```text
1. 取 per-instance 锁（P2/P3：先释放 map 锁再 await 实例锁）
2. job = registry.create(ConfigActivate, target) + 发 JobProgress{Preflight}
3. L0 预检
     a. validator.observe_port_usage(desired_ports) → 冲突则失败（REQ-CONFIG-005）
     b. geodata 就绪判定（离线 + 需 geodata → 失败）
4. L1 语法： validator.validate_syntax(body)
5. L2 语义： validator.validate_semantic(body)
6. candidate.validate(report)?   ← typestate 门禁，未通过无法继续
7. 记住 previous = repo.active(instance)（用于回滚，但只作"意图"）
8. repo.next_sequence + repo.save(version, body)      （幂等）
9. repo.set_active(instance, id)                      （幂等、原子）
10. controller.reload(ReloadRequest::Payload(body))
       ⚠ 禁止 force；Applied ≠ 生效
11. controller.health_check()   ← L3：进程 / controller / 配置 / 端口
12. 成功 → audit.record(ConfigActivate, Success)
         → publish ConfigActivated（审计之后）
         → job.Succeeded
13. 失败 → 进入 §4.2 回滚
```

**为什么 `save` 在 `set_active` 之前**：先让版本落盘，再切指针。若 `set_active` 失败，磁盘上多了一个未被激活的版本（无害，可清理）；若顺序相反，指针会指向不存在的版本（有害）。

### 4.2 回滚：观测优先（P4）

```rust
async fn rollback(
    ctx: &AppContext,
    instance: &MihomoInstanceId,
    attempted: &ConfigVersionId,
    intent: Option<ConfigVersionId>,
) -> Result<RollbackOutcome, ApplicationError> {
    // 1) 重新观测真实状态，不信任意图（P4）
    let observed = ctx.configs.active(instance).await?;

    let Some(target) = intent.or(observed.as_ref().map(ConfigVersion::id).cloned()) else {
        // 无可用回滚目标：不停机，标记 Degraded，保留现场
        return Ok(RollbackOutcome::NoTarget);
    };

    // 2) 恢复指针
    if let Err(e) = ctx.configs.set_active(instance, &target).await {
        // 存储不可用：报告观测到的状态，而不是宣称成功
        return Ok(RollbackOutcome::Failed { observed: observed.map(|v| v.id()) });
    }

    // 3) **用 restart 落地，不是 reload**（R02 C5b：reload 无法脱离僵尸态）
    ctx.process.stop(&handle, STOP_TIMEOUT).await?;
    ctx.process.start(start_options).await?;

    // 4) 再观测确认
    match ctx.configs.active(instance).await {
        Ok(Some(v)) if v.id() == &target => Ok(RollbackOutcome::Restored { to: target.clone() }),
        Ok(other) => Ok(RollbackOutcome::Failed { observed: other.map(|v| v.id()) }),
        Err(_) => Ok(RollbackOutcome::Failed { observed: None }),
    }
}
```

**三条不可协商的规则**：

1. **回滚用 restart**，不用 reload。
2. **回滚后必须再观测**；不确认就报告成功是撒谎。
3. **回滚失败不停机**：标记 `Degraded`、保留现场、告警，绝不 stop Mihomo。

### 4.3 `RollbackConfig` —— 复用而非另写

```rust
impl RollbackConfig {
    pub async fn execute(ctx: &AppContext, input: RollbackConfigInput)
        -> Result<RollbackConfigOutput, ApplicationError>
    {
        // 读目标版本 → 读其 body → 构造 candidate → 走 ActivateConfig 全流程
        // 差异仅在于：Input.rollback_to = Some(当前 active)，且落地用 restart
        ActivateConfig::execute(ctx, ActivateConfigInput {
            candidate,
            rollback_to: Some(current_active_id),
            ..
        }).await
    }
}
```

> ADR-004 D5 要求回滚复用同一路径，否则回滚本身成为从未被验证的代码。

### 4.4 `UpdateSubscription` —— 保留旧配置的不变量

```rust
pub struct UpdateSubscription;

#[derive(Debug)]
pub struct UpdateSubscriptionOutput {
    pub outcome: UpdateOutcome,
    /// 无论成败都必须给出：失败时 = 当前仍激活的版本（REQ-SUB-003）
    pub active_config: Option<ConfigVersionId>,
}

impl UpdateSubscription {
    pub async fn execute(ctx: &AppContext, id: &SubscriptionId)
        -> Result<UpdateSubscriptionOutput, ApplicationError>
    {
        // 0. 去重：同一订阅不得并发更新（REQ-SUB-004）
        let Some(_guard) = ctx.guards.try_begin(id) else {
            return Ok(UpdateSubscriptionOutput { outcome: /* already running */, .. });
        };

        // 1. 载入订阅并检查 enabled
        // 2. converter.convert(...)  —— 空产物由 domain 拒绝（ConvertedProxies::new）
        // 3. 生成完整配置（domain::configuration::generate）：
        //      端口 / controller / secret / CORS / dns / proxy-groups / rules
        //      TUN 仅在 capabilities.can_enable_tun() 时写入
        // 4. ActivateConfig::execute(...)   ← 复用唯一激活路径
        // 5. 任一步失败：
        //      record_update(UpdateOutcome::Failed(PreservedActiveConfig(current)))
        //      ⚠ 绝不停止 Mihomo、绝不替换 active
    }
}
```

**必须用 `ActivateConfig` 而不是自己写 reload 的原因**：订阅更新是"生成配置"的一种来源，如果它绕过唯一激活路径，那么"失败保留旧配置"这条不变量就有两处实现，迟早分叉。

### 4.5 生命周期命令

```rust
pub struct StartMihomo;
impl StartMihomo {
    pub async fn execute(ctx: &AppContext) -> Result<StartOutcome, ApplicationError> {
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        // 1. 载入实例聚合（含状态机）
        // 2. instance.begin_start()  ← 决策：Spawn / AlreadyStarting / AlreadyRunning / BusyStopping
        //      Already* → 直接返回，不 spawn（REQ-MIHOMO-005）
        // 3. Spawn → instance.transition(Starting) + process.start(...)
        // 4. 就绪探测（基于 controller 可达或可解析日志行，禁止 sleep）
        // 5. 健康检查（含代理端口，L4）
        // 6. 成功 → Running；健康检查部分失败 → Degraded；失败 → Failed
        // 7. 持久化实例状态 + 审计 + 事件
    }
}
```

`StopMihomo` 用 `AllowedSignal::Term` → 超时后 `Kill`；`RestartMihomo` 复用 stop+start **而不是** `POST /restart`（那是 `syscall.Exec` 自替换，会绕过状态机）；`ReloadMihomo` 走 `ActivateConfig` 的 reload+health 段（不重新生成配置）。

### 4.6 Queries

```rust
pub struct GetMihomoStatus;   // → MihomoStatusView（实例状态 + 运行版本 + active 版本 + 健康摘要）
pub struct ListConfigs;       // → Vec<ConfigVersionSummary>（含 is_active、checksum 前缀、source）
pub struct ListSubscriptions; // → Vec<SubscriptionSummary>（含 last_update、is_due）
pub struct GetCapabilities;   // → SystemEnvironment + CapabilitySet
pub struct RunDoctor;         // → DoctorReport（domain::system::doctor）

// 全部只读：不取实例锁（避免读操作与长操作互斥），不加审计。
```

---

## 5. 并发模型

### 5.1 per-instance 锁（P3 的落地写法）

```rust
// application/src/locks.rs
#[derive(Default)]
pub struct InstanceLocks {
    inner: tokio::sync::Mutex<HashMap<MihomoInstanceId, Arc<tokio::sync::Mutex<()>>>>,
}

impl InstanceLocks {
    pub async fn acquire(&self, instance: &MihomoInstanceId) -> OwnedMutexGuard<()> {
        // ⚠ 关键：在短作用域内取出 Arc，**释放 map 锁之后**再 await 实例锁。
        // 若在持有 map 锁时 await，会造成跨实例的相互阻塞（P3 实证）。
        let entry = {
            let mut map = self.inner.lock().await;
            Arc::clone(map.entry(instance.clone()).or_default())
        };
        entry.lock_owned().await
    }
}
```

**谁必须取锁**：`StartMihomo`、`StopMihomo`、`RestartMihomo`、`ReloadMihomo`、`ActivateConfig`、`RollbackConfig`、`UpdateKernel`、`UpdateSubscription`（因为它会激活配置）。

**谁不取锁**：全部 Queries、`RunDoctor`、`GetCapabilities`（只读且无副作用）。

### 5.2 订阅去重

```rust
pub struct SubscriptionGuards { inner: std::sync::Mutex<HashSet<SubscriptionId>> }

impl SubscriptionGuards {
    /// 返回 None 表示已有进行中的更新 → 调用方**跳过**而非排队。
    pub fn try_begin(&self, id: &SubscriptionId) -> Option<SubscriptionGuard<'_>>;
}

impl Drop for SubscriptionGuard<'_> { /* 自动释放 */ }
```

> 选择"跳过"而不是"排队"：排队会积压，且下一次调度到来时仍会跳过。跳过 + 记录状态是可观测的。

---

## 6. 错误与降级

### 6.1 `ApplicationError`

```rust
// application/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Port(#[from] PortError),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("validation failed: {0}")]
    ValidationFailed(String),

    #[error("converter unavailable")]
    ConverterUnavailable,

    #[error("config activation failed: {0}")]
    ConfigActivationFailed(String),

    #[error("mihomo reload failed: {0}")]
    MihomoReloadFailed(String),

    #[error("rollback failed: {0}")]
    RollbackFailed(String),

    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),

    /// 操作未完成，但系统处于安全状态（**不是错误，是降级**）
    #[error("degraded: {0}")]
    DegradedPreservingActiveConfig(String),
}
```

### 6.2 降级 vs 错误（必须分开）

| 情形 | 表达 | 为什么 |
|---|---|---|
| 转换器不可达，更新失败，旧配置继续服务 | `UpdateOutcome::Failed`（**返回值**） | 这是正常产品状态，不是异常 |
| TUN 不可用 | `CapabilityStatus::Unavailable`（**值对象**） | 环境事实，非错误 |
| 未部署 Sub-Store | `PortError::NotImplemented` → 降级为 Native | 可预期的配置状态 |
| 激活失败且回滚失败，实例 Degraded | `Ok(... rolled_back=false, restored_to=None)` + `Degraded` 标记 | 已有明确降级语义，不应再抛错 |
| 存储彻底不可用，无法确立版本状态 | `Err(ApplicationError::RollbackFailed)` | 无法确定系统状态，必须上报 |

> **判据**：操作失败**但系统状态已确定且安全** → 降级（`Ok`）；**系统状态不确定** → 错误（`Err`）。

### 6.3 审计失败（按确认的方案 A）

```text
审计写入失败 → 不阻断主操作
             → 发布 job 警告 / 结构化日志 error
             → 在 `JobState::Succeeded` 上附加 `degradation: Some(AuditUnavailable)`

不阻断的理由：审计是事后追溯；特权操作本身已经过认证与 Use Case 校验。
可用性优先：让审计故障导致"无法回滚配置"是不可接受的耦合。
```

`Degradation` 枚举在任务结果中显式携带，使"操作成功但审计缺失"可被上层观测到，而不是静默。

---

## 7. 测试策略

### 7.1 测试替身

用 `tokio::sync::Mutex` + 内存 `HashMap` 实现全部 Port 的 `InMemory*` 假件，放在 `application/src/test_support/`（`#[cfg(test)]` 或 `feature = "test-support"` 供 infrastructure 集成复用）。

**关键：假件必须能注入故障**，否则测不出回滚：

```rust
pub struct FakeConfigRepository {
    active: Mutex<Option<ConfigVersionId>>,
    /// 让第 N 次 set_active 失败
    fail_set_active_at: Option<usize>,
    calls: Mutex<Vec<String>>,
}
```

### 7.2 必须覆盖的失败路径（`AGENTS.md` Testing 要求逐条对应）

| # | 场景 | 断言 |
|---|---|---|
| T1 | 订阅转换失败 | `active` 版本不变；`UpdateOutcome::Failed(PreservedActiveConfig)` |
| T2 | 转换成功但输出为空/零节点 | 同 T1（domain 层拒绝） |
| T3 | L0 预检端口冲突 | 未调用 `reload`；`active` 不变 |
| T4 | L1 语法失败 | 未调用 `reload`；`active` 不变 |
| T5 | L2 语义失败 | 未调用 `reload`；`active` 不变 |
| T6 | `set_active` 失败 | 回滚到旧版本；`rolled_back=true` |
| T7 | `reload` 返回 `Rejected` | 触发回滚（restart 路径被调用） |
| T8 | reload 成功但健康检查失败（端口未监听） | 触发回滚；断言调用的是 `process.stop/start` 而**不是**第二次 `reload` |
| T9 | 回滚成功 | 最终 `active` = 旧版本；审计含 `ConfigRollback` |
| T10 | 回滚失败（`set_active` 持续失败） | `Err` 或 `RollbackFailed` + `restored_to=None`；**Mihomo 未被 stop** |
| T11 | 并发两次 `StartMihomo` | 只调用一次 `process.start` |
| T12 | `Starting` 期间再次 start | 返回 `AlreadyStarting`，不 spawn |
| T13 | 非法生命周期转换 | `DomainError::InvalidTransition`，状态不变 |
| T14 | 同一订阅并发两次更新 | 第二次跳过；`convert` 只调用一次 |
| T15 | `update:force` 相关 | 编译期不存在该变体；断言 `ReloadRequest` 无 force |
| T16 | 审计失败 | 操作仍成功；结果携带 `Degradation::AuditUnavailable` |
| T17 | 审计写入在事件发布之前 | 断言事件序列中 `ConfigActivated` 晚于 `audit.record` |
| T18 | 离线 + 需 geodata | L0 失败，未触及 `reload` |
| T19 | `UpdateSubscription` 走完整激活路径 | 断言 `ConfigValidator` 被调用（未绕过） |
| T20 | Queries 不取锁 | 长操作持锁期间 `GetMihomoStatus` 仍返回 |

### 7.3 验证命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

外加架构守卫（新增 `crates/application/tests/architecture.rs`）：

```text
- application 不得依赖 reqwest / sqlx / axum / systemd / 文件系统 / 进程
- application 不得依赖 infrastructure / interfaces / bootstrap
- application 的公开签名中不得出现 adapter 具名类型（文本扫描 reqwest::/sqlx::）
```

---

## 8. 被否决的替代方案

| 方案 | 否决理由 |
|---|---|
| Use Case 阻塞到完成（方案 B，用户已选 A） | Web 长请求易超时；job 概念会被复制到三个 interfaces 中，违反 REQ-ARCH-001 |
| 锁放在 adapter 内 | adapter 每次调用新建 → 锁形同虚设；两次 start 仍会并发 |
| 订阅自己去写 reload（不复用 `ActivateConfig`） | "失败保留旧配置"会有两处实现，必然分叉 |
| 回滚信任传入的 `previous` 而不重新观测 | P4 实证：存储全挂时会报告错误的"已恢复"状态 |
| 把 `ActiveConfig` 当作全局可变单例 | 违反多实例预留（`MihomoInstanceId` 从第一天存在） |
| 用 `Err` 表达降级 | 会诱导 interfaces 把正常产品状态当异常上报 |
| 引入 `tower`/中间件做重试 | MVP 无此需求；重试策略属 adapter |
| 为 Port 引入泛型参数 | 污染全部 Use Case 签名，单机无收益 |

---

## 9. 已知风险与变形

### 9.1 premise collapse（最重要）

**设计假设**：`ConfigRepository::save` 与 `set_active` 是**幂等**的。

**若不成立**：一次"`set_active` 成功但响应丢失"的重试会写入第二个 active 指针，导致两个版本同时被标记激活；回滚会基于不确定的状态做决策。

**已做的变形**：
1. 幂等要求**写进 Port 契约文档**，并由 adapter 负责实现（如 `set_active` 用原子 rename 覆盖同一指针文件）。
2. `ActivateConfig` 在回滚前**重新 `active()` 观测**，不假设自己知道状态（P4 已验证该路径的行为）。
3. 回滚失败时上报**观测值**，而不是宣称成功。

这使设计在幂等性被破坏时**降级为可观测的失败**，而不是静默的数据损坏。

### 9.2 其他风险

| 风险 | 缓解 |
|---|---|
| 长操作持锁期间 Queries 被阻塞 | Queries 不取实例锁（T20 验证） |
| `broadcast` 满导致事件丢失 | Port 契约规定订阅者收到 `Lagged` 后重新查询，不补全 |
| 健康检查误判（僵尸态） | L4 必查代理端口；R01/R02 实测支撑 |
| 审计缺失不可见 | `JobState::Succeeded { degradation }` 显式携带 |
| 进程重启的窗口期 | 回滚路径允许短暂不可用（与"完全不恢复"相比是净收益）；`Degraded` 状态对外可见 |

---

## 10. 实现顺序（每个 commit 独立可编译）

```text
1. feat(application): add PortError and ApplicationError taxonomy
2. feat(application): define mihomo control and process ports
3. feat(application): define config and validator ports
4. feat(application): define subscription, system, and security ports
5. feat(application): add job model, events, and locks
6. feat(application): add ActivateConfig with observation-based rollback
7. feat(application): add RollbackConfig reusing the activation path
8. feat(application): add lifecycle commands (start/stop/restart/reload)
9. feat(application): add UpdateSubscription preserving active config
10. feat(application): add subscription CRUD commands
11. feat(application): add read-only queries
12. test(application): add test_support fakes with fault injection
13. test(application): add architecture guards
```

第 1–5 个 commit 后 crate 可编译（Port 与骨架）；第 6–11 后全部 Use Case 可用；第 12 之后 T1–T20 可跑。

---

## 11. 未知项（defer，附 owner）

| 项 | 原因 | Owner / 时机 |
|---|---|---|
| `JobRegistry` 持久化 | MVP 内存即可；进程重启后 job 丢失可接受 | 实现阶段 |
| 事件容量与 `Lagged` 重查的具体上限 | 需真实负载观测 | 实现阶段 |
| `HealthReport` 的确切字段 | 依赖 R01 的 `/configs` 字段限制（33 字段）与 L4 端口探测设计 | 实现阶段（infrastructure 定形后） |
| `KernelInstaller::verify` 的校验强度 | 依赖 Q005/Q015（上游是否提供签名） | 打包前 |
| 是否把 `InstanceLocks` 提升为 bootstrap 单例以支持多实例 | MVP 单实例；类型已支持 | 多实例里程碑 |

---

## 12. 实现状态（Application 层已落地）

```text
阶段 1–5 与 6–7（ports / jobs / locks / ActivateConfig / RollbackConfig）：已实现
```

**产物**：`crates/application/`，`proxy-application` crate。

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --check` | PASS |
| 静态检查 | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| 测试 | `cargo test --workspace --all-features` | **243 passed**（domain 146 + application 97） |
| 构建 | `cargo build --workspace --all-features` | PASS |
| 依赖纯度 | `cargo tree -p proxy-application` | 仅 `proxy-domain` + `thiserror` + `async-trait` + `tokio(sync)` + `futures-core` |

**测试分布**：ports 单元测试 65、`activation.rs` 18、`concurrency.rs` 8、architecture 守卫 6。

### 12.1 与本文档设计的差异（实现期修正）

| 项 | 文档原设计 | 实现 | 原因 |
|---|---|---|---|
| 校验失败的返回值 | 未明确 | **不是 Err**，而是 `Ok { succeeded: false }` | 校验拒绝不改动任何状态，没有状态需要恢复。返回 `Err` 会迫使调用方为最常见的输入错误写特例（由 5 个失败测试暴露） |
| `AppContext` 进程句柄 | 未列出 | 新增 `ProcessState` 字段 | 内核不 daemonize、不写 pid 文件，Agent 内存中的 handle 是唯一真相；回滚需要它来 restart |
| `ConfigValidator::preflight` | 接收候选配置 | 接收 `&ConfigBody` + `&PreflightContext` | 避免 Port 依赖 domain 的 typestate 类型，使 adapter 无法"看到"校验状态 |
| `test_support` | 文档写 `#[cfg(test)]` 或 feature | **feature 门控** `test-support`（默认关闭） | 集成测试在其他 crate 中，`#[cfg(test)]` 不可见；同时保证 shipping 构建不含测试脚手架 |
| `PortError` 可克隆性 | 未考虑 | 不可 `Clone`；测试替身存普通数据、调用时构造错误 | `Box<dyn Error>` 不实现 `Clone`；替身改存 `ReloadOutcome`/`HealthBehaviour` 枚举 |
| 空流替身 | 计划用 `futures-util` | 手写 `EmptyStream<T>` | 避免为测试替身引入真实依赖（`futures-core` 不提供 `empty()`） |

### 12.2 关键不变量与对应测试

| 不变量 | 测试 |
|---|---|
| 失败/拒绝的校验不触碰内核 | `preflight_failure_never_reaches_the_kernel`、`syntax_…`、`semantic_…` |
| 端口冲突在预检拦截 | `port_conflict_is_caught_by_the_preflight_layer` |
| `save` 先于 `set_active` | `successful_activation_persists_activates_and_reports_health` |
| 审计先于 `ConfigActivated` 事件 | `audit_is_written_before_the_activation_event` |
| reload 用 payload 而非 path | `activation_searches_the_kernel_by_payload_not_path` |
| reload 被拒 → 回滚 | `rejected_reload_triggers_recovery` |
| 僵尸态（controller 通、端口不通）→ 回滚 | `degraded_health_triggers_recovery` |
| 回滚用 restart 而非 reload | `recovery_restarts_rather_than_reloading` |
| 回滚不停机 | `recovery_never_stops_the_kernel_permanently` |
| 无法确认时报告观测值 | `unconfirmable_recovery_reports_observed_state` |
| 审计失败不阻断、但可见 | `audit_failure_degrades_without_failing_the_operation` |
| 并发激活被串行化 | `concurrent_activations_do_not_interleave` |
| 失败/成功都释放实例锁 | `instance_lock_is_released_after_*` |
| 同一订阅并发更新被抑制 | `concurrent_subscription_updates_are_suppressed` |
| 每个 job 都到达终态 | `jobs_always_reach_a_terminal_state` |

### 12.3 已实现的 Use Case 全清单

| 模块 | Use Case | 状态 |
|---|---|---|
| `commands/activate_config.rs` | `ActivateConfig` | ✅ |
| `commands/rollback_config.rs` | `RollbackConfig` | ✅ |
| `commands/lifecycle.rs` | `StartMihomo`、`StopMihomo`、`RestartMihomo`、`ReloadMihomo`、`signal_kernel` | ✅ |
| `commands/update_subscription.rs` | `UpdateSubscription`、`SubscriptionCrud`（save/delete/list/get/test） | ✅ |
| `queries.rs` | `GetMihomoStatus`、`ListConfigs`、`ListSubscriptions`、`GetCapabilities`、`RunDoctor`、`ListJobs`、`ListAuditEntries` | ✅ |

**测试补充（本轮新增）**：`lifecycle.rs` 12 个、`subscription.rs` 23 个。

### 12.4 本轮实现期修正

| 项 | 问题 | 处置 |
|---|---|---|
| `RestartMihomo` 自死锁 | 取实例锁后又调用 `StartMihomo::execute`（同锁不可重入） | 拆出 `start_locked` 内部助手，并**显式修正**设计文档 §3 未说明的锁非重入约束（写进 `commands/mod.rs` 模块文档） |
| 失败路径不发布事件 | 转换失败/校验失败走 early return，跳过 `SubscriptionUpdated` 事件 → 订阅连续失败多日时面板仍显示"从未更新" | 统一为 `fail_and_announce`，**所有**失败路径都记录并广播（由测试 `update_publishes_an_event_for_both_outcomes` 暴露） |
| `Subscription` 占位对象 | 早期版本为"订阅不存在"构造占位订阅，内部用 `unreachable!()` | 改为 `Option<&mut Subscription>`：失败可先于拥有订阅发生，不需要伪造对象，也不引入 panic |
| 就绪判定 | 文档未明确 | 轮询 `health_check` 至截止（默认 30s），**不 sleep 固定时长**；内核绑定失败不致命，代理端口未监听时判 `Degraded` 而非 `Running` |
| 内核重启方式 | 文档未明确 | stop-then-start，**不使用**内核的 self-restart（原地替换进程映像，Agent 无法观测且状态丢失） |
| `ReloadMihomo` 语义 | 文档未明确 | 只重载**已激活**版本，不生成/校验新配置；变更配置走 `ActivateConfig` |
| Queries 取锁 | 文档 §5.1 已规定不取锁 | 实现确认；新增测试 `read_only_queries_are_not_blocked_by_an_activation` |

---

## 13. 最小 Bootstrap 与一个真实缺陷的发现

### 13.1 产出

| 项 | 内容 |
|---|---|
| `crates/bootstrap` | `AdapterFactory`（适配器供给）、`Bootstrap`（组装 + 选择决策）、`RuntimeConfig` |
| `crates/application` 新增 | `AppContextBuilder`（19 字段分步装配）、`InstanceRepository` Port、`FakeInstanceRepository` |
| feature 拆分 | `test-doubles`（仅装配所需的最小替身）/ `test-support`（含故障注入） |
| 测试 | bootstrap 18 个（含 9 个 smoke）；application 138 个 |

### 13.2 ⚠️ 发现并修复了一个真实的并发缺陷

**症状**：4 个并发 `StartMihomo` **全部 spawn 了进程**（4 次 `process.start`），而 per-instance 锁本应保证只有一次。

**根因**：`StartMihomo::execute` 的签名是

```rust
pub async fn execute(ctx: &AppContext, instance: &mut MihomoInstance, now: Timestamp)
```

聚合由**调用方持有**。于是：

```text
任务 A: 取锁 → 看到自己的副本 Stopped → 决定 Spawn → 放锁
任务 B: 取锁 → 看到自己的副本 Stopped → 决定 Spawn → 放锁   ← 副本从未更新
```

**锁串行化了 `process.start` 的调用，但没有串行化决策本身** —— 因为决策依据是每个调用方各自的副本。

**为什么这是设计文档的实现偏差**：设计 §4.5 的第 1 步明确写了「**载入实例聚合**（含状态机）」，且原签名是 `execute(ctx)`（无 `instance` 参数）。我在实现时为了方便测试引入了 `&mut MihomoInstance` 参数，把共享状态变成了调用方局部状态。

**修复**：

1. 新增 `InstanceRepository` Port（`load` / `save` / `list`），状态存在所有调用方都能看到的地方。
2. 生命周期命令改为在**取锁之后** `load_instance()`，在**放锁之前** `save_instance()`。
3. `Starting` 状态在 spawn **之前**持久化，使并发请求看到"启动中"而非"已停止"。

**验证**：`concurrent_starts_on_a_composed_context_spawn_once` 从「4 tasks spawned」变为通过。

**教训**：这个缺陷**只有真正的端到端组装 + 并发测试才能发现** —— 原有的 `lifecycle.rs` 测试每个用例各持一个 `instance`，串行调用，永远看不到问题。这正是做最小 bootstrap 的价值。

### 13.3 为什么 `AppContextBuilder::build()` 返回 `Result`

新增 `instances` 字段后，全部 18 个 activation 测试立刻失败，报错是：

```text
every dependency is supplied above: MissingDependency("instances")
```

这正是不返回 `Result` 时**得不到**的信息 —— 编译器只会给出一片 "missing field" 错误。分步装配 + 具名缺失依赖，让"You forgot X"变成一条可读的报告。

### 13.4 最小 Bootstrap 验证了什么

| # | 属性 | 测试 |
|---|---|---|
| S1 | 16 个依赖全部可装配，`Arc<dyn Port>` 满足 `Send + Sync + 'static` | `in_memory_composition_wires_every_dependency` |
| S2 | 组装产物能跑通真实 Use Case（activation / lifecycle / query） | `assembled_context_runs_*` |
| S3 | 组装产物可跨 task 共享 | `assembled_context_is_shareable_across_tasks` |
| S4 | 并发生命周期只 spawn 一次（**缺陷回归测试**） | `concurrent_starts_on_a_composed_context_spawn_once` |
| S5 | 环境探测失败不产出上下文 | `environment_probe_failure_is_reported` |
| S6 | 无 init system 是合法部署 | `composition_succeeds_without_an_init_system` |

### 13.5 尚未实现

`Bootstrap::build` 的**真实** `AdapterFactory` 实现（需要 Infrastructure 存在）。当前 `InMemoryFactory` 用于验证装配；换入真实 adapter 不改 `Bootstrap` 一行代码 —— 这是 `AdapterFactory` 抽象的目的。
