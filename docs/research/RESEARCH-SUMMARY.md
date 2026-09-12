# RESEARCH-SUMMARY — Phase 0 总纲

> 状态：v1.0（Phase 0 收口）
> 日期：2026-09-12
> 这是开发阶段的**唯一入口文件**。

**阅读顺序**：

```text
RESEARCH-SUMMARY.md   ← 你在这里
      ↓
docs/adr/ADR-001..006
      ↓
docs/research/requirements.md
      ↓
其余 R01–R15 作为依据与追溯资料
```

---

## 1. 一句话结论

> 面向 Linux Server / PVE LXC 的 Mihomo 管理 Agent 值得做，但**"管理 Mihomo"本身不是差异化**——metacubexd 已有 agent/supervisor、ShellCrash 与 mihari 等已覆盖大半。我们真正被验证为空白的能力是：**不可变配置版本库 + 原子激活与任意历史回滚 + PVE LXC 五值能力分层检测 + 统一 Doctor + 统一 Use Case + 最小权限与审计**。

---

## 2. Phase 0 的四个原问题与答案

| 问题 | 答案 |
|---|---|
| **现有项目解决了什么？** | Mihomo 提供数据面与完整 controller API（~45 route）；Sub-Store 提供订阅转换；metacubexd 提供 Dashboard，并已扩展到进程管理/配置校验/内核热切换（`packages/agent`）；ShellCrash 提供 Linux 侧一键安装与网络规则；mihari/mihomo-tui/clashtui/flclash-tui 等已实现 daemon + CLI/TUI + 配置原子写与失败回滚。 |
| **哪些能力应直接复用？** | Mihomo 内核与 controller；metacubexd 的**静态 UI 产物**；Sub-Store 的**转换服务**（外部进程）；Linux 内核的 TUN/netfilter。 |
| **哪些必须自己负责？** | 配置版本化与回滚编排、PVE LXC 能力检测与降级、Doctor、统一 Use Case、可替换转换器、最小权限与审计、内核二进制更新链路、systemd/init 抽象。 |
| **Domain / Port / Adapter 如何划分？** | 见 ADR-001（依赖方向与 crate 划分）、ADR-002/003（Port 拆分）、ADR-004（配置生命周期）、ADR-005（安全模型）、ADR-006（部署形态）。 |

---

## 3. 最重要的十个研究结论（每条都可追溯）

| # | 结论 | 证据 | 影响 |
|---|---|---|---|
| **C1** | **Unix socket controller 完全不校验 secret，且被硬编码 `chmod 0666`** | R01/R09/R12 三方独立 + 本次会话源码复核（`server.go startUnix()`） | 同机任意用户可控制内核 → 必须补偿控制（ADR-005 → REQ-SEC-002） |
| **C2** | **`secret` 为空 = 无鉴权；CORS 默认 `*` + private-network** | R12 + 本次复核 `config/config.go:595-598` | 生成配置必须强制 secret + 收窄 CORS（ADR-005 → REQ-SEC-001/011） |
| **C3** | **`/download/sub` 不存在**；Sub-Store 是"入库 + 按名下载"的两段式，且**只输出 `proxies:` 段** | R04 实测（404；433 B 仅 proxies） | 设计文档 §12/§43 必须修正；Agent 必须自己生成完整配置（ADR-002 → REQ-SUB-011/013） |
| **C4** | **sub-store-convert 判定 Rejected**：能力子集 + 失败时输出空 `proxies:`（9 B/0 节点）+ 标称 MIT 却内联 AGPL 无声明 | R06 §1、R13 §1 | 不实现该 Adapter（ADR-002 D1） |
| **C5** | **metacubexd 已含 supervisor + 内核热切换**，其 all-in-one server 会与我们的 systemd 形成双 supervisor | R07 §1/§4 | 只内嵌静态产物 + 同源反代（ADR-006 D1） |
| **C6** | **TUN = `/dev/net/tun` 可用 ∩ `CAP_NET_ADMIN`**；设备存在且 `open()` 成功仍可能 `TUNSETIFF EPERM` | R10 §1（容器实测） | 能力判定禁止用 bool；必须区分 `Misconfigured`（REQ-LXC-001/009） |
| **C7** | **TProxy/redirect 只需 `CAP_NET_ADMIN`**，但隐藏门槛是 **sysctl 可写**（`ip_forward`/`route_localnet`） | R10 C4、R11 C5 | doctor 必须单列 sysctl 探测项（REQ-LXC-010） |
| **C8** | **TUN 与 TProxy 前置条件完全不同**：TUN 由内核自身写路由，TProxy 必须外部规则 + 策略路由；`auto-redirect` ≠ TPROXY | R11 C1/C2/C3 | MVP 只做检测；TProxy 自动化归 `Later`（ADR-004 关联、REQ-NET） |
| **C9** | **Mihomo `/upgrade` 与 `/restart` 不可依赖**；`PUT /configs` 解析失败时失败安全（旧配置保留），但**空 body 会 400** | R01 §1 实测 | 内核更新自研；reload 必须带 JSON body（ADR-003 D2、REQ-MIHOMO-013） |
| **C11** | **⚠️ `PUT /configs?force=true` 是数据面杀手**：解析通过但 listener 绑定失败时，会拆除旧监听器、留下 `mixed-port: 0` 僵尸态，**后续 reload 无法恢复、必须重启**。回滚因此必须用 restart 而非 reload | R02 §1 C5/C6 实测 | **禁止 `force=true`**；回滚用 restart（ADR-004 D3/D5 → REQ-CONFIG-011/013） |
| **C12** | **`mihomo -t` 不是纯 dry-run，且有两个假阴性/假阳性**：① 不检测未知字段（`mixed-portt` 拼错 **exit 0 通过**）→ L2 必须有 Agent 字段白名单；② `-t -f <不存在的文件>` **exit 0 "成功"**（自动创建初始配置）；③ 含 GEOIP/GEOSITE 时**真实联网下载 geodata**（阻塞约 90s） | R02 §1 C2/C3（本会话已独立复现 ②） | L2 = `-t` ⊕ 字段白名单；调用前必须确认文件存在；离线需预置 geodata（ADR-004 D2 → REQ-CONFIG-005/012） |
| **C13** | **上游不存在任何官方机器可校验 schema**（1222 项完整源码树无 schema 文件；官方文档 "schema" 0 次命中），事实 schema 只有 `config.go` 的 yaml tag | R02 §7 | L2 不得等待上游 schema（回答 Q001） |
| **C14** | **离线环境含 `GEOIP` 规则会让 Mihomo fatal 退出**（无法下载 `geoip.metadb`） | R14 新增实测 | 离线 PVE LXC 必须预置 geodata；L0 资源预检拦截（REQ-CONFIG-005） |
| **C10** | **竞品已覆盖"配置原子写 + 失败回滚"**（flclash-tui、mihari、metacubexd agent），但**无人提供不可变多版本历史 + `list/diff/activate/rollback`** | R15 §1 | 差异化必须建立在版本库 + LXC 能力分层 + Doctor + 审计上（产品范围 §1） |

---

## 4. 架构决策（ADR 索引）

| ADR | 决策 | 关键约束 |
|---|---|---|
| [ADR-001](adr/ADR-001-architecture.md) | DDD-lite + Hexagonal + Modular Monolith；5 crate；依赖方向不可逆；Domain 纯同步 | interfaces → application → domain；infrastructure → ports；bootstrap 唯一组装点 |
| [ADR-002](adr/ADR-002-subscription-provider.md) | Converter Port + `SubStoreConverter`（可选外部）+ `NativeConverter`（MVP 仅接口）；**sub-store-convert Rejected** | Application 不得出现 Sub-Store 参数名；失败必须保留旧配置 |
| [ADR-003](adr/ADR-003-mihomo-integration.md) | 拆 `MihomoController` / `MihomoObserver` / `MihomoConnectionOps` / `ProcessManager`；禁 `/restart` `/upgrade`；unix socket + 收紧权限 | 就绪判定分层；连接/流量明细不进 Domain |
| [ADR-004](adr/ADR-004-config-lifecycle.md) | 不可变配置版本库 + 唯一激活路径 + 三层校验 + 失败自动回滚；内核更新与配置更新分离 | reload 带 JSON body；L4 端口检查；active 版本永不删 |
| [ADR-005](adr/ADR-005-security-model.md) | 四层信任边界；Mihomo 三重加固（loopback + secret + CORS）；unix socket 0660；远程 Bearer token；五值能力状态 | 非 loopback + 无 token ⇒ 拒绝启动；日志脱敏强制 |
| [ADR-006](adr/ADR-006-deployment-model.md) | **默认 Model D**：Agent + Mihomo + 可选 Sub-Store + metacubexd 静态产物；deb 主分发；不做官方镜像 | 不采用 metacubexd agent/all-in-one；三条独立升级链路 |
| [ADR-007](adr/ADR-007-metadata-persistence.md) | 元数据驱动选 `rusqlite` + `bundled`（偏离 `AGENTS.md` 的 `sqlx` 基线）；连接池而非全局锁；Domain 增加受校验的重建入口 | 读不出来必须报错、不得默认值；审计用结构化列不用显示 label；Domain 仍只依赖 `thiserror` |
| [ADR-008](adr/ADR-008-kernel-acquisition-and-field-validation.md) | 内核二进制只从上游 release 直连（不收口镜像）；校验用 GitHub API 的 asset `digest`；`mihomo -t` 必须在隔离目录跑；字段白名单由上游源码生成 | 无 digest 即拒绝安装；digest 覆盖压缩产物而非解压后二进制；未知字段报告而非拒绝；unsafe 收进 `proxy-sys` |
| [ADR-009](adr/ADR-009-real-adapter-composition.md) | 真实适配器装配：pool 与 secret 由 composition root 预构造后作为纯数据注入；目录权限分 `Required`/`Preferred` 两档并接受 sticky 目录；未实现 port 显式报错 | `AdapterFactory` 不接收 `&AppContext` 的约束保留；`StartOptions` 的配置路径从**激活指针**解析（不得写死文件名）；首次端到端真机联通 |
| [ADR-010](adr/ADR-010-cli-client-and-agent.md) | CLI 形态：**单二进制两个角色**（`proxyctl <cmd>` / `proxyctl agent run`）；**socket 是唯一通路，无 `--direct`**；退出码 0–7 划分；`--json` 原样透传响应体；`logs` 明确报未实现（退出码 7） | `client/**` 不得命名 application/domain/bootstrap（架构测试强制）；客户端路径与服务端路由表逐条比对；单二进制体积代价已实测（aarch64 release 8.7 MB）并记录 |

---

## 5. 交付物清单（Phase 0 DoD）

### 5.1 Research（15 份）

| 文件 | 主题 | 状态 |
|---|---|---|
| `01-mihomo.md` | Mihomo Controller API inventory（~45 route 实测冻结） | ✅ |
| `02-mihomo-config.md` | 配置生命周期、`mihomo -t` 覆盖范围、schema 问题 | ✅ |
| `03-mihomo-runtime.md` | 进程模型、信号语义、就绪、Unix socket、健康分层 | ✅ |
| `04-sub-store.md` | Sub-Store 公开 API（两段式模型、target 白名单） | ✅ |
| `05-sub-store-deployment.md` | 部署方案与资源实测（3.0 MiB / 130 ms / 60 MB） | ✅ |
| `06-sub-store-convert.md` | 评估 → **Rejected** | ✅ |
| `07-metacubexd.md` | 能力分析与复用策略（静态产物 + 同源反代） | ✅ |
| `08-shellcrash.md` | Feature Reverse Engineering（Reuse/Improve/Ignore） | ✅ |
| `09-linux-runtime.md` | systemd 集成（单 unit / 静态用户 / 两档 hardening） | ✅ |
| `10-pve-lxc.md` | PVE LXC 能力矩阵 + 探测清单 + 真机补测清单 | ✅ |
| `11-network-stack.md` | TUN/TProxy 前置条件 + MVP 四分类 | ✅ |
| `12-security.md` | Threat Model + Trust Boundary + 权限模型 + 32 条 checklist | ✅ |
| `13-licenses.md` | License Matrix + 合规清单 + 17 项待确认 | ✅ |
| `14-deployment-model.md` | 四模型对比 + 默认 Model D | ✅ |
| `15-competitors.md` | 竞品 Feature Matrix + 差异化分析 | ✅ |

### 5.2 Product

| 文件 | 状态 |
|---|---|
| `capability-matrix.md`（谁已经会什么 / 我们负责什么） | ✅ |
| `product-scope.md`（MUST / SHOULD / WON'T + 降级模型） | ✅ |
| `requirements.md`（87+ 条可验证 REQ，含验证方式） | ✅ |
| `open-questions.md`（25 条，架构阻塞项已全部 RESOLVED） | ✅ |

### 5.3 Architecture

| ADR | 状态 |
|---|---|
| ADR-001 架构 | ✅ |
| ADR-002 订阅提供方 | ✅ |
| ADR-003 Mihomo 集成 | ✅ |
| ADR-004 配置生命周期 | ✅ |
| ADR-005 安全模型 | ✅ |
| ADR-006 部署模型 | ✅ |

---

## 6. 对现有设计文档的修正项（重要）

Phase 0 调研**推翻了设计文档中的若干假设**，实现前必须按下列修正执行：

| # | 设计文档位置 | 原假设 | 修正 |
|---|---|---|---|
| M1 | §12、§43 | `GET /download/sub?target=ClashMeta&url=...` 是 Sub-Store 公开接口 | **该端点不存在**。改为两段式：`POST /api/subs`（幂等写入）+ `GET /download/:name?target=<白名单>` |
| M2 | §12 | Sub-Store 产出可直接作为 Mihomo Config | Sub-Store **只输出 `proxies:` 段**；完整配置（端口/controller/secret/dns/tun/groups/rules）**必须由 Agent 生成** |
| M3 | §12 优先级 | 优先级：Sub-Store → sub-store-convert → Native | sub-store-convert **Rejected**（能力子集 + 失败面破坏不变量 + 许可阻塞） |
| M4 | §29、§47 | "直接提供 metacubexd 静态文件" | 需升级为**静态托管 + 同源 Clash API 反代**（避免 CORS/`external-ui`/浏览器持有 secret）；**不启用**其 agent/all-in-one 形态 |
| M5 | §22、§21 | LXC 能力检测（隐含 bool 语义） | 必须用**五值枚举**；`/dev/net/tun` 存在 ≠ TUN 可用（需 `ioctl(TUNSETIFF)` 验证） |
| M6 | §30 | Unix socket 不校验 secret（已知） | 补充：socket 还被硬编码 `chmod 0666` → Agent **必须**主动收紧权限，否则同机任意用户可控制内核 |
| M7 | §17 | reload 失败保留旧配置（方向正确） | 补充实现细节：`PUT /configs` **必须带 JSON body**（空 body 400）；**禁止 `force=true`**；**回滚必须用 restart 而非 reload**；必须用 `path` 模式且受 `SAFE_PATHS` 约束；健康检查必须含**代理端口**（bind 失败不致命、`/version` 仍 200） |
| M12 | §41 L2 校验 | 假设 `mihomo -t` 可作为语义门禁 | **`-t` 不检测未知字段**（拼错字段 exit 0 通过）→ L2 必须补 **Agent 字段白名单**；且 `-t` 有副作用（geodata 下载）与假阳性（不存在的文件 exit 0），调用需隔离目录 + 文件存在性检查 |
| M13 | §41 校验层级 | 三层（语法/语义/启动） | 需新增 **L0 资源预检**（端口可用性 / geodata 就绪 / provider 可达）；离线含 GEOIP 配置会 fatal（R14 实测） |
| M8 | §38 | 内核更新（未定） | 明确**禁止依赖** Mihomo `/upgrade` 与 `/restart`；Agent 自研"下载 + 校验 + 原子替换 + 重启 + 健康检查 + 回滚" |
| M9 | §23 | `proxy-agent.service` | 补充：**单 unit、不双 unit**；静态用户 `proxy-agent`（**不用 `DynamicUser=`**）；`NoNewPrivileges=` 与 `AmbientCapabilities=` 可共存（实测+源码+man 三重证据）；hardening 必须两档（`PrivateDevices=yes` 会让 `/dev/net/tun` 消失） |
| M10 | 产品命名 | 产品名含 "Mihomo 管理 Agent" | Mihomo README 有命名限制（下游项目名不得含 `mihomo`）→ 包名/二进制用 `proxy-agent`/`proxyctl`，文档描述性引用可保留 |
| M11 | §45 | Native Converter 状态 `NotImplemented` | 保持；但需明确它是**唯一无外部依赖的兜底**（Sub-Store 缺失时的降级路径） |

---

## 7. 已知风险与未验证项（必须带入实现阶段）

### 7.1 环境导致的证据等级限制

| 限制 | 影响范围 | 处理 |
|---|---|---|
| 宿主为 macOS，**无真实 systemd PID 1** | R09、R03、R12 的 unit 行为 | ✅ **2026-09-12 已部分收口**：接入 Debian forky（systemd 261，PID 1 = systemd）实测，验证了 `AmbientCapabilities` 生效、TUN 三段判定、mihomo socket `0666` + 不校验 secret、`chmod 0660` 缓解措施可行；并**发现 `NoNewPrivileges=`/`PrivateDevices=` 在 LXC 中静默失效**（已回写 ADR-005 D7b）。仍未验证真实 PVE LXC 的 privileged/unprivileged 差异 |
| **无真实 PVE 主机** | R10 的 PVE 部分 | 已给 P1–P30 补测清单；需真机验证 `lxc.cgroup2.devices.allow`、`cap.drop`、`/proc/sys` 可写性、ambient cap |
| **Docker Hub 与镜像源不可达** | 容器镜像相关结论 | 用本地缓存镜像完成 TUN/CAP/nftables 实测；镜像体积/容器内存未测 |
| `github.com` 直连超时；GitHub API 限流耗尽 | 元数据抓取 | 改用 `raw.githubusercontent.com` + `ghfast.top` 代理；关键结论均交叉验证 |

### 7.2 必须在对应里程碑前收口的开放问题

```text
Q016          → 内嵌前（metacubexd 的 Highcharts 专有许可）

> **Q015 已于 2026-09-12 收口**：默认不经镜像，只从上游 release 直连；实测发现上游**不发布 `.sha256` 文件**，校验改走 GitHub API 的 asset `digest`。
Q020         → Config L2 校验实现前（mihomo -t 是否真的无副作用）
Q023         → SubscriptionConverter Adapter 实现前（是否必须写入 Sub-Store 库）
Q009 / Q004  → Linux / PVE 里程碑前（真机补测）
Q012 / Q024  → 网络能力里程碑前（SSRF 边界；sysctl 可写性）
```

完整清单见 `docs/research/open-questions.md`（25 条，其中架构阻塞项已全部 `RESOLVED`）。

---

## 8. 进入实现的下一步

> **详细类型与 trait 设计已产出**：`docs/design/domain-ports-bootstrap.md`
> （Domain 4 个有界上下文的类型、9 个 Port trait、Bootstrap 组装与启动期安全硬校验；
> 含 `async_trait` 的 dyn 兼容性实证与 typestate 校验门禁的正/负向编译验证）

```text
① Domain Model
   - mihomo: MihomoInstance / MihomoInstanceId / MihomoVersion / MihomoRuntimeStatus
   - configuration: ConfigVersion / ConfigVersionId / ConfigSource / ConfigChecksum / ValidationResult
   - subscription: Subscription / SubscriptionId / SubscriptionSource / ConverterId / Schedule
   - system: Platform / Architecture / InitSystem / ContainerEnvironment / CapabilityStatus(五值) / NetworkCapabilities
   ⚠ 必须从第一天带 MihomoInstanceId（多实例预留），即使 MVP 单实例

② Application Use Cases
   StartMihomo / StopMihomo / RestartMihomo / ReloadMihomo / UpdateMihomoKernel
   UpdateSubscription / ActivateConfig / RollbackConfig / ValidateConfig / RunDoctor
   ⚠ ActivateConfig 是唯一激活路径；RollbackConfig 必须复用它

③ Ports（小而聚焦）
   MihomoController / MihomoObserver / MihomoConnectionOps / ProcessManager
   SubscriptionConverter / ConfigRepository / CapabilityProbe / NetfilterManager(延后)
   InitSystem(ServiceManager) / SecretStore

④ 骨架与质量门
   cargo fmt / clippy -D warnings / test / build + cargo deny（allow-list，默认拒绝未列出许可）

⑤ 最先写测试的三条路径（核心不变量）
   订阅更新失败保留旧配置 / 激活后健康检查失败自动回滚 / 并发 start 只 spawn 一次
```

---

## 9. 最终核心原则（实现期不得违反）

```text
1. 失败的更新、转换、reload、能力探测 —— 只能降级，绝不能破坏当前可用配置或停机
2. 能力必须被检测，而不是被假设；能力状态是五值，不是 bool
3. 配置是不可变版本；激活只有一条路径；回滚复用该路径
4. 内核更新与配置更新彻底分离
5. 外部组件可替换（Sub-Store / metacubexd / init system）
6. 特权操作映射到显式 Use Case + Port；Web 层绝不持有 root shell
7. 默认不暴露：controller 绑 loopback/unix socket，Agent API 绑 loopback
8. 日志与审计默认脱敏
9. 复杂配置编辑放 Web，TUI/CLI 保自动化友好（`--json` + 退出码）
10. 上游复用优先于重写；许可证边界决定复用方式（进程隔离 vs 内嵌）
```

---

## 10. 证据与可追溯性

- 15 份调研文档均在文末给出**证据与来源**与**未验证假设**章节。
- 每条关键结论标注 `[实测]` / `[实测-容器]` / `[上游源码]`（附文件路径）/ `[上游文档]` / `[推测]` / `[未验证]`。
- 涉及上游行为的**安全关键结论**（unix socket 免鉴权 + 0666、CORS 默认值、secret 空值语义）已由**至少两方独立复核**（专项调研 subagent + 本会话直接抓源码）。
- 全部实测均在 `/tmp` 与本地端口完成，未修改宿主网络配置；遗留的调研进程已清理。
