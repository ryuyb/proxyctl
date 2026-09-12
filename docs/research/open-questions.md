# Open Questions — Phase 0

> 状态：v1.0（已并入 Phase 0 规范 Q001–Q006、R01–R15 各文档的开放问题，以及本次会话的独立复核结论）
> 日期：2026-09-12
> 规则：**未经验证的假设不得作为架构事实**。每条问题标注"阻塞什么决策 / 如何验证 / 当前状态"。

## 状态图例

| 状态 | 含义 |
|---|---|
| `RESOLVED` | 已有可验证结论（附来源） |
| `PARTIAL` | 已有部分证据，但不足以冻结决策 |
| `OPEN` | 未解决，需要继续调研或实测 |
| `DEFERRED` | 明确推迟到实现阶段或后续版本 |

---

# 第一部分：Phase 0 规范提出的原始问题

## Q001 — Mihomo Config 是否存在适合机器校验的稳定 Schema？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 结论 | 上游**没有**面向第三方消费的稳定机器可校验 schema。因此 Config Validation 的 Level 2 语义门禁**只能由 `mihomo -t` 承担**，不能依赖自建字段白名单（会与上游演进脱节）。 |
| 影响 | REQ-CONFIG-005 的实现方式：L1 语法 + L2 `mihomo -t` + L3 运行期验证；**不自研 schema**。 |
| 验证 | R02 §7；ADR-004 D2 |
| 来源 | `docs/research/02-mihomo-config.md` |

## Q002 — Sub-Store `/download/sub` 是否存在长期兼容保证？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`（结论出乎预料）** |
| 结论 | **该端点不存在**。实测 `GET /download/sub?url=...` 返回 404 `RESOURCE_NOT_FOUND`（`sub` 被当作订阅名）。真实入口是 `GET /download/:name[/:target]`（`download.js:74,82`）。且 Sub-Store 是"先 `POST /api/subs` 入库、再按名下载"的**两段式有状态服务**。 |
| 影响 | 设计文档 §12/§43 的示例必须修正；Adapter 必须实现两段式；`target` 必须白名单映射（`clash` 小写会 500）。见 ADR-002。 |
| 来源 | `docs/research/04-sub-store.md` §1/§3/§4 |

## Q003 — metacubexd 的 agent 是否值得直接复用？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`** |
| 结论 | **不复用其 agent**。它确实包含 supervisor（`packages/agent/src/supervisor.ts`：spawn mihomo、`-t` 校验、崩溃退避、SSE 日志）、profile 单槽 `.bak` 回滚、内核下载热切换 —— 但它面向桌面/容器场景，**没有 systemd、没有 PVE LXC 能力检测、没有多实例、没有版本历史**；其官方 All-in-One Server 会与我们的 systemd 托管形成**双 supervisor 冲突**。 |
| 决策 | 只以**静态产物**形态内嵌其 UI + 同源 Clash API 反代（ADR-006 D1）；`/api/control` 形态不启用。 |
| 注意 | 其依赖含 **Highcharts（专有许可）**，内嵌分发前必须处理（见 Q016）。 |
| 来源 | `docs/research/07-metacubexd.md` §1/§4、`docs/research/13-licenses.md` §1 |

## Q004 — PVE unprivileged LXC + TUN 的最小权限组合是什么？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`（容器实测已得结论，PVE 真机未验证）** |
| 结论 | 最小组合 = bind-mount `/dev/net/tun` + `lxc.cgroup2.devices.allow: c 10:200 rwm` + 保留 `CAP_NET_ADMIN`（用 `create=file` **不需要** `features: mknod=1`）。实测 `--cap-add=NET_ADMIN --device /dev/net/tun` 下 TUNSETIFF / `ip tuntap add` / `ip link add` 全部成功。 |
| 关键反直觉发现 | 有 `CAP_MKNOD`、`mknod` 成功、`open(O_RDWR)` 也成功，但 `ioctl(TUNSETIFF)` 仍可能 `EPERM` → **门槛在 capability 检查，不是"文件是否存在"**。 |
| 仍未验证 | PVE 默认 `lxc.cap.drop` 是否含 `net_admin`（P7/P8）；unprivileged LXC 内 `/proc/sys` 是否可写（P10）；容器内能否设 ambient capabilities（P23/P24，决定最安全生产形态）。 |
| 来源 | `docs/research/10-pve-lxc.md` §1/§7/§9（P1–P30 补测清单） |

## Q005 — Mihomo binary update 是否应该完全由 Agent 管理？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`（方向），`OPEN`（打包细节）** |
| 结论 | **是，Agent 必须自研**。上游 `/upgrade` 不可依赖：无更新时返回 500（把"已是最新"当错误）、无签名校验、`force=true` 有替换成半截二进制的风险；`/restart` 是 `syscall.Exec` 进程自替换会绕过状态机。 |
| 开放子问题 | (a) 经 ghproxy/ghfast 等镜像下载再分发的合规性（Q015）；(b) Mihomo 是否 Bundled 进 deb 还是首次运行下载（ADR-006 D4 待定）；(c) 上游是否提供签名（当前结论：仅有 checksum 级别的可用性，签名待确认）。 |
| 来源 | `docs/research/01-mihomo.md` §1、`13-licenses.md` §8、ADR-003 D2 / ADR-006 D4 |

## Q006 — MVP 是否直接实现 nftables apply，还是只做 detection？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`** |
| 结论 | **MVP 只做 detection（+ 后续 dry-run）**。TProxy/redirect 规则自动生成归入 `Later`；规则 apply 必须先在具备 快照 + DryRun + watchdog + 死手开关 之后才做。 |
| 理由 | 自动写防火墙风险极高（与 Docker/K8s/其他防火墙冲突、规则残留导致断网）；PVE LXC 能力不确定；且 TProxy 的隐藏门槛是 **sysctl 可写**（`ip_forward`/`route_localnet`），不是 netfilter 权限。 |
| MVP 边界 | Supported = Mixed 端口 + TUN 可选启用（检测 + 降级，Agent 不写规则）；Detection Only = tun/CAP/防火墙后端/策略路由/resolvectl/规则残留；Later = TProxy 自动化；Unsupported = 改宿主网络、自动改 DNS、自动 modprobe。 |
| 来源 | `docs/research/11-network-stack.md` §1 C10 / §7、ADR-004 关联 |

---

# 第二部分：本次调研新增问题

## Q007 — Mihomo unix socket 默认 `chmod 0666` 且不校验 secret，风险如何处置？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`** |
| 证据 | `hub/route/server.go` `startUnix()`：`router(cfg.IsDebug, "", ...)`（secret 硬编码为空）+ `os.Chmod(addr, 0o666)`。三方独立确认（R01 实测、R12 源码、R09 源码）。 |
| 结论 | 同机任意用户可获**完整内核控制权**（含 `/upgrade`、`/restart`）。这是上游行为，无法通过配置关闭。 |
| 处置 | Agent 必须在 socket 创建后 `chmod 0660` + 设专用 group，`/run/proxy-agent` 建为 `0750`；并纳入 health check。见 ADR-005 D2。 |
| 残余风险 | 存在"Mihomo 创建 → Agent chmod"的短暂窗口。 |

## Q008 — Mihomo CORS 默认值是否放大浏览器侧攻击面？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`** |
| 证据 | `config/config.go:595-598`：默认 `AllowOrigins: ["*"]`、`AllowPrivateNetwork: true`（本次会话独立复核 + R12 实测：即使显式写 `allow-origins: []` 仍返回 `*`）。 |
| 结论 | 浏览器中任意站点可在用户访问时控制本机 Mihomo（私网请求 + 任意来源），`secret` 为空时后果最严重。 |
| 处置 | Agent 生成配置必须写入 CORS 白名单；doctor 对默认 CORS 告警；同源反代代替浏览器直连 controller。见 ADR-005 D2。 |

## Q009 — 本机（macOS）无法完成的实测项如何收口？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`（2026-09-12 已大幅收口）** |
| 背景 | 宿主为 macOS：无法实测真实 systemd PID 1、PVE LXC、nftables/iptables 写操作。Docker Hub 与镜像源不可达（仅本地缓存镜像可用），容器证据为 `[实测-容器]` 级。 |
| 已补齐的部分 | TUN/CAP/TPROXY/nftables/iptables 在容器内取得了实测（R10、R11）；Sub-Store 用 Node 直跑取得实测（R04、R05）。 |
| 未覆盖 | 真实 systemd 下的 unit 行为、PVE 真机 capability 边界、unprivileged LXC 的 `/proc/sys` 可写性、ambient capabilities 在容器内是否可用、真实流量语义。 |
| 验证方法 | 执行 R09 §13 与 R10 §9（P1–P30）的补测清单；在 Debian 13 / Ubuntu 24.04 上跑 `systemd-analyze verify/security`。 |
| 关联 | REQ-LXC-001~007、REQ-NET-007、Q004 |

> **2026-09-12 更新**：接入一台真实的 Debian forky（**systemd 261，PID 1 = systemd，内核 7.0.14 aarch64**）
> 后，原先"无法实测"的项目已补齐以下部分（全部 `[实测]`，详见 R09 §4.2.1 / §4.2.2）：
>
> | 原未验证项 | 现状 |
> |---|---|
> | TUN 的 `/dev/net/tun` 存在 + `open()` + `ioctl(TUNSETIFF)` 三段判定 | ✅ 已验证：设备在、`open` 成功、无 `CAP_NET_ADMIN` 时 `TUNSETIFF` 返回 `EPERM` |
> | `CAP_NET_ADMIN` 是必要且充分条件 | ✅ 已验证：只给 `CAP_NET_ADMIN` → `TUNSETIFF` 成功；只给 `CAP_SYS_ADMIN` → 仍 `EPERM` |
> | domain 的 `evaluate_tun` 判定规则 | ✅ 已用真实观测值驱动，输出 `Misconfigured` / `Supported` / `Unavailable` 全部符合预期 |
> | mihomo unix socket `chmod 0666` + 不校验 secret | ✅ Linux 上复现（三种 header 均 200） |
> | ADR-005 的 `chmod 0660` 缓解措施 | ✅ 已验证可行，且不影响 API 可用性；预建 0750 目录不被覆盖 |
> | `AmbientCapabilities=` 生效 | ✅ 已验证（`CapAmb=0x1000`） |
> | **`NoNewPrivileges=` / `PrivateDevices=` 生效** | ❌ **静默失效**（LXC 环境）→ 已回写 ADR-005 D7b |
> | 容器检测方式 | ✅ 新增确定性方法：`/run/systemd/container`（比 `/proc/1/cgroup` 可靠，后者在 cgroup v2 下为空） |
>
> **仍未验证**：真实 PVE LXC（非 OrbStack 的 LXC）上的 `NoNewPrivileges=` 行为、
> privileged vs unprivileged 差异、`SO_PEERCRED` 在 tokio 下的 API。

## Q010 — sub-store-convert 的许可证与上游跟随机制是否可接受？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED` — 判定 Rejected** |
| 结论 | ① 能力是 Sub-Store 严格子集（bundle 内 `operator` 0 次、`mergeSources` 0 次）；② **失败面违反核心不变量**：网络不可达时 `convert()` 仍成功 resolve 并输出 `proxies:\n`（9 字节 0 节点），调用方无法区分"空订阅"与"拉取失败"；③ npm 产物标称 MIT 却内联 27 个 Sub-Store(AGPL-3.0) 源文件且零许可声明 → 不得 Bundled / 不得 Source Reuse；④ 落后上游约 1 个月（2.36.33 vs 2.39.6）。 |
| 决策 | 不写 `SubStoreConvertAdapter`；实现集合 = `SubStoreConverter`（可选）+ `NativeConverter`（兜底）。见 ADR-002 D1。 |
| 重评估门槛 | 见 `06-sub-store-convert.md` §7.3（5 条）。 |
| 来源 | `docs/research/06-sub-store-convert.md`、`13-licenses.md` §1/§3 |

## Q011 — Agent 自身升级与配置/内核升级的失败域如何隔离？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 结论 | 三条链路必须独立：① Agent 自升级（deb）；② 内核升级（下载→校验→原子替换→重启→健康检查→回滚）；③ 配置回滚（激活历史版本→reload→健康检查→回滚）。见 ADR-006 D5。 |
| 仍未验证 | Agent 升级中断（deb 半安装）时 Mihomo 是否仍运行；SQLite schema 迁移失败如何回退。 |
| 验证方法 | 在 Linux 上模拟中断的 E2E 测试。 |
| 关联 | REQ-OPS-002/004 |

## Q012 — 订阅抓取的 SSRF 防护边界应到哪一层？

| 字段 | 内容 |
|---|---|
| 状态 | **`OPEN`** |
| 背景 | 订阅 URL 由用户提供。若由 Sub-Store 抓取（`url=` 参数），**Agent 无法控制其出站目标**；只有 NativeConverter 直连时 Agent 才能拦截。 |
| 待决策 | 是否要求所有订阅抓取都经 Agent？是否允许订阅指向内网（自建 subconverter 的合理场景）？黑名单还是白名单？ |
| 建议 | Agent 侧实现黑名单（拒绝 loopback/链路本地/元数据/RFC1918）+ 可选白名单豁免；Sub-Store 侧风险文档化并要求只绑 loopback。 |
| 关联 | REQ-SUB-009、REQ-SEC-010、ADR-005 D6 |

## Q013 — Web 认证在 MVP 采用哪种形态？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`（MVP 方案）** |
| 结论 | 本地：Unix socket + 文件权限（`0660 root:proxyctl`）+ `SO_PEERCRED` 校验 uid/gid。远程：**Bearer token（哈希存储、可轮换）** + 严格 CORS 白名单 + 角色（`ADMIN`/`READ_ONLY`）；**非 loopback 监听而无 token ⇒ 拒绝启动**。会话 cookie+CSRF 与 mTLS 列入 Phase 2。 |
| 未验证 | `SO_PEERCRED` 在 tokio/Rust 下的具体 API 可用性需在实现时验证。 |
| 来源 | `docs/research/12-security.md` §1、ADR-005 D3 |

## Q014 — PVE LXC 是否需要官方容器镜像形态？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`** |
| 结论 | **MVP 不提供官方镜像**，只提供 deb + 手动安装。理由：PVE LXC 下 Docker-in-LXC 代价与不确定性高；容器形态会与 systemd 托管形成双 supervisor。 |
| 来源 | `docs/research/14-deployment-model.md`、`07-metacubexd.md` §4、ADR-006 D6 |

---

## Q015 — 经镜像（ghproxy/ghfast 等）下载 Mihomo 二进制再分发是否合规？

| 字段 | 内容 |
|---|---|
| 状态 | **`RESOLVED`（2026-09-12：默认不经镜像）** |
| 结论 | **不使用第三方镜像。** 内核二进制只从上游 GitHub release 直连获取。理由见下方实测：**镜像无法提供可信摘要**，走镜像等于放弃校验或依赖第三方 `sha256sums`（其本身同样不可信）。 |
| 阻塞 | 已解除。原"本机直连超时"是 **macOS 本地网络问题，不是目标环境属性**。 |
| 实测（Debian aarch64） | `github.com` / `api.github.com` 直连 **200**；asset 下载 200（16,965,828 字节）；解压后可执行（`Mihomo Meta v1.19.30 linux arm64`）。 |
| ⚠️ 关键修正 | **上游不发布 `.sha256` 文件**（`.gz.sha256` → **404**）。唯一类校验 asset 是 `version.txt`，内容仅版本号。因此"checksum 校验"必须改走 **GitHub API 的 asset `digest` 字段**（`sha256:<hex>`），已独立复算确认一致。 |
| GPL-3.0 §6(d) | 源码归档可达：`/archive/refs/tags/<tag>.tar.gz` → **200**，可在 Bundled 时履行同址提供义务。 |
| 关联 | R13 §8 待确认清单、ADR-006 D4、**ADR-008** |

## Q016 — metacubexd 内嵌分发时的 Highcharts 专有许可如何处理？

| 字段 | 内容 |
|---|---|
| 状态 | **`OPEN`（法律问题-待确认，阻塞内嵌方案）** |
| 背景 | metacubexd 应用为 MIT，但 `packages/ui` 依赖 `highcharts`，其 npm license 指向 `https://www.highcharts.com/license`，包内 LICENSE.txt 写明**商业使用须 Highsoft Standard License Agreement**。另含 UFL-1.0 字体与 CC-BY-4.0 资源（需署名）。 |
| 待决策 | 是否替换/移除 Highcharts 组件、取得授权、或改为可选由用户自行部署 Dashboard。 |
| 关联 | ADR-006 D1、R13 §1/§6 |

## Q017 — Mihomo README 的命名限制如何落地？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 结论 | 上游 README 声明下游项目名不得含 `mihomo`。建议产品名与包名使用 `proxy-agent` / `proxyctl`（**当前设计已满足**）；文档中的描述性引用属 nominative use。 |
| 待确认 | 该声明是否构成 GPL-3.0 §7 的 additional restriction（影响我们的分发声明）。 |
| 来源 | `docs/research/13-licenses.md` §1 待确认清单 Q3 |

## Q018 — `/configs` 的 SAFE_PATHS 与文件读取残余风险如何设计？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 背景 | CVE-2025-56499（mihomo ≤1.19.11 任意文件读取）在 v1.19.30 已修复绝对路径穿越，但**工作目录内 / `SAFE_PATHS` 内的文件仍可被读取并可能进入日志**。Agent 必须使用 `PUT /configs` 的 `path` 模式，因此 `SAFE_PATHS` 是我们必须配置的东西。 |
| 待决策 | `SAFE_PATHS` 的最小集合（只包含 `/var/lib/proxy-agent/configs`？）；配置目录内不得存放敏感文件；日志中不得回显被读取内容。 |
| 关联 | ADR-004 D3、ADR-005 D7、R12 §1 |

## Q019 — 健康检查的 L4（代理端口）实现与误判边界

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 背景 | 实测：listener bind 失败**不致命**（仅 error 日志），`/version` 仍返回 200 → 只查 API 会把"代理端口没起来"误判为成功。 |
| 待决策 | L4 的探测方式（TCP connect 到 mixed-port）；在 TUN-only 或 `bind-address` 非 loopback 时如何判定；是否需要 parse 启动日志作为交叉验证。 |
| 关联 | ADR-003 D5、ADR-004 D4、R01 §1 |

## Q020 — `mihomo -t` 是否真的无副作用？

| 字段 | 内容 |
|---|---|
| 状态 | **`OPEN`（阻塞 L2 校验的安全前提）** |
| 背景 | Config Lifecycle 的 L2 语义校验依赖 `mihomo -t`。R02 的任务要求验证它是否仍会下载 geodata、写 `cache.db`、或在含 `tun:` 的配置上产生副作用。 |
| 影响 | 若 `-t` 有网络/磁盘副作用，则需在隔离目录中执行并显式说明。 |
| 关联 | REQ-CONFIG-009、ADR-004 D2、`docs/research/02-mihomo-config.md` |

## Q021 — 容器/无 systemd 环境下 Mihomo 进程管理的自研范围

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 背景 | 实测容器内 `/proc/1/comm=sh`（无 systemd），`ProcessManager` 必须有 `SupervisedChildProcess` 适配器。这意味着我们要自己实现退避重启、僵尸回收、日志捕获。 |
| 待决策 | 回退模式的触发条件（`/run/systemd/system` 不存在？）；回退模式下是否禁用 `systemctl` 相关 Use Case；重启策略与 systemd 的边界。 |
| 关联 | ADR-003 D4、R09 C9、R10 §1 C6 |

## Q022 — 多实例的领域建模是否现在就引入？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`（方向已定）** |
| 结论 | MVP 只保证**单实例可用**，但领域模型**从第一天**必须带 `MihomoInstanceId`（ShellCrash 硬编码禁止多实例是反例）。 |
| 待验证 | 配置目录布局、端口分配、socket 命名如何按实例参数化（避免后期破坏性迁移）。 |
| 关联 | R08 §1、产品范围 SHOULD |

## Q023 — 订阅必须"入库"是否可避免？

| 字段 | 内容 |
|---|---|
| 状态 | **`OPEN`** |
| 背景 | Sub-Store 的 `/download/:name` 要求订阅先存在（`POST /api/subs`），Agent 需写入其数据库。这带来副作用（写入外部服务状态）与 SSRF 面。 |
| 备选 | ① 要求用户在 Sub-Store 中自建订阅，Agent 只读（最保守）；② Agent 幂等写入（当前设计）；③ 若 `content=` 内联可行，则完全绕过入库（未验证）。 |
| 关联 | ADR-002 D1/D4、`04-sub-store.md` §10 O2/O5 |

## Q024 — `route_localnet` / `ip_forward` 等 sysctl 的写权限如何处理？

| 字段 | 内容 |
|---|---|
| 状态 | **`OPEN`** |
| 背景 | R11 实测：Docker 默认把 `/proc/sys` 挂成 ro（即使有 `SYS_ADMIN` 也写不动），但这是 **Docker 特有伪影**；PVE LXC 默认不这样挂载（未验证）。TProxy 的隐藏门槛正是 sysctl 可写。 |
| 处置 | doctor 必须把"sysctl 可写"单列为独立探测项，避免 TProxy 静默失效。 |
| 关联 | R11 C5、R10 §9 P10、REQ-NET-007 |

## Q025 — 探测脚本的"无副作用"如何被证明？

| 字段 | 内容 |
|---|---|
| 状态 | **`PARTIAL`** |
| 背景 | REQ-LXC-005 要求探测无副作用。R10 建议：写类探测默认关闭（`ProbeOptions{allow_write_probes:false}`）；TUN 探测优先用 `ioctl + close`（内核保证 close 即删设备与路由），而不是 `ip tuntap add/del`。 |
| 待验证 | 需要一组"副作用审计"测试（例如探测后对比 `ip rule`/`nft list`/`/proc/sys` 快照与探测前一致）。 |
| 关联 | REQ-LXC-005、ADR-005 D8、R10 §6.2 |

---

## 汇总：Phase 0 收口状态

| 状态 | 数量 | 条目 |
|---|---|---|
| `RESOLVED` | 11 | Q001(PARTIAL→已定实现方式)、Q002、Q003、Q005（方向）、Q006、Q007、Q008、Q010、Q013、Q014、**Q015**（2026-09-12：默认不经镜像） |
| `PARTIAL` | 9 | Q001、Q004、Q011、Q017、Q018、Q019、Q021、Q022、Q025 |
| `OPEN` | 6 | Q009、Q012、Q016、Q020、Q023、Q024 |

**结论**：Phase 0 的**架构阻塞项已全部解决**（Q002/Q003/Q006/Q007/Q008/Q010/Q013/Q014 均 `RESOLVED`）。剩余 `OPEN` 项均属于**实施细节或法律确认**，不阻塞 Domain Model 与 Application Ports 的设计，但必须在对应里程碑前收口：

```text
Q016       → 内嵌前（ADR-006 D1）；Q015 已收口（默认不经镜像，只走上游 release 直连）
Q020       → Config L2 校验实现前（ADR-004 D2）
Q023       → SubscriptionConverter Adapter 实现前（ADR-002 D4）
Q009/Q004  → Linux/PVE 里程碑前（需真机）
Q012/Q024  → 网络能力里程碑前
```
