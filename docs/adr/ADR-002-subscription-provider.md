# ADR-002 — 订阅转换提供方策略

| 字段 | 值 |
|---|---|
| Status | Accepted（Phase 0 定稿） |
| Date | 2026-09-12 |
| Related | ADR-001、ADR-004、ADR-006、`docs/research/04-sub-store.md`、`05-sub-store-deployment.md`、`06-sub-store-convert.md`、`13-licenses.md` |

---

## 1. Context

### 1.1 需求约束

- REQ-SUB-001：Application 只依赖 `SubscriptionConverter` Port；不得出现 Sub-Store 的 URL、参数名、JSON 结构。
- REQ-SUB-006：转换器必须可替换。
- REQ-SUB-003：订阅更新失败（不可达 / 转换失败 / 输出非法）时，**旧配置必须保持激活**。
- 非目标：自研完整 Sub-Store；MVP 不实现完整 Native Converter（设计文档 §45）。

### 1.2 调研结论（决定性证据）

| # | 结论 | 证据 |
|---|---|---|
| C1 | **`GET /download/sub?url=...` 不是真实接口**。实测 404 `RESOURCE_NOT_FOUND`；`sub` 被当作订阅名。真实入口是 `GET /download/:name[/:target]`。 | R04 §1、§3（实测 + `download.js:74,82`） |
| C2 | **Sub-Store 是"两段式"有状态服务**：先 `POST /api/subs` 入库，再按名下载。无凭据即可写入（201）。 | R04 §4.3（实测）、R05 §1 |
| C3 | **输出只有 `proxies:` 段，不是完整 Mihomo 配置**（无 `mixed-port`/`external-controller`/`dns`/`tun`/`rules`/`proxy-groups`）。**完整配置必须由 Agent 生成**。 | R04 §6（实测 433 B） |
| C4 | `target` 合法值必须白名单映射：`clash`（小写）/`ALL` 实测 **500**；`Clash`/`ClashMeta`/`mihomo` 有效。 | R04 §3.2 |
| C5 | Sub-Store 后端 API **无认证**，源码默认绑 `::`（与文档所称 `127.0.0.1` 不符），数据目录必须预先存在否则启动崩溃。 | R04 §8、R05 §1 |
| C6 | **sub-store-convert 判定为 Rejected**：能力是 Sub-Store 严格子集（无 operator/`mergeSources`）；**失败面违反核心不变量**（网络不可达时仍 resolve 成功并输出 `proxies:\n` 9 字节 0 节点）；落后上游约 1 个月；**MIT 标称但内联 27 个 AGPL 源文件且零许可声明**。 | R06 §1、§7 |
| C7 | **许可证边界**：Sub-Store 为 AGPL-3.0。以"独立进程 + 仅 HTTP + 不修改源码"使用**不触发** §13、不传染；Bundled/Source Reuse 则触发源码提供义务。 | R13 §1、§3 |
| C8 | Sub-Store 官方 Release bundle（3.0 MiB）**零 npm 依赖**、冷启动 130 ms、空闲内存 60 MB → 部署成本极低。 | R05 §1 |
| C9 | 官方 All-in-One Server 与我们的 systemd 托管会形成**双 supervisor 冲突**（同类结论见 ADR-006）。 | R07 §4 |

---

## 2. Decision

### D1. 采用「Port + 两个实现 + 明确降级」的模型

```text
SubscriptionConverter (Port，定义在 application)
├── SubStoreConverter       ← MVP 默认实现（外部服务，可选部署）
└── NativeConverter         ← MVP 仅保留接口，返回 NotImplemented（最小直连解析兜底）
```

**判定：sub-store-convert = Rejected**（不作为 Primary/Secondary/Optional，不写 `SubStoreConvertAdapter`）。

理由（任一即足以拒绝）：能力是严格子集、失败面违反"失败不得破坏当前配置"的核心不变量、许可证阻塞（内联 AGPL 无声明）、维护为单作者且无 issue/release 流程。

### D2. Sub-Store 定位为「可选的外部组件」

- **默认不安装、不内嵌、不托管、不自动升级**。
- 用户以 Docker（`xream/sub-store`，`-p 127.0.0.1:3001:3001`）或 Node ≥22 + 官方 Release bundle 自行部署。
- Agent 通过可配置 `base_url`（可含 `BACKEND PREFIX`）接入；**默认只允许 loopback**。
- Agent 只做**版本检测与提示**，不自动升级（上游 JSON schema 迁移单向不可回滚，且 Sub-Store 不在关键路径）。

### D3. Agent 必须自己生成完整 Mihomo 配置

订阅转换的输出是**中间产物**（`proxies` 列表），不是最终配置：

```text
订阅源
  ↓   SubStoreConverter（Adapter 内部：写入 + 按名下载 + 解析 proxies）
代理节点集合（Domain 值对象）
  ↓   Agent 自身编排（必须自研）
补全运行参数：mixed-port / bind-address / external-controller(-unix) / secret / CORS 白名单
按能力注入：dns / tun（仅当 CapabilityProbe 判定可用）
生成：proxy-groups / rules / rule-providers
  ↓
完整 Mihomo Config（新 ConfigVersion）
```

**必须修正设计文档**：§12 与 §43 中 `GET /download/sub?target=ClashMeta&url=...` 的示例是**过时/错误的**，须改成两段式。

### D4. Adapter 契约

```rust
#[async_trait]
pub trait SubscriptionConverter: Send + Sync {
    async fn convert(&self, request: ConvertRequest) -> Result<ConvertedProxies, PortError>;
    async fn capabilities(&self) -> Result<ConverterCapabilities>;
}
```

- `ConvertRequest` 只表达业务意图（订阅源、UA、可选代理、`TargetFormat::Mihomo`），**不含任何 Sub-Store 参数名**。
- 错误枚举：`Unreachable` / `SubscriptionNotFound` / `UnsupportedTarget` / `InvalidRequest` / `EmptyOrInvalidOutput` / `InvalidOutput`。
- **所有错误 → 保留当前激活配置**（REQ-SUB-003）。
- Adapter 独占知识：端点是 `/download/:name`（非 `/download/sub`）、`target` 白名单、`url/ua/content/noCache` 参数名、`/api/subs` 写入细节、错误 JSON 形状。

### D5. 安全边界（不可协商）

- Sub-Store **绝不暴露到非 loopback**；Agent 的部署模板必须显式设置 backend host 为 `127.0.0.1`（源码默认 `::` 是陷阱）。
- doctor 必须把"Sub-Store 监听非 loopback"判为 `Misconfigured`。
- 不得把 Sub-Store 视为可信组件：其 `/api/utils/env` 会回显全部 `SUB_STORE_*` 环境变量（含凭据），**严禁整体入日志**。
- 订阅 URL 抓取的 SSRF 边界见 ADR-005（REQ-SUB-009）。

---

## 3. Alternatives

| 方案 | 描述 | 拒绝理由 |
|---|---|---|
| A. 只依赖 Sub-Store（原设计） | Application 直接依赖 `/download/sub` | 接口不存在（C1）；且依赖其内部有状态写入 |
| B. 引入 sub-store-convert 作为备用 | 按设计文档三优先级 | C6：能力子集 + 失败面破坏核心不变量 + 许可阻塞 |
| C. 自研完整 Native Converter 进 MVP | Rust 实现 URI 解析 + 规则生成 | 范围爆炸；上游已成熟；MVP 应聚焦编排与回滚（产品范围 WON'T） |
| D. 直接使用 subconverter | 生态标准工具 | 不在本项目 MVP 目标内；GPL-3.0 且非必要引入 |
| **E. 可选外部 Sub-Store + Port 抽象 + Agent 生成完整配置（选定）** | 本决策 | — |

---

## 4. Consequences

### 4.1 正面

- 转换器可替换：切换实现（或将来接 Native）不改 Application（REQ-SUB-006）。
- 订阅失败不影响当前代理（REQ-SUB-003），可用性风险被隔离在 adapter 内。
- 许可证义务最小化：Sub-Store 保持独立进程（R13）。
- 部署成本低：用户可完全跳过 Sub-Store（Native 直连降级）。

### 4.2 负面 / 成本

- Agent 必须实现"配置骨架补全 + proxy-groups + rules"逻辑，工作量不小（这是 C3 的直接后果）。
- 两段式写入意味着 Agent 需要在 Sub-Store 中维护订阅映射。**2026-09-12 修正**：实测 `PATCH /api/sub/:name` 会
  **连带更新引用该订阅的 collections / artifacts / files**，所以 Agent **只需保证注册名稳定**，
  **不需要**本地映射表，也不需要自己去改那些引用（重复做会与服务的维护打架）。
  注册名由**源 URL 的纯函数**派生（`proxy-agent-<16 位 hex>`），因后端拒绝含 `/` 的名字（实测 500 `INVALID_NAME`）。
  幂等靠 `PATCH` 先试、404 才 `POST`；**不用 `PUT /api/subs`**——它全量替换，共享实例上具破坏性。
- 需要在 doctor 中额外检测 Sub-Store 可达性、监听地址、版本。

### 4.2.1 实现状态（2026-09-12）

| 实现 | 状态 |
|---|---|
| `SubStoreConverter` | ✅ 已实现（`crates/infrastructure/src/subscription/substore.rs`） |
| `NativeConverter` | 未实现，按 D1 保留最小直连降级的接口位 |

真机验证（Debian aarch64 + Sub-Store **v2.39.6**）：
- `health()` 读出真实版本；一次转换从真实源得到 **2 个节点**
- 不可达源被分类为 `Unreachable`（而非 `InvalidRequest`），且源 URL 被 **`<redacted-url>` 脱敏**
- **幂等**：重复转换后库中记录数不变（4 → 4）

错误映射（实测对应）：`404` → `SubscriptionNotFound`；`500` + 远端抓取失败 → `Unreachable`；
空/零节点 → `EmptyOrInvalidOutput`；不支持的 target → `UnsupportedTarget`。

安全：新增 `ConverterConfig::External::allow_non_loopback`（默认 **false**）。后端**完全无认证**，
故非 loopback 一律拒绝，除非运维显式接受暴露面；adapter 自身**再次校验**，不依赖上层已检查。

### 4.3 必须遵守

```text
1. application 中不得出现 "download/"、"api/subs"、"target="、"sub-store" 等字符串（REQ-SUB-001）
2. 任何转换失败都不得替换激活配置（REQ-SUB-003）
3. Sub-Store 默认只绑 loopback；非 loopback 判 Misconfigured（ADR-005）
4. 不得内嵌 Sub-Store 源码或其前端产物（ADR-006、R13）
5. `target` 不得透传用户输入，必须白名单映射（C4）
```

---

## 5. Evidence

- `docs/research/04-sub-store.md` §1/§3/§4/§6/§7/§8（实测 404、433 B proxies、无认证、target 白名单）
- `docs/research/05-sub-store-deployment.md` §1（bundle 3.0 MiB / 60 MB RSS / `::` 默认绑定 / 数据目录陷阱）
- `docs/research/06-sub-store-convert.md` §1/§7（Rejected 判定与 5 条重新评估门槛）
- `docs/research/13-licenses.md` §1/§3（AGPL-3.0 进程隔离边界）
- `docs/research/07-metacubexd.md` §4（双 supervisor 冲突）
- 设计文档 §12、§43–§45（原策略，本 ADR 对其 §12/§43 提出修正）
