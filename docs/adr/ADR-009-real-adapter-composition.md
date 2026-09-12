# ADR-009 — 真实适配器装配与首次端到端联通

| 字段 | 值 |
|---|---|
| Status | Accepted（2026-09-12） |
| Date | 2026-09-12 |
| Related | ADR-001（依赖方向）、ADR-003（Mihomo 集成）、ADR-005（安全模型）、ADR-007（元数据持久化）、ADR-008（内核获取与校验） |

---

## 1. Context

至此 Domain / Application / Infrastructure 三层已完成，但 `Bootstrap` 只装配 `InMemoryFactory`——**没有任何一条真实路径跑通过**。13 个 adapter 各自有单测，却从未在真实数据库、真实内核、真实进程上一同运行。

本 ADR 记录把它们接起来时暴露的约束与缺陷。

### 1.1 装配缺口（逐个 adapter 核对构造参数后得出）

| Adapter | 需要 | `RuntimeConfig` 原状 |
|---|---|---|
| EventPublisher / ServiceManager / CapabilityProbe / ProcessManager | 无参或调用期传参 | ✅ |
| 5 个 SQLite adapter | 共享一个 `SqlitePool` | ✅ |
| **MihomoController（Loopback）** | **secret** | ❌ 字段不存在 |
| **ConfigRepository** | DB 路径 + configs_dir | ⚠️ 需推导 |
| **ConfigValidator / KernelInstaller** | **二进制路径、data_dir、scratch** | ❌ 字段不存在 |

### 1.2 一个真实循环依赖

```
loopback controller 需要 secret
        ↓
secret 在 SecretStore 里
        ↓
SecretStore 需要 database（pool）
        ↑
而 pool 由 composition root 建 —— 但 factory 拿不到 port
```

`AdapterFactory` 的契约明确写着**没有 factory 方法接收 `&AppContext`**，理由是一旦接收，adapter 就能依赖另一个 adapter，把编译期问题变成运行期问题。

### 1.3 同步 vs 异步

`AdapterFactory` 的方法是**同步的**，但部分 adapter 需要真实初始化（HTTP client、带特定权限的目录）。`FileConfigRepository::new` 原本是 `async`（会 `create_dir_all`）。

---

## 2. Decision

### D1. pool 与 secret 由 composition root 预先构造，作为**纯数据**注入

```text
Bootstrap::build_real(config)
  1. prepare_directories(config)      ← 目录 + 权限（async，能失败）
  2. open_store(config)               ← SqlitePool
  3. resolve_secret(config, &pool)    ← 仅 loopback 需要；unix socket 不生成
  4. RealFactory::new(pool, config, secret)?   ← 校验 + 持有
  5. Bootstrap::build(&factory, config)        ← 原有装配决策
```

**理由**：循环在**一个可读的地方**被打破，`AdapterFactory` 的"不接收 `&AppContext`"约束完整保留，factory 仍是输入的纯函数。

### D2. 目录准备放在 composition root，而非 adapter

因为 factory 方法同步，adapter 内做目录 I/O 会**阻塞运行时线程**。同理 `FileConfigRepository` 增加 `over_existing_dir(pool, dir)` 同步构造器。

### D3. 目录权限分两档：`Seal::Required` 与 `Seal::Preferred`

| 档 | 用于 | 语义 |
|---|---|---|
| `Required` | **socket 目录** | 模式即安全边界（内核在 unix socket 上**不鉴权**），达不到即失败 |
| `Preferred` | 数据/配置/scratch 目录 | 优先收紧；已存在且非世界可读则接受 |

**并且接受 sticky 目录**：实测 `/tmp` 是 `1777`（世界可访问**且 sticky**）。sticky 位正是 socket 需要其父目录提供的保护——防止他人替换条目。非 sticky 的世界可访问目录仍拒绝。

### D4. 未实现的 port **显式报错**，不给假实现

`MihomoObserver`、`MihomoConnectionOps`、`SubscriptionConverter` 返回
`PortError::InvalidResponse("no adapter is available for {port}: {why}")`，消息里写明原因与关联的开放问题。

假的空实现（返回 `Ok`）会让**「还没做」和「做了但坏了」无法区分**。

### D5. `StartOptions` 由 builder 注入，配置路径从**激活指针**解析

`AppContextBuilder` 新增 `start_options(...)`；`Bootstrap` 从 `ConfigRepository::active()` 取当前版本，用其 `label()` 按仓库自身的 `<label>.yaml` 约定拼路径。

**绝不能用写死的文件名**（如 `active.yaml`）：仓库根本不写这个文件，而缺失文件在这里是**最坏的错误**——`mihomo -t` 对不存在的文件报 **success 并创建它**，内核会以 starter 配置启动而不是已激活的配置。

---

## 3. Alternatives

| 方案 | 否决理由 |
|---|---|
| factory 方法接收 `&AppContext` | 破坏 trait 的核心约束，adapter 可依赖 adapter（D1） |
| factory 方法接收 `SecretProvider` 参数 | 签名膨胀，且把循环藏在参数里而非显式暴露（D1） |
| loopback 下要求配置文件明文写 secret | secret 会落入配置文件与进程环境 |
| adapter 内建目录（阻塞） | 同步方法里做文件 I/O 会阻塞运行时线程（D2） |
| 所有目录一律严格要求模式 | 会让 `/tmp` 下的开发根、共享挂载等**可工作配置拒绝启动**（D3） |
| socket 目录只要 `/tmp` 就跳过检查 | 把"共享"当成"安全"，丢失了 sticky 这一真正起作用的属性（D3） |
| 未实现 port 返回空实现 | 无法区分"未做"与"坏掉"（D4） |
| 配置路径写死 `active.yaml` | 仓库不写该文件；`-t` 会静默创建它并以 starter 配置启动（D5） |

---

## 4. Consequences

**正面**

- **首次端到端联通**：真实内核经真实 process adapter 启动（52 ms 就绪）、经真实 controller 观察健康、`AlreadyRunning` 防重复、**优雅停止**（`forced=false`）。
- **状态跨 Agent 重启存活**：重新组合后仍识别到运行中的内核并拒绝再 spawn——重复 spawn 防护在整条链路上生效。
- `AppContextBuilder` 缺失 `StartOptions` 入口这类缺口一旦修好，`Bootstrap::build` 的既有装配决策**未被改动**。

**代价与约束**

- `RuntimeConfig` 新增 4 个字段；`rooted_at()` 让测试能指到临时树而不触碰系统其他部分。
- `resolve_secret` 只在 loopback 下生成 secret。**unix socket 下不生成**——生成会暗示 socket 受它保护，而实际边界是 socket 的文件权限（ADR-005 R2 已实测：内核在 unix socket 上完全不校验 secret）。

**已知遗留**

| 项 | 状态 |
|---|---|
| `MihomoObserver` / `MihomoConnectionOps` | 推迟到 interfaces 层（无真实消费者即空转） |
| `SubscriptionConverter` | 卡 Q023（Sub-Store 入库） |
| interfaces 层（REST/CLI/TUI） | 未开始 |
| systemd 单元安装 | 属 packaging |

---

## 5. Evidence

- **真机**（Debian aarch64，mihomo v1.19.30）：workspace **627 passed / 0 failed**；端到端 3/3 通过。
- 端到端实测输出：`kernel started: pid=94396 ready_after=52.016715ms` / `kernel stopped: forced=false`。
- host workspace：**614 passed / 0 failed**。
- `/tmp` 模式实测为 `1777`（世界可访问 + sticky），这正是 D3 的依据。

### 5.1 实施中发现并修复的缺陷

| 缺陷 | 后果 | 如何发现 |
|---|---|---|
| `AppContextBuilder` 无 `StartOptions` 入口 | `process_state` 恒空 → **内核永远无法启动** | 真机 E2E：`InvalidState("no start options configured; cannot spawn")` |
| 配置路径写死 `active.yaml` | 仓库不写该文件；`-t` 会**成功创建**它以 starter 配置启动 | 核对 `FileConfigRepository::body_path` 时发现 |
| 目录准备对 `/tmp` 执行 chmod | station 组合**直接失败**，可工作配置无法启动 | 真机 E2E：`cannot set mode 750: Operation not permitted` |
| E2E 测试写死端口且失败时不回收内核 | 残留 6 个内核占住端口，后续运行报"降级启动"，**与代码无关** | `17901 BUSY` + `grep -c mhbin = 6` |
| `Seal::Required` 一度无人使用 | clippy 报未构造变体——暴露 socket 目录被误放宽 | `-D warnings` |
