# ADR-007 — 元数据持久化：驱动选择与领域重建入口

| 字段 | 值 |
|---|---|
| Status | Accepted（2026-09-12，批次 A 实施时定稿） |
| Date | 2026-09-12 |
| Related | ADR-001（D6 持久化）、ADR-004（配置生命周期）、ADR-005（安全模型）、`AGENTS.md` Persistence / Dependency Policy |

---

## 1. Context

`AGENTS.md` 把持久化定为「SQLite 存元数据 + 文件系统存配置版本」，并把 `sqlx` 列入首选依赖基线，但没有指定驱动。实施批次 A 时出现两个必须先定、否则无法开工的问题：

1. **驱动选择**：`sqlx` 还是 `rusqlite`？
2. **领域重建**：Domain 的类型能否从持久化记录还原？

### 1.1 驱动：实测约束

| 事实 | 结果 |
|---|---|
| `rusqlite 0.40.2`（`bundled`）在本项目沙箱可构建 | ✅ 7 秒，无系统 SQLite 依赖 |
| SQLite 访问是否阻塞 | 阻塞（两种驱动都一样，都需要阻塞边界） |
| `sqlx` 是否要求编译期 `DATABASE_URL` | 是（迁移需编译期可见） |
| crates.io 可获取 | ✅ `sqlx` 0.9.0 / `rusqlite` 0.40.2 |

### 1.2 领域重建：发现的真实缺口

实施 `InstanceRepository` 时核查发现，**Domain 的类型无法从持久化记录还原**：

| 类型 | 正向 label | 反向 parse | 后果 |
|---|---|---|---|
| `MihomoStatus` | ✅ `as_str()` | ❌ 私有 `Inner` enum | 无法重建实例状态 |
| `MihomoInstance` | — | ❌ 无「带入 status」的构造器 | 无法重组聚合 |
| `AuditAction/Actor/Target` | ✅ | ❌ 只有正向 | 审计能写不能读 |
| `*Id`（`shared/id.rs`） | ✅ | ✅ 已有 `parse` | 无问题 |

`MihomoStatus(Inner)` 是**刻意密封**的（防止外部构造非法状态），因此这不是 bug 而是**设计意图与持久化需求的真实冲突**。

---

## 2. Decision

### D1. 驱动选 `rusqlite` + `bundled`，偏离 `AGENTS.md` 的 `sqlx` 基线

- SQLite 访问在两种方案下**都是阻塞的**，因此「异步驱动」并不提供优势：`sqlx` 内部同样是 `spawn_blocking`。
- `sqlx` 额外引入**编译期 `DATABASE_URL` 依赖**和一套宏体系，而本项目只有 4 张表、schema 稳定。
- `bundled` 把 SQLite 编译进二进制，**不依赖发行版的 sqlite 版本与包布局**——对一个要打进 deb 的常驻服务，这是可部署性优势。

**这是 `AGENTS.md` 依赖基线的一次显式偏离**，按该文件的 Dependency Policy 要求在此记录。

### D2. 每个适配器持有**连接池**，不使用全局单连接 + 全局 Mutex

`rusqlite::Connection` 是 `Send` 但**不是 `Sync`**，无法跨异步任务共享。两个"顺手"的写法都是错的：

- **单连接 + 全局 `Mutex`**：会让所有适配器互相串行——一次 audit 读取会阻塞配置激活，并且在应用层精心设计的**每实例锁之下再叠一把无人设计的粗粒度锁**。
- **在异步任务里直接调用 SQLite**：SQLite 阻塞，会把运行时线程挂在数据库上而不是让出。

因此：每次操作**从池中取连接** + 在**阻塞线程池**上执行。并发由池大小界定，而非由锁界定。

### D3. 领域增加**受校验的重建入口**，而非暴露内部表示

| 新增 API | 关键约束 |
|---|---|
| `MihomoStatus::from_label` | **未知 label 返回错误，绝不回退到 `Stopped`** |
| `MihomoInstance::reconstitute` | **重新校验不变量**（名字非空）；status 与 build 矛盾时以 status 为准并丢弃 build |
| `AuditAction::from_label`、`AuditActor/Target/Result::from_parts` | 缺字段或未知判别符 → 错误 |

**为什么 `from_label` 不能回退**：一个持久化状态读取失败的**正在运行**的内核会显示为 `Stopped`，于是生命周期命令**再 spawn 一个内核**——正是 `InstanceRepository` 与状态机存在的意义所在。

**为什么审计用结构化列而非解析显示 label**：`AuditActor::label` 会渲染本地用户的**名字**，名字可被修改且非单射，不能作为存储键。

---

## 3. Alternatives

| 方案 | 否决理由 |
|---|---|
| `sqlx` + 编译期迁移 | 4 张表不值得一套宏体系与编译期数据库依赖；阻塞性质与 `rusqlite` 无差别 |
| 单连接 + 全局 `Mutex` | 适配器互相串行，并在每实例锁下叠加粗粒度锁（见 D2） |
| 在 Infrastructure 里 `match` label 重建聚合 | **技术不可行**：`MihomoInstance` 字段全私有且无 setter，Infrastructure 拼不出聚合 |
| `load()` 总是返回 `None`（不真正持久化实例） | 破坏 Port 唯一价值——重启后重复 spawn 防护失效 |
| 暴露 `MihomoStatus` 的 `Inner` 或加裸构造器 | 摧毁状态机的密封性，任何调用方都能构造非法状态 |
| 解析 `AuditActor::label` 还原（如 `local:name(1000)`） | 名字可含括号/冒号，解析歧义；改名即失效 |

---

## 4. Consequences

**正面**

- Domain 边界与依赖方向**未变**：新增的是重建入口，不是新的依赖；`proxy-domain` 仍只依赖 `thiserror`（已用 `cargo tree` 验证）。
- 重复 spawn 防护**跨 Agent 重启有效**，有测试覆盖（重开后仍决策 `AlreadyRunning`）。
- 审计轨迹不可被改写（表上不存在 update/delete 语句），且损坏时**报错而非编造**。
- 作业表**有界**，长时间运行不会无界增长（prune 与 insert 同事务）。

**代价与约束**

- Domain 新增了「为持久化服务」的 API。这是对密封性的**有控制让步**：让步以 `Result` 形式体现，而非裸构造。
- 每个持久化适配器都必须**显式书写** `from_parts`/`into_domain`，不能依赖 `serde` 自动派生。这是刻意的——自动派生会绕过不变量校验。
- 后续 `ConfigRepository`/`SubscriptionRepository` 必须沿用同一模式：**读不出来就报错，不要默认值**。

**已记录的残余风险**

- 作业丢失是**可接受**的（Port 明确说明：丢几个只损失可见性，不损失正确性）。
- 秒级 `updated_at` 不足以区分同一秒内的多次更新；`recent` 用 `rowid` 做了稳定的平局排序。

---

## 5. Evidence

- `rusqlite 0.40.2` + `bundled` 构建实测：本项目沙箱 7 秒通过；`journal_mode=WAL`、`busy_timeout`、`INSERT OR IGNORE` 幂等、`UPSERT ... RETURNING` 原子序列均在独立探针工程中验证。
- 真机验证（Debian aarch64，OrbStack）：`proxy-infrastructure` **56 个 storage 测试全部通过**，含 WAL、并发访问、重开持久性、保留上限；同时 14 个内核集成测试保持通过。
- 领域纯度：`cargo tree -p proxy-domain --depth 1` → 仅 `thiserror`。
- 依赖方向：`crates/application/src/` 中无 `rusqlite`/`reqwest`/`sqlx` 实现依赖（架构守卫测试通过）。
- 测试总量：workspace **438 passed / 0 failed**（批次 A 前为 359）。
