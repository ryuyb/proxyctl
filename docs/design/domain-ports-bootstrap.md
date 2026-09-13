# Domain / Application Ports / Bootstrap — Rust 类型与 trait 详细设计

> 状态：Accepted（设计冻结，待实现）
> 日期：2026-09-12
> 依据：`docs/research/RESEARCH-SUMMARY.md`、`docs/adr/ADR-001..006`、`docs/research/requirements.md`、`AGENTS.md`
> 范围：`crates/domain`、`crates/application`（Ports）、`crates/bootstrap`
> 非范围：`infrastructure`（仅列契约）、`interfaces`（REST/CLI/TUI）

---

## 0. 设计原则与已实证的工具链约束

### 0.1 三条硬约束

| # | 约束 | 来源 | 后果 |
|---|---|---|---|
| 1 | Domain 不得依赖 `tokio`/`reqwest`/`sqlx`/axum/文件系统/进程 | `AGENTS.md` Forbidden | Domain 全同步；时间与 ID 由外部注入 |
| 2 | Port 必须可 `dyn` 装配 | 本节 0.2 实证 | 必须使用 `#[async_trait]` |
| 3 | 失败不得破坏当前可用配置 | 核心不变量 | 用类型把关（见 0.3） |

### 0.2 `async_trait` 是硬约束，不是风格偏好（已实证）

本机 rustc `1.100.0-nightly` 实测：

```rust
// 探针 1：native async fn in trait + Box<dyn>
pub trait Port { async fn call(&self) -> u32; }
pub fn make() -> Box<dyn Port> { Box::new(Adapter) }
// → error[E0038]: the trait `Port` is not dyn compatible
```

```rust
// 探针 2：#[async_trait]
#[async_trait::async_trait]
pub trait Port: Send + Sync { async fn call(&self) -> u32; }
pub fn make() -> Box<dyn Port> { Box::new(Adapter) }
// → Finished `dev` profile（编译通过）
```

**结论**：Phase 0 的"外部集成必须可替换"要求 bootstrap 能在运行时装配 `Arc<dyn Port>`，而 native AFIT 目前不支持该用法 → 全部 Port 使用 `#[async_trait]`。

### 0.3 用类型编码不变量（本设计的核心手法）

Phase 0 的多数不变量是"某操作在某个前置条件下才允许"。这类约束用注释和 review 保证会漂移，因此本设计把它们编码进类型系统，并已实证两个方向：

```rust
// 正向：正确用法编译通过
let c = ConfigCandidate::new(body);
let v = c.validate(true)?;      // → ConfigCandidate<Validated>
v.activate();

// 负向：跳过校验直接激活 —— 编译失败
let c = ConfigCandidate::new(body);
c.activate();
// → error[E0599]: no method named `activate` found for
//   struct `ConfigCandidate<Unvalidated>`
```

映射到需求：

| 不变量 | 编码方式 | 需求 |
|---|---|---|
| 未校验配置不得激活 | typestate `ConfigCandidate<Unvalidated → Validated>` | REQ-CONFIG-005/006 |
| 能力状态不是 bool | `CapabilityStatus` 五值 enum | REQ-LXC-002 |
| 配置版本不可变 | 字段私有 + 无 `&mut self` + 无 `set_*` | REQ-CONFIG-007 |
| 非法生命周期转换被拒绝 | `transition()` 返回 `Result`，无裸 `set_status` | REQ-MIHOMO-005 |
| controller 不得绑 `0.0.0.0` | `ControllerEndpoint` 构造器校验（见 3.1） | REQ-SEC-001/003/012 |
| 禁止 `force=true` | `ReloadRequest` 枚举中**不存在** force 变体 | REQ-CONFIG-011 |

---

## 1. 依赖与 crate 边界

```text
crates/
├── domain/          deps: thiserror, (可选) time/chrono —— 无 tokio/async
├── application/     deps: domain, thiserror, async-trait, tokio(sync 类型)
├── infrastructure/  deps: application, domain, reqwest, sqlx, tokio
├── interfaces/      deps: application, domain, axum, clap, ratatui
└── bootstrap/       deps: 全部 —— 唯一组装点
```

```text
domain        ← 不依赖任何层
application   → domain
infrastructure → application, domain
interfaces    → application, domain
bootstrap     → 全部
```

**Domain 的领域内表示 vs 边界表示**：Domain 只依赖 `thiserror`，**不依赖 chrono/serde**。时间用自有值对象 `Timestamp`（内含 `i64` Unix 秒），URL 用自有 `SubscriptionUrl`（内含 `String` + 校验后的 host），UUID 用自有 newtype 包装 `String`。这样 Domain 对序列化与时间库完全免疫，adapter 负责转换。若后续认为自研过重，可放开 `chrono`——这是一个可逆决定，不影响 Port 形状。

---

## 2. Domain 层

### 2.1 通用内核类型（`domain/src/shared/`）

```rust
// domain/src/shared/id.rs
macro_rules! define_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// 从已校验的字符串构造。空字符串是唯一的非法输入。
            pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
                let raw = raw.into();
                if raw.trim().is_empty() {
                    return Err(DomainError::invariant(concat!($prefix, " must not be empty")));
                }
                Ok(Self(raw))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
    };
}

define_id!(MihomoInstanceId, "mihomo instance id");
define_id!(ConfigVersionId, "config version id");
define_id!(SubscriptionId, "subscription id");
define_id!(JobId, "job id");
define_id!(ConverterId, "converter id");
define_id!(AuditEntryId, "audit entry id");
```

> **为什么 newtype 而不是裸 `String`**：避免 `activate(config_id)` 与 `activate(subscription_id)` 互换而编译器不报错。这是本项目最容易犯的错位 bug。

```rust
// domain/src/shared/time.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(i64);           // Unix 秒，UTC

impl Timestamp {
    pub const fn from_unix_seconds(secs: i64) -> Self { Self(secs) }
    pub const fn as_unix_seconds(self) -> i64 { self.0 }
    pub fn elapsed_since(self, earlier: Timestamp) -> Duration { /* 纯计算 */ }
}

/// 时间由外部注入，Domain 不调用系统时钟（保证可测试与纯度）
pub trait Clock { fn now(&self) -> Timestamp; }
```

```rust
// domain/src/shared/error.rs
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("invariant violated: {0}")]
    Invariant(String),

    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition { from: &'static str, to: &'static str },

    #[error("validation failed: {0}")]
    Validation(ValidationFailure),

    #[error("capability not available: {0}")]
    CapabilityUnavailable(CapabilityKind),
}

impl DomainError {
    pub fn invariant(msg: impl Into<String>) -> Self { Self::Invariant(msg.into()) }
}
```

### 2.2 `domain/src/system/` — 能力与运行环境

```rust
/// 五值能力状态。**禁止用 bool 表达兼容性**（REQ-LXC-002）。
/// `Misconfigured` 是本项目的核心语义：前置条件具备但实际不可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    Unavailable,
    Misconfigured,
    Unknown,
}

impl CapabilityStatus {
    pub fn is_usable(self) -> bool { matches!(self, Self::Supported) }
    /// 降级决策：只有 Supported 才允许启用依赖该能力的特性
    pub fn permits(self, feature: Feature) -> bool { ... }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    TunDevice,     // /dev/net/tun 存在 + open + ioctl(TUNSETIFF) 成功
    NetAdmin,      // CAP_NET_ADMIN
    NetRaw,
    NfTables,
    IptablesNft,
    IptablesLegacy,
    PolicyRouting, // ip rule / fwmark
    SysctlWritable,// 关键：TProxy 的隐藏门槛（R11 C5）
    Systemd,
    DnsResolved,
}

/// 单项能力 + 证据。探测结果必须可追溯到命令/系统调用（REQ-LXC-007）。
#[derive(Debug, Clone)]
pub struct Capability {
    kind: CapabilityKind,
    status: CapabilityStatus,
    evidence: CapabilityEvidence,
}

#[derive(Debug, Clone)]
pub struct CapabilityEvidence {
    pub probe: String,          // 例如 "ioctl(TUNSETIFF)"
    pub detail: String,         // 例如 "EPERM" / "not present"
    pub observed_at: Timestamp,
}

impl Capability {
    pub fn new(kind: CapabilityKind, status: CapabilityStatus, evidence: CapabilityEvidence) -> Self;
    pub fn kind(&self) -> CapabilityKind;
    pub fn status(&self) -> CapabilityStatus;
    pub fn evidence(&self) -> &CapabilityEvidence;
}
```

**TUN 的领域不变量（把 R10 的实测结论固化成规则）**：

```rust
/// TUN 可用 = 设备可打开 ∩ CAP_NET_ADMIN ∩ ioctl(TUNSETIFF) 成功。
/// 实测：设备存在且 open() 成功仍可能 EPERM → 绝不接受"文件存在"作为充分条件。
pub fn evaluate_tun(
    device_present: bool,
    device_openable: bool,
    net_admin: CapabilityStatus,
    ioctl_tunsetiff: ProbeResult,
) -> (CapabilityStatus, CapabilityEvidence) {
    if !device_present { return (CapabilityStatus::Unavailable, ev("device missing")); }
    if !device_openable { return (CapabilityStatus::Misconfigured, ev("open() denied")); }
    if net_admin != CapabilityStatus::Supported {
        // 设备在、能打开，但缺 CAP_NET_ADMIN → 正是 Misconfigured 而非 Unavailable
        return (CapabilityStatus::Misconfigured, ev("ioctl(TUNSETIFF) EPERM: missing CAP_NET_ADMIN"));
    }
    match ioctl_tunsetiff {
        ProbeResult::Ok      => (CapabilityStatus::Supported, ev("ioctl ok")),
        ProbeResult::Eperm   => (CapabilityStatus::Misconfigured, ev("EPERM")),
        ProbeResult::Unknown => (CapabilityStatus::Unknown, ev("probe failed")),
    }
}
```

```rust
// domain/src/system/environment.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystem { Debian, Ubuntu, OtherLinux, Unknown }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture { X86_64, Aarch64, Other }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitSystem { Systemd, OpenRc, None, Unknown }

/// 不得假设 LXC == TUN 可用（AGENTS.md）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerEnvironment {
    BareMetal,
    VirtualMachine,
    Lxc { privileged: Privilegedness },
    Docker,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilegedness { Privileged, Unprivileged, Unknown }

#[derive(Debug, Clone)]
pub struct SystemEnvironment {
    os: OperatingSystem,
    os_version: Option<String>,
    arch: Architecture,
    kernel: Option<String>,
    init: InitSystem,
    container: ContainerEnvironment,
    capabilities: CapabilitySet,
}

impl SystemEnvironment {
    pub fn cap(&self, kind: CapabilityKind) -> &Capability;
    pub fn tun(&self) -> CapabilityStatus;
    pub fn supports_basic_proxy(&self) -> bool;   // 恒为 true：Mixed 端口不依赖特权
}
```

```rust
// domain/src/system/capability_set.rs
/// 能力集合：无论环境如何，查询永不 panic（未知能力返回 Unknown）
#[derive(Debug, Clone)]
pub struct CapabilitySet(Vec<Capability>);

impl CapabilitySet {
    pub fn status(&self, kind: CapabilityKind) -> CapabilityStatus;  // 缺失 → Unknown
    pub fn iter(&self) -> impl Iterator<Item = &Capability>;
    /// 降级决策集中在此，避免能力判断散落各处
    pub fn can_enable_tun(&self) -> bool;
    pub fn can_apply_transparent_proxy(&self) -> bool;  // MVP 恒 false（Detection Only）
}
```

```rust
// domain/src/system/doctor.rs
#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub environment: SystemEnvironment,
    pub mihomo: MihomoDoctorSection,
    pub network: NetworkDoctorSection,
    pub runtime: RuntimeDoctorSection,
    pub conclusion: DoctorConclusion,
}

/// 产品级承诺：降级状态是合法结果，不是错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorConclusion {
    pub basic_proxy: CapabilityStatus,
    pub tun: CapabilityStatus,
    pub transparent_proxy: CapabilityStatus,
}
```

### 2.3 `domain/src/mihomo/` — 实例与生命周期

```rust
// domain/src/mihomo/version.rs
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MihomoVersion(String);   // 例如 "v1.19.30"

impl MihomoVersion {
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelFlavor { Meta, Unknown }   // /version 的 meta:true（R01 C1）

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MihomoBuild {
    pub version: MihomoVersion,
    pub flavor: KernelFlavor,
    pub raw: String,      // 上游原始串，用于诊断
}
```

```rust
// domain/src/mihomo/status.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MihomoStatus {
    Stopped,
    Starting,
    Running,
    /// 必需状态：实测反复出现"进程活着但某层坏了"（R03 C3）
    Degraded,
    Stopping,
    Failed,
}

impl MihomoStatus {
    /// 非法转换必须被拒绝（REQ-MIHOMO-005）
    pub fn can_transition_to(self, next: MihomoStatus) -> bool;
}

#[derive(Debug, Clone)]
pub struct MihomoInstance {
    id: MihomoInstanceId,
    name: String,
    status: MihomoStatus,
    active_config: Option<ConfigVersionId>,
    running_version: Option<MihomoBuild>,
    last_failure: Option<FailureRecord>,
}

impl MihomoInstance {
    pub fn new(id: MihomoInstanceId, name: String) -> Result<Self, DomainError>;
    pub fn id(&self) -> &MihomoInstanceId;
    pub fn status(&self) -> MihomoStatus;

    /// 唯一的状态变更入口。非法转换返回 Err，调用方不得绕过。
    pub fn transition(&mut self, next: MihomoStatus, at: Timestamp) -> Result<(), DomainError>;

    /// 幂等保护：Starting → Starting 不得触发二次 spawn（REQ-MIHOMO-005）
    pub fn begin_start(&mut self, at: Timestamp) -> Result<StartDecision, DomainError>;

    pub fn mark_config_active(&mut self, id: ConfigVersionId);
    pub fn note_failure(&mut self, failure: FailureRecord);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDecision { Spawn, AlreadyStarting, AlreadyRunning }
```

> **注意**：`MihomoInstance` 没有 `set_status`。所有变更走 `transition` / `begin_start`，非法路径返回 `DomainError::InvalidTransition`。

### 2.4 `domain/src/configuration/` — 不可变版本与校验门禁

```rust
// domain/src/configuration/version.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChecksum(String);   // 形如 "sha256:<hex>"

impl ConfigChecksum {
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError>;
    pub fn as_str(&self) -> &str;
}

/// 配置来源，用于追溯（REQ-CONFIG-008）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Subscription(SubscriptionId),
    Manual,
    Imported,
    Generated,
    Rollback { from: ConfigVersionId },
}

/// 不可变配置版本。字段全私有、无 &mut self、无 setter（REQ-CONFIG-007）。
/// 回滚 = 激活旧版本，而不是改写历史。
#[derive(Debug, Clone)]
pub struct ConfigVersion {
    id: ConfigVersionId,
    instance_id: MihomoInstanceId,
    sequence: u64,               // 单调递增，用于 "vNNN"
    source: ConfigSource,
    checksum: ConfigChecksum,
    created_at: Timestamp,
    activated_at: Option<Timestamp>,
}

impl ConfigVersion {
    /// 只能由 Domain 内部流程创建（构造函数不对外公开字段名）
    pub fn record(
        id: ConfigVersionId,
        instance_id: MihomoInstanceId,
        sequence: u64,
        source: ConfigSource,
        checksum: ConfigChecksum,
        created_at: Timestamp,
    ) -> Self;

    pub fn id(&self) -> &ConfigVersionId;
    pub fn checksum(&self) -> &ConfigChecksum;
    pub fn source(&self) -> &ConfigSource;
    pub fn is_active(&self) -> bool;
    /// 返回新值而不是原地修改（不可变语义）
    pub fn activated(self, at: Timestamp) -> Self;
}
```

**校验门禁（typestate，已在 0.3 实证）**：

```rust
// domain/src/configuration/validation.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationLevel { Syntax, Semantic, Runtime }

#[derive(Debug, Clone)]
pub struct ValidationFailure {
    pub level: ValidationLevel,
    pub message: String,
    pub evidence: Option<String>,   // 例如 mihomo -t 的输出
}

/// 四层校验结果，含 Phase 0 新增的 L0 资源预检（ADR-004 D2）
#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub syntax: LevelOutcome,
    pub resource_preflight: LevelOutcome,
    pub semantic: LevelOutcome,
    pub runtime: LevelOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelOutcome { Passed, Failed(String), Skipped(String) }

impl ValidationReport {
    pub fn is_acceptable(&self) -> bool;
    pub fn first_failure(&self) -> Option<&str>;
}

/// 未校验的候选配置
pub struct Unvalidated;
/// 已通过 L0+L1+L2 的候选配置
pub struct Validated;

/// 只有 Validated 才能激活 —— 类型级门禁
#[derive(Debug)]
pub struct ConfigCandidate<State> {
    instance_id: MihomoInstanceId,
    source: ConfigSource,
    body: ConfigBody,
    checksum: ConfigChecksum,
    report: Option<ValidationReport>,
    _state: std::marker::PhantomData<State>,
}

impl ConfigCandidate<Unvalidated> {
    pub fn new(
        instance_id: MihomoInstanceId,
        source: ConfigSource,
        body: ConfigBody,
    ) -> Result<Self, DomainError>;

    /// 校验通过 → 升级为 Validated；失败 → 返回错误并丢弃候选体
    pub fn validate(self, report: ValidationReport) -> Result<ConfigCandidate<Validated>, DomainError>;
}

impl ConfigCandidate<Validated> {
    pub fn report(&self) -> &ValidationReport;
    pub fn body(&self) -> &ConfigBody;
    pub fn checksum(&self) -> &ConfigChecksum;
}

/// 配置正文：Domain 不解释 YAML，只承载不透明字节与校验和
#[derive(Debug, Clone)]
pub struct ConfigBody(String);

impl ConfigBody {
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError>;  // 拒绝空内容
    pub fn as_str(&self) -> &str;
    pub fn checksum(&self) -> ConfigChecksum;   // 纯函数计算
}
```

> **`ConfigBody` 不解析 YAML**：YAML 解析属基础设施细节（`-t` 由内核执行）。Domain 只知道"有一份不透明配置 + 它的校验和 + 它的校验结论"。

### 2.5 `domain/src/subscription/` — 订阅

```rust
// domain/src/subscription/source.rs
/// 订阅源。**Domain 不依赖 url crate**，只保留校验后的字符串 + 解析出的 host。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionUrl {
    raw: String,
    host: String,      // 用于 SSRF 判定
}

impl SubscriptionUrl {
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError>;   // 仅允许 http/https
    pub fn as_str(&self) -> &str;
    pub fn host(&self) -> &str;
    /// SSRF 防护（REQ-SUB-009）：拒绝环回/链路本地/元数据/RFC1918
    pub fn is_public_destination(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionSource {
    Url { url: SubscriptionUrl, user_agent: Option<String> },
    // 后续：LocalFile / ManualContent
}
```

```rust
// domain/src/subscription/schedule.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval(u64);   // 秒

impl Interval {
    /// 下限保护：避免配置成每秒触发（REQ-SUB-004 并发抑制的前提）
    pub fn from_seconds(secs: u64) -> Result<Self, DomainError>;   // 最小 60
    pub fn as_seconds(self) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule { pub interval: Interval }
```

```rust
// domain/src/subscription/mod.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetFormat { Mihomo }   // Domain 只认业务目标，不认 Sub-Store 的 target 串

#[derive(Debug, Clone)]
pub struct Subscription {
    id: SubscriptionId,
    name: String,
    source: SubscriptionSource,
    converter: ConverterId,
    target: TargetFormat,
    enabled: bool,
    schedule: Option<Schedule>,
    last_update: Option<UpdateRecord>,
}

impl Subscription {
    pub fn new(...) -> Result<Self, DomainError>;    // name 非空
    pub fn id(&self) -> &SubscriptionId;
    pub fn is_due(&self, now: Timestamp) -> bool;
    pub fn enable(&mut self) / disable(&mut self);
    pub fn record_update(&mut self, record: UpdateRecord);
}

#[derive(Debug, Clone)]
pub struct UpdateRecord {
    pub at: Timestamp,
    pub outcome: UpdateOutcome,
    pub produced_config: Option<ConfigVersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    Succeeded,
    Failed(UpdateFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateFailure {
    Unreachable,
    ConvertFailed(String),
    InvalidOutput(String),
    ValidationFailed(String),
    /// 明确记录：失败时旧配置仍激活（REQ-SUB-003）
    PreservedActiveConfig(ConfigVersionId),
}
```

```rust
// domain/src/subscription/conversion.rs
/// 转换产物：**只是 proxies 段，不是完整配置**（R04 C3）
#[derive(Debug, Clone)]
pub struct ConvertedProxies {
    pub nodes_yaml: String,
    pub node_count: usize,
}

impl ConvertedProxies {
    /// 空产物视为失败，绝不能静默通过（R06：sub-store-convert 的教训）
    pub fn new(nodes_yaml: impl Into<String>, node_count: usize) -> Result<Self, DomainError>;
}

/// 转换器能力（供 Application 决策，不含任何 Sub-Store 参数名）
#[derive(Debug, Clone)]
pub struct ConverterCapabilities {
    pub id: ConverterId,
    pub supports_targets: Vec<TargetFormat>,
    pub supports_merge_sources: bool,
    pub version: Option<String>,
}
```

### 2.6 `domain/src/audit/`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditAction {
    MihomoStart, MihomoStop, MihomoRestart, MihomoReload,
    KernelUpdate,
    ConfigActivate, ConfigRollback,
    SubscriptionUpdate,
    SystemFirewallApply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditActor { LocalRoot, LocalUser { uid: u32, name: String }, RemotePrincipal { id: String } }

/// 注意：target 与 metadata 已是脱敏后的内容（REQ-SEC-006）
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: AuditEntryId,
    pub action: AuditAction,
    pub actor: AuditActor,
    pub target: String,
    pub result: AuditResult,
    pub at: Timestamp,
    pub metadata: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditResult { Success, Failure { reason: String } }
```

---

## 3. Application 层：Ports

### 3.1 控制面 Ports（命令）

```rust
// application/src/ports/mihomo_controller.rs
#[async_trait::async_trait]
pub trait MihomoController: Send + Sync {
    async fn version(&self) -> Result<MihomoBuild, PortError>;
    async fn runtime_config(&self) -> Result<RuntimeConfigSummary, PortError>;

    /// 唯一 reload 入口。
    /// ⚠ 设计约束：ReloadRequest 中**不存在 force 变体**（REQ-CONFIG-011）。
    /// ⚠ 返回 204 不代表生效 —— 必须随后调用 health_check（R02 C1b）。
    async fn reload(&self, request: ReloadRequest) -> Result<ReloadOutcome, PortError>;

    async fn proxies(&self) -> Result<ProxyList, PortError>;
    async fn select_proxy(&self, group: &str, proxy: &str) -> Result<(), PortError>;
    async fn test_delay(&self, name: &str, opts: DelayOptions) -> Result<DelayOutcome, PortError>;
    async fn rules(&self) -> Result<RuleList, PortError>;
    async fn health_check(&self) -> Result<HealthReport, PortError>;
}

/// payload 模式优先（绕过 SAFE_PATHS 与文件存在性两个失败点，R02 §11）
#[derive(Debug, Clone)]
pub enum ReloadRequest {
    Payload(ConfigBody),
    Path(ConfigPath),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadOutcome { Applied, Rejected { http_status: u16 } }

/// 504 必须映射为业务结果，而不是基础设施错误（ADR-003 D6）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelayOutcome { Measured { millis: u32 }, Timeout, Unavailable { reason: String } }
```

```rust
// application/src/ports/process_manager.rs
/// 语义：管理 **Agent 自己的子进程**（MVP 权威形态）。
/// systemd 只负责拉起 proxy-agent 本身；管理 mihomo.service 是另一个 Port（ServiceManager）。
#[async_trait::async_trait]
pub trait ProcessManager: Send + Sync {
    async fn start(&self, opts: StartOptions) -> Result<ProcessHandle, PortError>;
    async fn stop(&self, handle: &ProcessHandle, timeout: Duration) -> Result<ExitStatus, PortError>;
    async fn status(&self, handle: &ProcessHandle) -> Result<ProcessStatus, PortError>;
    /// 只允许发送已认证的信号；实现必须拒绝 SIGUSR1/SIGUSR2（R03 C2）
    async fn signal(&self, handle: &ProcessHandle, signal: AllowedSignal) -> Result<(), PortError>;
    /// 捕获的日志行流（供 MihomoLog 事件）
    fn log_stream(&self, handle: &ProcessHandle) -> Result<LogStream, PortError>;
}

/// 白名单信号：SIGUSR1/SIGUSR2 在 mihomo 中未注册 → 默认处置=终止（R03 实测）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedSignal { Term, Kill }

#[derive(Debug, Clone)]
pub struct StartOptions {
    pub binary: PathBuf,
    pub working_dir: PathBuf,
    pub config: ConfigPath,
    /// ambient capability 由 systemd 提供；此处仅声明期望，不自行提权（R09 C3）
    pub required_capabilities: Vec<CapabilityKind>,
}

#[derive(Debug, Clone)]
pub struct ProcessHandle { pub pid: u32 }
```

```rust
// application/src/ports/service_manager.rs
/// systemd（或等价 init）抽象。MVP 只实现"探测 + Agent 自身状态"，
/// 不用于托管 mihomo（避免双 supervisor，ADR-003 D4 / R09 C2）。
#[async_trait::async_trait]
pub trait ServiceManager: Send + Sync {
    async fn detect(&self) -> Result<InitSystem, PortError>;
    async fn is_agent_service_active(&self) -> Result<bool, PortError>;
    /// 探测是否可用 systemctl（容器内常不可用，R10 C6）
    async fn supports_unit_control(&self) -> Result<bool, PortError>;
}
```

### 3.2 观测面 Ports（只读流）

```rust
// application/src/ports/mihomo_observer.rs
#[async_trait::async_trait]
pub trait MihomoObserver: Send + Sync {
    async fn traffic(&self) -> Result<TrafficStream, PortError>;
    async fn logs(&self, level: LogLevel) -> Result<LogStream, PortError>;
    async fn memory(&self) -> Result<MemoryStream, PortError>;
}

pub struct TrafficStream(pub BoxStream<'static, TrafficSample>);
pub struct LogStream(pub BoxStream<'static, LogEntry>);
pub struct MemoryStream(pub BoxStream<'static, MemorySample>);
pub type BoxStream<'a, T> = std::pin::Pin<Box<dyn futures_core::Stream<Item = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel { Debug, Info, Warning, Error }

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    /// 已脱敏（REQ-SEC-006）
    pub message: String,
    pub at: Option<Timestamp>,
}
```

```rust
// application/src/ports/mihomo_connection_ops.rs
/// 连接明细含 uid/process/processPath 隐私字段（R01 C9）→ 独立 Port 便于单独授权
#[async_trait::async_trait]
pub trait MihomoConnectionOps: Send + Sync {
    async fn connections(&self) -> Result<ConnectionList, PortError>;
    async fn close_connection(&self, id: &str) -> Result<(), PortError>;
    async fn close_all(&self) -> Result<(), PortError>;
}
```

### 3.3 配置 Ports

```rust
// application/src/ports/config_repository.rs
#[async_trait::async_trait]
pub trait ConfigRepository: Send + Sync {
    /// 必须带 limit，避免无界返回（规模爆炸防护）
    async fn list(&self, instance: &MihomoInstanceId, limit: usize) -> Result<Vec<ConfigVersion>, PortError>;
    async fn get(&self, id: &ConfigVersionId) -> Result<Option<ConfigVersion>, PortError>;
    async fn next_sequence(&self, instance: &MihomoInstanceId) -> Result<u64, PortError>;
    async fn save(&self, version: &ConfigVersion, body: &ConfigBody) -> Result<(), PortError>;
    async fn active(&self, instance: &MihomoInstanceId) -> Result<Option<ConfigVersion>, PortError>;
    /// 原子切换 active 指针（temp + fsync + rename，REQ-CONFIG-003）
    async fn set_active(&self, instance: &MihomoInstanceId, id: &ConfigVersionId) -> Result<(), PortError>;
    async fn read_body(&self, version: &ConfigVersion) -> Result<ConfigBody, PortError>;
}
```

```rust
// application/src/ports/config_validator.rs
/// 独立成 Port 的理由（R02 实测）：
/// ① mihomo -t 有副作用（含 GEOIP/GEOSITE 时真实下载 geodata，阻塞约 90s）
/// ② mihomo -t 对不存在的文件返回 exit 0 假成功
/// ③ mihomo -t 不检测未知字段 → 必须有独立的字段白名单校验
/// 因此必须能与纯语法校验分离测试、分离替换。
#[async_trait::async_trait]
pub trait ConfigValidator: Send + Sync {
    /// L0 资源预检：端口可用性 / geodata 就绪 / provider 可达
    async fn preflight(&self, candidate: &ConfigCandidate<Unvalidated>, ctx: &PreflightContext)
        -> Result<LevelOutcome, PortError>;

    /// L1 YAML 语法
    async fn validate_syntax(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError>;

    /// L2 语义 = mihomo -t ⊕ Agent 字段白名单
    /// 实现必须：隔离临时 -d 目录、先确认文件存在、离线时跳过 geodata 相关
    async fn validate_semantic(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError>;
}

#[derive(Debug, Clone)]
pub struct PreflightContext {
    pub desired_ports: Vec<u16>,
    pub requires_geodata: bool,
    pub offline: bool,
}
```

### 3.4 订阅与转换 Ports

> **对本设计的 ADR 修正（需同步 ADR-002）**：ADR-002 D4 把返回类型写作 `ConvertedSubscription`。本设计改为 **`ConvertedProxies`**，因为 R04 实测证明 Sub-Store 只返回 `proxies:` 段（不含 `mixed-port`/`dns`/`rules`/`proxy-groups`）。名字必须反映"这只是节点列表，不是可运行订阅"，否则实现者容易误以为拿到的是完整配置。这是**命名收紧**，不改变方法集，属 ADR-002 的澄清而非推翻。

```rust
// application/src/ports/subscription_converter.rs
#[async_trait::async_trait]
pub trait SubscriptionConverter: Send + Sync {
    async fn convert(&self, request: ConvertRequest) -> Result<ConvertedProxies, PortError>;
    async fn capabilities(&self) -> Result<ConverterCapabilities, PortError>;
}

/// 只表达业务意图；**不含任何 Sub-Store 参数名**（REQ-SUB-001）
#[derive(Debug, Clone)]
pub struct ConvertRequest {
    pub source: SubscriptionSource,
    pub target: TargetFormat,    pub proxy: Option<String>,
    pub merge_sources: bool,
}

/// 错误枚举对应 ADR-002 D4
#[derive(Debug, thiserror::Error)]
pub enum ConverterError {
    #[error("converter unreachable")]
    Unreachable,
    #[error("subscription not found in converter backend")]
    SubscriptionNotFound,
    #[error("unsupported target")]
    UnsupportedTarget,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("converter returned empty or invalid output")]
    EmptyOrInvalidOutput,
    #[error("converter returned invalid output: {0}")]
    InvalidOutput(String),
}
```

```rust
// application/src/ports/subscription_repository.rs
#[async_trait::async_trait]
pub trait SubscriptionRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<Subscription>, PortError>;
    async fn get(&self, id: &SubscriptionId) -> Result<Option<Subscription>, PortError>;
    async fn save(&self, sub: &Subscription) -> Result<(), PortError>;
    async fn delete(&self, id: &SubscriptionId) -> Result<(), PortError>;
    async fn due_for_update(&self, now: Timestamp) -> Result<Vec<SubscriptionId>, PortError>;
}
```

### 3.5 系统与安全 Ports

```rust
// application/src/ports/capability_probe.rs
/// 探测必须无副作用（REQ-LXC-005）
#[async_trait::async_trait]
pub trait CapabilityProbe: Send + Sync {
    async fn environment(&self) -> Result<SystemEnvironment, PortError>;
    async fn probe_all(&self, opts: ProbeOptions) -> Result<CapabilitySet, PortError>;
}

#[derive(Debug, Clone, Copy)]
pub struct ProbeOptions {
    /// 默认为 false：写类探测（nft 试写、ip rule 等）必须显式开启
    pub allow_write_probes: bool,
}
```

```rust
// application/src/ports/secret_store.rs
/// 生成/存储/轮换 secret 与 token。实现负责哈希存储（REQ-SEC-004）。
#[async_trait::async_trait]
pub trait SecretStore: Send + Sync {
    async fn mihomo_controller_secret(&self) -> Result<String, PortError>;
    async fn rotate_mihomo_secret(&self) -> Result<String, PortError>;
    async fn verify_api_token(&self, presented: &str) -> Result<Option<Principal>, PortError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal { pub id: String }
```

> 早期草案里 `Principal` 还带一个 `Role { Admin, ReadOnly }`。它已被删除：
> 认证是唯一的授权维度，见 ADR-010 D12。`sessions.role` 与 `api_principals.role`
> 两列也随之在 schema v5 中移除。

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
// application/src/ports/kernel_installer.rs
/// 自研内核更新链路（禁止依赖 /upgrade，ADR-003 D2）
#[async_trait::async_trait]
pub trait KernelInstaller: Send + Sync {
    async fn current_install(&self) -> Result<Option<KernelInstallation>, PortError>;
    async fn fetch(&self, version: &MihomoVersion) -> Result<DownloadedArtifact, PortError>;
    async fn verify(&self, artifact: &DownloadedArtifact, expected: &ConfigChecksum) -> Result<(), PortError>;
    /// 原子替换 + 保留上一版本以便回滚
    async fn install(&self, artifact: &DownloadedArtifact) -> Result<KernelInstallation, PortError>;
    async fn rollback_previous(&self) -> Result<KernelInstallation, PortError>;
}
```

### 3.6 Port 错误模型

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
    Timeout(Duration),

    #[error(transparent)]
    Converter(#[from] ConverterError),

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

impl PortError {
    /// 供 application 做重试决策；只读失败可重试，非法请求不可
    pub fn is_retryable(&self) -> bool;
    /// 是否属于"能力不可用"类降级（而非真错误）
    pub fn is_degradation(&self) -> bool;
}
```

---

## 4. Application 层：Use Case 签名与错误

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

    /// 明确的降级结果：操作未完成，但当前配置未受影响
    #[error("degraded: {0}")]
    DegradedPreservingActiveConfig(String),
}
```

```rust
// application/src/context.rs
/// Use Case 共享的依赖集合。全部 Arc<dyn ...>，由 bootstrap 组装。
pub struct AppContext {
    pub instance: MihomoInstanceId,
    pub controller: Arc<dyn MihomoController>,
    pub process: Arc<dyn ProcessManager>,
    pub observer: Arc<dyn MihomoObserver>,
    pub connections: Arc<dyn MihomoConnectionOps>,
    pub configs: Arc<dyn ConfigRepository>,
    pub validator: Arc<dyn ConfigValidator>,
    pub subscriptions: Arc<dyn SubscriptionRepository>,
    pub converter: Arc<dyn SubscriptionConverter>,
    pub probe: Arc<dyn CapabilityProbe>,
    pub secrets: Arc<dyn SecretStore>,
    pub audit: Arc<dyn AuditSink>,
    pub kernel: Arc<dyn KernelInstaller>,
    pub clock: Arc<dyn Clock>,
    pub events: EventBus,
}
```

### 4.1 配置生命周期 Use Cases（核心）

```rust
// application/src/use_cases/config.rs

/// ActivateConfig —— 唯一激活路径。回滚复用本函数（ADR-004 D5）。
pub struct ActivateConfig;

#[derive(Debug)]
pub struct ActivateConfigInput {
    pub candidate: ConfigCandidate<Unvalidated>,
    /// 回滚场景：失败时恢复到该版本（通常为当前 active）
    pub rollback_to: Option<ConfigVersionId>,
}

#[derive(Debug)]
pub struct ActivateConfigOutput {
    pub activated: ConfigVersionId,
    pub reload: ReloadOutcome,
    pub health: HealthReport,
    pub rolled_back: bool,
}

impl ActivateConfig {
    pub async fn execute(
        ctx: &AppContext,
        input: ActivateConfigInput,
    ) -> Result<ActivateConfigOutput, ApplicationError>;
}

/* 实现骨架（关键不变量标注）：

   1. candidate.validate(report)              // typestate 门禁
        ├─ preflight()      → L0 资源预检
        ├─ validate_syntax()→ L1
        └─ validate_semantic() → L2（-t ⊕ 字段白名单）
   2. repo.next_sequence + repo.save(version, body)   // 不可变版本落盘
   3. repo.set_active(version.id)                     // 原子切换
   4. controller.reload(ReloadRequest::Payload(...))  // 禁止 force
   5. controller.health_check()                       // 204 ≠ 生效
   6. 失败 → RollbackConfig（restart 落地，不是 reload）
   7. audit.record(...)
*/
```

```rust
/// RollbackConfig —— 必须以 restart 落地（R02 C5b）
pub struct RollbackConfig;

#[derive(Debug)]
pub struct RollbackConfigInput { pub target: ConfigVersionId }

#[derive(Debug)]
pub struct RollbackConfigOutput { pub active: ConfigVersionId, pub restarted: bool }

impl RollbackConfig {
    pub async fn execute(ctx: &AppContext, input: RollbackConfigInput)
        -> Result<RollbackConfigOutput, ApplicationError>;
}
```

### 4.2 其余 Use Cases（签名）

```rust
// application/src/use_cases/mihomo.rs
pub struct StartMihomo;    // → Result<StartOutcome, ApplicationError>（含 AlreadyRunning）
pub struct StopMihomo;
pub struct RestartMihomo;  // 用 ProcessManager，不用 /restart
pub struct ReloadMihomo;   // 复用 ActivateConfig 的 reload+health 段
pub struct GetMihomoStatus;
pub struct UpdateMihomoKernel;   // 与配置链路完全独立（REQ-MIHOMO-007）

// application/src/use_cases/subscription.rs
pub struct CreateSubscription;
pub struct UpdateSubscription;   // ★ 失败必须保留旧配置（REQ-SUB-003）
pub struct DeleteSubscription;
pub struct ListSubscriptions;
pub struct TestSubscription;     // 只转换不激活

// application/src/use_cases/system.rs
pub struct RunDoctor;
pub struct GetCapabilities;
```

```rust
/// UpdateSubscription —— 核心不变量的主测试对象
impl UpdateSubscription {
    pub async fn execute(ctx: &AppContext, id: &SubscriptionId)
        -> Result<UpdateSubscriptionOutput, ApplicationError>;
}

#[derive(Debug)]
pub struct UpdateSubscriptionOutput {
    pub outcome: UpdateOutcome,
    /// 无论成功失败都必须给出：失败时 = 当前仍激活的版本
    pub active_config: ConfigVersionId,
}

/* 实现骨架：
   1. sub = repo.get(id)?  ；校验 enabled
   2. converted = converter.convert(...)   // 空产物 → EmptyOrInvalidOutput（绝不放行）
   3. 组装完整 ConfigBody（Agent 职责：端口/controller/secret/CORS/dns/tun/groups/rules）
   4. ActivateConfig::execute(...)          // 复用唯一激活路径
   5. 任一步失败 → 记录 UpdateFailure::PreservedActiveConfig(current_active)
      ⚠ 绝不停止 Mihomo、绝不替换 active（REQ-SUB-003）
*/
```

---

## 5. Bootstrap 层

### 5.1 职责与结构

```rust
// bootstrap/src/main.rs
#[tokio::main]
async fn main() -> ExitCode {
    // 1. 解析 CLI 参数与配置（属 interfaces，bootstrap 只做装配）
    // 2. Build AppContext（唯一组装点）
    // 3. 启动 server / TUI / 一次性命令
    // 4. 优雅关闭
}
```

```rust
// bootstrap/src/composition.rs
pub struct Bootstrap;

impl Bootstrap {
    /// 唯一组装点：根据运行时配置选择 Adapter 实现
    pub async fn build(config: &RuntimeConfig) -> Result<AppContext, BootstrapError> {
        // Adapter 选择（Phase 0 决定的边界）
        let process: Arc<dyn ProcessManager> = match cap.status(CapabilityKind::Systemd) {
            CapabilityStatus::Supported => Arc::new(SupervisedChildProcess::new()),
            // 容器内无 systemd（R10 C6）→ 同一实现，systemd 只负责拉起 Agent
            _ => Arc::new(SupervisedChildProcess::new()),
        };

        // controller 通道选择：unix socket 优先（ADR-003 D3）
        let controller: Arc<dyn MihomoController> = if let Some(sock) = &config.controller_unix {
            Arc::new(UnixSocketController::new(sock.clone(), Arc::clone(&secrets)))
        } else {
            Arc::new(HttpController::new(config.controller_http.clone(), Arc::clone(&secrets)))
        };

        // converter 选择：Sub-Store > Native（ADR-002 D1；sub-store-convert 已 Rejected）
        let converter: Arc<dyn SubscriptionConverter> = match &config.converter {
            ConverterConfig::SubStore { base_url, .. } =>
                Arc::new(SubStoreConverter::new(base_url.clone(), http.clone())),
            ConverterConfig::Native =>
                Arc::new(NativeConverter::new()),   // MVP: 返回 NotImplemented
        };

        // validator 与 probe 在启动时用真实环境构造（能力探测一次，缓存到 AppContext）
        let probe = Arc::new(LinuxCapabilityProbe::new(ProbeOptions::default()));
        let environment = probe.environment().await?;

        Ok(AppContext {
            instance: config.instance_id.clone(),
            controller, process, validator: Arc::new(MihomoConfigValidator::new(...)),
            probe, environment,
            /* observer, connections, configs, subscriptions,
               secrets, audit, kernel, clock, events */
        })
    }
}

/// Adapter 选择必须可被配置覆盖，且非法组合在启动期就失败（fail fast）
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Port(#[from] PortError),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("security precondition failed: {0}")]
    SecurityPrecondition(String),
}
```

### 5.2 启动期安全硬校验（REQ-SEC-004/012/013）

```rust
// bootstrap/src/guards.rs
/// 启动期必须通过，否则拒绝启动（不是告警）
pub fn enforce_security_preconditions(
    config: &RuntimeConfig,
    secrets: &Arc<dyn SecretStore>,
) -> Result<(), BootstrapError> {
    // 1. controller 必须 loopback 或 unix socket；禁止 ":9090" / 0.0.0.0（R12 实测）
    if let Some(addr) = &config.controller_http {
        if !addr.is_loopback() {
            return Err(BootstrapError::SecurityPrecondition(
                "mihomo controller must bind loopback or use unix socket".into()));
        }
    }
    // 2. TCP controller 模式必须有非空 secret（R12 C1）
    // 3. Agent API 非 loopback 监听而无 token ⇒ 拒绝启动（ADR-005 D3）
    // 4. CORS 必须已被收窄（R12 C4：默认 ["*"] + private-network）
    Ok(())
}
```

### 5.3 生命周期与优雅关闭

```rust
// bootstrap/src/lifecycle.rs
pub struct ShutdownCoordinator {
    token: tokio_util::sync::CancellationToken,
    tasks: tokio::task::JoinSet<()>,
}

impl ShutdownCoordinator {
    /// 所有后台任务必须持 token；禁止游离的 spawn（AGENTS.md）
    pub fn spawn<F>(&mut self, name: &'static str, fut: F);
    /// SIGTERM/SIGINT → 取消 → 等待（超时强杀）
    pub async fn shutdown(self, timeout: Duration) -> ShutdownReport;
}

/// 关闭序列：停止接受新请求 → 取消后台任务 → 停止 mihomo（SIGTERM，超时 SIGKILL）
/// 超时分配与 systemd TimeoutStopSec 协同（R09）
pub struct StopSequence {
    pub mihomo_stop_timeout: Duration,   // 建议 10s
    pub agent_shutdown_timeout: Duration,// 建议 15s（systemd TimeoutStopSec=20s）
}
```

### 5.4 事件总线

```rust
// application/src/events.rs（bootstrap 负责实例化）
#[derive(Debug, Clone)]
pub enum DomainEvent {
    MihomoStatusChanged { instance: MihomoInstanceId, from: MihomoStatus, to: MihomoStatus },
    MihomoLog(LogEntry),
    TrafficUpdated(TrafficSample),
    SubscriptionUpdated { id: SubscriptionId, outcome: UpdateOutcome },
    ConfigActivated { instance: MihomoInstanceId, version: ConfigVersionId },
    JobFinished { id: JobId, result: AuditResult },
    CapabilityChanged { kind: CapabilityKind, status: CapabilityStatus },
}

/// MVP 用 broadcast（AGENTS.md）；容量受限，慢消费者收到 Lagged 而不是拖垮生产者
#[derive(Clone)]
pub struct EventBus(tokio::sync::broadcast::Sender<DomainEvent>);

impl EventBus {
    pub fn new(capacity: usize) -> Self;
    pub fn publish(&self, event: DomainEvent);      // 无订阅者时静默丢弃
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<DomainEvent>;
}
```

### 5.5 并发串行化（REQ-MIHOMO-005）

```rust
// bootstrap/src/locks.rs
/// per-instance 串行化：生命周期与配置激活共用同一把锁，避免交叉
pub struct InstanceLocks {
    inner: tokio::sync::Mutex<std::collections::HashMap<MihomoInstanceId, Arc<tokio::sync::Mutex<()>>>>,
}

impl InstanceLocks {
    /// 生命周期操作与激活操作都必须先取锁
    pub async fn acquire(&self, instance: &MihomoInstanceId) -> tokio::sync::OwnedMutexGuard<()>;
}

/// 订阅更新去重：同一订阅不得并发更新（REQ-SUB-004）
pub struct SubscriptionGuards {
    inner: std::sync::Mutex<std::collections::HashSet<SubscriptionId>>,
}

impl SubscriptionGuards {
    /// 返回 None 表示已有进行中的更新 → 调用方应跳过而非排队
    pub fn try_begin(&self, id: &SubscriptionId) -> Option<SubscriptionGuard>;
}
```

---

## 6. 测试映射（Domain 纯单测 / Application mock Ports）

| 需求 | 测试位置 | 测试内容 |
|---|---|---|
| REQ-CONFIG-002 / REQ-SUB-003 | Application（mock Ports） | 转换失败、校验失败、reload 失败、健康检查失败四种注入下，`active` 版本不变 |
| REQ-CONFIG-005/006 | Domain + Application | 未校验候选无法激活（编译期）；四层校验各自失败路径 |
| REQ-CONFIG-011 | Application | `ReloadRequest` 无 force 变体（编译期）+ 请求体断言 |
| REQ-CONFIG-013 | Application | 健康检查失败后回滚走 restart 路径（ProcessManager 被调用） |
| REQ-MIHOMO-005 | Domain | `transition` 非法转换返回 Err；`begin_start` 在 Starting 时返回 `AlreadyStarting` |
| REQ-LXC-002 | Domain | `CapabilityStatus` 五值；`evaluate_tun` 对四种组合（缺设备/打不开/缺 CAP/ioctl 失败）分别产出正确值 |
| REQ-LXC-005 | Application | `ProbeOptions{allow_write_probes:false}` 下探测不触发写操作 |
| REQ-SUB-004 | Application | 同一订阅并发两次 → `try_begin` 第二次返回 None |
| REQ-SUB-009 | Domain | `SubscriptionUrl::is_public_destination` 拒绝 `127.0.0.1`/`169.254.169.254`/`::1`/RFC1918 |
| REQ-SEC-001/012 | Unit | controller 地址校验拒绝 `":9090"`、`0.0.0.0:9090`、空 host；CORS 收窄 |
| REQ-SEC-006 | Unit | 脱敏函数对 URL query 中 `token`/`password`、secret、UUID 的矩阵测试 |
| REQ-OPS-002 | E2E（Linux） | deb 升级不覆盖 conffile |

---

## 7. 未知项（明确 defer，附 owner）

| 项 | 原因 | Owner / 时机 |
|---|---|---|
| `SO_PEERCRED` 的 tokio API 具体形态 | 属 adapter 实现；Port 只暴露 `Principal` | Infrastructure 阶段 |
| `ConfigBody` 是否改用 `bytes::Bytes` | 需要大配置实测数据 | 实现阶段，可逆 |
| Domain 是否引入 `chrono` 而非自研 `Timestamp` | 当前选自研以保证纯度；如样板代码过多可放开 | 实现阶段，可逆 |
| `SAFE_PATHS` 最小集合 | 依赖 `open-questions.md` Q018 收敛 | ADR-004 后续 |
| `KernelInstaller` 的校验强度（checksum vs 签名） | 依赖 Q005/Q015（上游是否提供签名、镜像合规性） | 打包前 |

---

## 8. 被否决的替代方案

| 方案 | 否决理由 |
|---|---|
| native AFIT（无 `async_trait`） | **实测不可 dyn**（E0038）→ bootstrap 无法运行时装配，违反"外部集成可替换" |
| 泛型 Port（`ProcessManager<P>`） | 类型参数污染 Application 全部签名；单机 Modular Monolith 无收益 |
| Domain 持有 `Arc<dyn Clock>` 而非参数注入 | 会让 Domain 依赖 trait 对象与运行时；参数注入更纯且更易测 |
| 用 `bool` 表达能力 | 违反 REQ-LXC-002；实测"设备存在但不可用"必须可表达 |
| 让 `ConfigVersion` 提供 `set_active` | 违反不可变性（REQ-CONFIG-007）；回滚是"激活旧版本"而非改写 |
| `ProcessManager` 兼管 `mihomo.service`（双 unit） | 跨用户后 Agent 无法 kill/读写其 socket，需 polkit 窄化（R09 C2/C3）；容器内无 systemd（R10 C6） |
| `SubscriptionConverter` 暴露 Sub-Store 参数 | 违反 REQ-SUB-001；且 `/download/sub` 实为不存在的接口（R04） |

---

## 9. 提交顺序建议（每个 commit 独立可编译）

```text
1. feat(domain): add shared kernel (ids, timestamp, DomainError)
2. feat(domain): add system capabilities with five-state status
3. feat(domain): add mihomo instance lifecycle state machine
4. feat(domain): add config version and typestate validation gate
5. feat(domain): add subscription model and SSRF-aware source
6. feat(application): define ports and PortError taxonomy
7. feat(application): add ActivateConfig / RollbackConfig use cases
8. feat(application): add UpdateSubscription with preserve-active invariant
9. feat(application): add doctor and capability use cases
10. feat(bootstrap): add AppContext composition and security guards
11. feat(bootstrap): add shutdown coordinator, event bus, instance locks
```

前 5 个 commit 完成后 `domain` 已可独立测试（无 IO、无 async）；第 6–9 个 commit 后可用 mock Ports 覆盖全部核心失败路径。

---

## 10. 实现状态（Domain 层已落地）

```text
阶段 1–5（domain）状态：已实现并通过全部质量门
```

**产物**：`crates/domain/`，共 28 个文件、约 5100 行（含测试），`proxy-domain` crate。

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --check` | PASS |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| 测试 | `cargo test --workspace` | **146 passed, 0 failed** |
| 构建 | `cargo build --workspace` | PASS |
| 依赖纯度 | `cargo tree -p proxy-domain` | 仅 `thiserror`（无 tokio/reqwest/sqlx/axum/async-trait） |

**测试分布**：configuration 45、subscription 31、mihomo 27、system 19、shared 13、audit 6、架构守卫 5。

### 10.1 与本文档设计的差异（实现期修正）

| 项 | 文档原设计 | 实现 | 原因 |
|---|---|---|---|
| `SubscriptionSource::url()` 构造器 | `from_url()` | 与访问器 `url()` 同名会冲突（E0592） | Rust 不允许同名方法与关联函数共存 |
| 多处 `const fn` | 广泛标注 `const` | 收窄为可在 const 上下文中求值的少数函数 | `Option::map`、`PartialEq`、析构均非 const-stable，`const` 会编译失败 |
| `Timestamp::seconds_since` | 直接 `saturating_sub` | 改为 `checked_sub` + 显式判正 | `i64::saturating_sub` 在 `i64::MIN` 饱和而非 0，未来时间戳会得到错误结果（该 bug 被测试捕获） |
| 架构守卫 | 未在文档中列出 | 新增 `crates/domain/tests/architecture.rs` | 静态扫描 `Cargo.toml` 与源码，阻止领域层引入基础设施依赖 |

### 10.2 领域纯度如何被强制

三层防护，且**已验证会真正失败**（而非仅存在）：

1. **`Cargo.toml` 依赖白名单** —— 加入 `tokio` 会导致编译失败，这是最强保证。
2. **源码文本守卫**（5 个测试）—— 覆盖 `Cargo.toml` 与全部 `src/*.rs`。
3. **crate 属性** —— `#![forbid(unsafe_code)]`、`#![deny(missing_docs)]`，以及非测试构建下的 `deny(clippy::unwrap_used, expect_used, panic)`。

验证方式：向 `crates/domain/Cargo.toml` 加入 `tokio` 后，架构守卫以
`domain must not depend on 'tokio'; it breaks the layering rules in AGENTS.md`
失败 2 个测试；移除后恢复通过。

### 10.3 已实现的不变量及其测试

| 不变量 | 机制 | 代表测试 |
|---|---|---|
| 非法生命周期转换被拒绝且不改状态 | 密封 newtype + `transition()` | `illegal_transition_leaves_state_untouched` |
| 重复 start 不产生二次 spawn | `begin_start()` → `StartDecision` | `repeated_start_requests_never_spawn_twice` |
| 未校验配置不可激活 | typestate `ConfigCandidate<Unvalidated/Validated>` | `failing_report_cannot_promote` |
| `Starting → Starting` 被拒绝 | 转换表 | `starting_to_starting_is_forbidden` |
| 能力不可用时不启用 TUN | `can_enable_tun()` + 生成期门控 | `tun_withheld_for_every_non_supported_status` |
| 设备存在但缺 CAP 时判 `Misconfigured` | `evaluate_tun()` | `openable_device_without_net_admin_is_misconfigured` |
| 不生成无节点配置 | `ConvertedProxies::new` 拒绝空/零节点 | `rejects_zero_nodes_even_with_content` |
| controller 不得绑通配地址 | `ControllerEndpoint::parse` | `controller_rejects_wildcard_and_remote_hosts` |
| CORS 必须收窄 | 生成器无条件写入 | `cors_is_always_narrowed` |
| 订阅 URL 不得指向内网 | `is_public_destination()` | `rejects_cloud_metadata_address`、`rejects_ipv4_mapped_loopback` |
| 端口冲突在预检阶段拦截 | `preflight::evaluate` | `occupied_port_fails_preflight` |
| 离线 + geodata 规则 → 预检失败 | `geodata_unobtainable()` | `offline_with_geodata_rules_fails_preflight` |

### 10.4 下一步

阶段 6–11（application ports / use cases / bootstrap）尚未实现。
按 §9 的提交顺序，`domain` 已可作为稳定基础被引用。

