# R14 — 部署模型选型

> 状态：已完成（由原调研证据重建） | 调研日期：2026-09-12 | 证据等级：`[实测]`（本机 + 容器）+ `[实测-文档残留]`（`/tmp/r14-deploy/` 证据目录）+ `[上游文档]` + `[上游源码]` + `[推测]`
> 关键结论一句话：**默认模型 = Model D —— `proxy-agent`（systemd 唯一 unit，托管 Mihomo 子进程）+ 内嵌 metacubexd 静态产物（同源反代，无进程）+ 可选外部 Converter（Sub-Store，默认不安装）；最小可用组合不依赖任何外部服务。**
>
> **说明**：本文是**重建**。原调研子任务在写文件前因上下文耗尽失败，仅留下 `/tmp/r14-deploy/` 下的实测证据。凡标注 `[实测-文档残留]` 的条目，均指本次从该证据目录中的原始日志/产物/抓取文件直接读出，而非重新测量。本次新增的少量补测标注为 `[实测-本次]`。
>
> **与 ADR-006 的关系**：`docs/adr/ADR-006-deployment-model.md`（Accepted）已先行定稿，本文是其 Research 层输入与证据展开。两者的模型选择、目录布局、端口、三条升级链路**完全一致**，本文不重复 ADR 的决策格式，只提供对比矩阵与证据。

---

## 1. 结论摘要（TL;DR）

| # | 结论 | 证据等级 |
|---|------|----------|
| 1 | **默认模型 = Model D**：Agent + Mihomo + 可选 Converter + metacubexd（静态产物）。Model B 不被选为默认，因为 Sub-Store 不在关键路径上，绑定它只会增加默认部署复杂度与 AGPL 义务面。 | 综合（见 §3/§4） |
| 2 | **metacubexd 不是一个进程，而是内嵌的静态文件集合**（Agent 的 `/ui/*` + 同源 `/clash-api` 反代）。**官方 All-in-One Server 容器被明确排除**——它自带 supervisor（`packages/agent/src/supervisor.ts` 直接 `spawn()` 内核），会与我们的 systemd/Agent 托管形成**双 supervisor 状态分裂**。 | `[实测-文档残留]` + `[上游源码]`（R07 §1/§4） |
| 3 | **Sub-Store 是可选外部组件，默认不安装、不由 Agent 托管**。未配置时订阅功能降级为 Native 直连解析；Sub-Store 故障时订阅更新失败但**当前激活配置保持不变**，且**绝不因此停止 Mihomo**。 | `[实测-文档残留]`（R05 §8） |
| 4 | **最小可用组合 = `proxy-agent` + Mihomo 两者即可**提供 HTTP/SOCKS/Mixed 代理、配置版本化与回滚、CLI/TUI/API、Doctor——不需要 Sub-Store，也不需要 Dashboard。 | 综合 + `[实测]`（Mihomo 独立跑通） |
| 5 | **资源占用（实测，非估算）**：Mihomo 空闲约 **11–12 MB**；Sub-Store（Node 直跑）空闲约 **93–94 MB RSS**。metacubexd 静态产物约 **8.1 MB / 158 文件**（未压缩），归档后 **3.0 MB**。 | `[实测-本次]` + `[实测-文档残留]` |
| 6 | **主分发形态是 deb 包**，不是容器镜像，也不是 `curl \| sh`。MVP **不提供官方 Docker 镜像**。 | 综合（§6/§8，ADR-006 D4/D6） |
| 7 | **三条升级链路必须独立**：Agent 自升级 / Mihomo 内核升级 / 配置回滚。内核升级必须"自研下载 + 校验 + 原子替换"，**禁止依赖 Mihomo 的 `/upgrade`**。 | `[实测-文档残留]`（R01 §1/§5）+ 设计文档 §39 |
| 8 | **Mihomo controller 与 Agent API 绝不默认 `0.0.0.0`**。Sub-Store 上游默认绑 `::`（等价全网卡），部署必须显式设 `127.0.0.1`。 | `[实测-文档残留]`（R12 §1、R05 §1） |

---

## 2. 组件与进程模型

### 2.1 进程/职责总表

| 组件 | 形态 | 谁拉起 | 运行身份 | 是否必须 | 失败域隔离 |
|---|---|---|---|---|---|
| `proxy-agent` | systemd unit（**唯一**） | systemd | root（capability 收敛，见 R09 §5/§6） | ✓ 必须 | Agent 挂 → Mihomo 子进程一并回收 |
| Mihomo | Agent 的 fork/exec **子进程** | `proxy-agent` | 同 unit 身份（ambient capabilities 传权） | ✓ 必须 | 崩溃由 Agent 退避重启；不影响 Agent 自身 API |
| metacubexd | **静态文件**（Agent `/ui/*` 提供，**无进程**） | — | — | ✗ 可选 | 前端加载失败不影响任何后端能力 |
| Sub-Store | **外部** Node/Docker 进程 | **用户自行** | 用户自定 | ✗ 可选 | 不可达 → 该次订阅更新失败，**旧配置保持激活** |

```text
                       ┌───────────────────────────────────────────┐
   systemd ──────────► │  proxy-agent.service  (唯一 unit)          │
                       │  ├── /api/v1/*   REST                     │
                       │  ├── /ws/v1/*    WebSocket                │
                       │  ├── /admin/*    自研 Web Admin            │
                       │  ├── /ui/*       metacubexd 静态产物(内嵌)   │
                       │  └── /clash-api  同源反代 ──┐              │
                       │                              │              │
                       │      fork/exec + 健康检查 ◄───┼───┐          │
                       └──────────┬───────────────────┼───┼──────────┘
                                  │                   │   │
                    ┌─────────────▼──────┐   ┌────────▼───┴──────┐
                    │ Mihomo 子进程       │   │ Mihomo controller │
                    │ HTTP/SOCKS/Mixed   │   │ unix sock 或      │
                    │ (可选) TUN/TProxy  │   │ 127.0.0.1:9090    │
                    └────────────────────┘   └───────────────────┘
                                                      ▲
                    ┌─────────────────────────────┐   │ 仅 Agent 可达
                    │ Sub-Store（可选，外部进程）   │   │ （浏览器永不直连）
                    │ 127.0.0.1:3001（须显式设置） │
                    └─────────────────────────────┘
```

### 2.2 为什么是"单 unit + 内核为子进程"

原调研在 R09 已定稿该结论（R09 C2/C3、`docs/research/09-linux-runtime.md` §3/§5），R14 沿用，不重复论证。要点：

- 若用 systemd 的**第二个 unit** 管 Mihomo，则 Agent 与 Mihomo 处于**不同 cgroup 与不同用户**，Agent 既无法 `kill(2)`（需要同 uid 或 `CAP_KILL`），也无法读写其 socket；只能回到 polkit / D-Bus 提权，复杂度与攻击面都显著上升。
- 单 unit 下 Agent 直接持有子进程句柄，`ExecStop` / 崩溃回收 / 退避重启 / 健康检查全部可在 Rust 内实现，且能施加 per-instance 生命周期串行锁（AGENTS.md「State and Concurrency」）。
- 代价：Agent 自身携带 `CapabilityBoundingSet=`，而 Web/API 攻击面在同一进程内。R09 已给出缓解设计（Agent 自身不主动用 `CAP_NET_ADMIN`，spawn 后 `capset()` 丢弃 effective 位）——该缓解为实现期 `[推测]`，见 §13。

### 2.3 无 systemd 环境的回退（PVE LXC 关键路径）

`[实测-文档残留]` R10 §4.2 的容器实测中，四种模式（默认 / `--privileged` / `--cap-add=NET_ADMIN` / `+ /dev/net/tun`）下 `/proc/1/comm` **全部为 `sh`**，且 **`systemctl` 不存在**。

因此 `ProcessManager` Port 必须有**两个适配器**：

```text
ProcessManager (Application 定义)
├── SystemdProcessManager     systemd 可用时：Type=notify、ExecReload=、
│                             KillMode=control-group、Restart= 限流、RuntimeDirectory=
└── DirectProcessManager      systemd 不可用时：Agent 自持子进程生命周期
                              （退避重启、进程组 kill、信号语义自实现、socket 自建）
```

选择由 `doctor` 的 `InitSystem` 探测结果决定（五值状态：`Supported`/`Unsupported`/`Unavailable`/`Misconfigured`/`Unknown`），而不是编译期分支或用户手填。

> **未验证**：DirectProcessManager 的完整行为（尤其"Agent 自身被 SIGKILL 后孤儿 Mihomo 的回收"）**本次未做实验**。§13 列为开放问题。

### 2.4 资源占用（实测数据与来源）

> **本节的数字全部来自实测，没有一个是推算的。** 无法实测的项一律标 `[未验证]`。

| 组件 | 指标 | 实测值 | 方法 / 证据来源 |
|---|---|---|---|
| **Mihomo** | 空闲内存 | **11 MB**（footprint，极简配置：单 `mixed-port` + `MATCH,DIRECT`） | `[实测-本次]` darwin arm64，`Mihomo Meta v1.19.13`（go1.25.0），macOS `footprint <pid>`；T+0/5/10/15s 四次采样均稳定 11 MB |
| **Mihomo** | 空闲内存（较真实配置） | **12 MB**（footprint；3 个 proxy、2 个 group、4 条规则、3 个 listener、fake-ip DNS） | `[实测-本次]` 同上；加载完成后监听 `mixed 17901 / socks 17902 / http 17903 / controller 127.0.0.1:19098`，`/proxies` 返回 11 个条目 |
| **Mihomo** | 二进制体积（linux amd64） | **32,043,156 B ≈ 30.6 MiB**，statically linked, stripped | `[实测-文档残留]` `/tmp/r14-deploy/art/mihomo-linux-amd64`，`file` 输出 `ELF 64-bit LSB executable, x86-64, statically linked, stripped` |
| **Mihomo**（deb 内同版本） | 二进制体积 | 同上（deb `data.tar.gz` 内 `usr/bin/mihomo`） | `[实测-文档残留]` 校验 `file` 输出一致 |
| **Sub-Store**（Node 直跑） | 空闲 RSS | **93.0 → 93.5 MB**（`rss_mb`），`heapUsed` 17.6–18.3 MB | `[实测-文档残留]` `ss_mem.log`：T+3s `rss_mb:93`，T+30s `93.2`，T+45s `93.2` |
| **Sub-Store** | 加载一次订阅后 RSS | **94.9 MB**（+1.9 MB） | `[实测-文档残留]` `ss_mem2.log`：`idle+10s rss 93.5` → `after-load rss 94.9` |
| **Sub-Store** | 官方 bundle 体积 | **2.39.6，3.0 MiB 自包含，`runtime-manifest.json` 声明 `"npm": []`** | `[实测-文档残留]`（R05 §1/§3.2 同源；证据目录内 `sub-store.json`、`package.json` 版本亦为 `2.39.6`，`license: AGPL-3.0`） |
| **Sub-Store** | 本次证据目录内的安装规模 | `node_modules` 120 个顶层项 / **46 MB**（源码途经，非 bundle 路径） | `[实测-文档残留]` `/tmp/r14-deploy/node_modules`；注意这与 R05 记录的"pnpm 全量含 devDeps 139 MB"不是同一路径，不可混用 |
| **metacubexd** | 静态产物（解压） | **8.1 MB / 158 个文件**（`_nuxt/` 112 项） | `[实测-文档残留]` `/tmp/r14-deploy/art/mcxd` + `du -sh` |
| **metacubexd** | 静态产物（归档） | **3.0 MB**（`compressed-dist.tgz`） | `[实测-文档残留]` `/tmp/r14-deploy/art/compressed-dist.tgz`（2,539,072 B） |
| **metacubexd** | 版本 | `v1.273.1` | `[实测-文档残留]` `/tmp/r14-deploy/mcxd.html` = GitHub Release 页面，title 为 `Release v1.273.1 · MetaCubeX/metacubexd` |
| **Sub-Store 内存** | Docker 路径 | **`[未验证]`** | R05 §3.1：Docker Hub 不可达，`docker pull xream/sub-store` 失败；R05 给出 `[推测]` 60–120 MB，**本文不采用该推测值作为结论** |

**关于 Mihomo 内存数据的适用边界（重要）**：

> 上表 Mihomo 的 11/12 MB 是 **darwin arm64 数据**，配置为**不含 TUN、不含大型规则集、不含 geoip/geosite 数据库**的最小配置。Linux 与 darwin 的 Go runtime 在 RSS 行为上不完全可比；启用 TUN、加载 `geoip.metadb` / `geosite.dat`（数百 MB 级规则库）或大量 proxy providers 后，常驻内存会显著高于 12 MB。**因此本文只用它确立一个量级结论："Mihomo 的基线常驻开销是个位数到十位数 MB 级别，远小于 Sub-Store（Node，约 93 MB）；在 LXC 内存预算中 Mihomo 不是瓶颈。"** 精确的 Linux + TUN + geodata 场景内存**本次未测量**，见 §13。

**一个顺带得到的实测事实（对 doctor 有价值）**：`[实测-本次]` 在无外网环境下启动含 `GEOIP,CN,DIRECT` 规则的配置时，Mihomo 会尝试下载 `geoip.metadb`，失败后**直接 fatal 退出**：

```text
level=error msg="can't initial GeoIP: can't download MMDB: Get \"https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb\": net/http: TLS handshake timeout"
level=fatal msg="Parse config error: rules[1] [GEOIP,CN,DIRECT] error: can't download MMDB: ..."
```

→ **离线部署（PVE LXC 无外网）必须预置 geodata 文件**，否则用户订阅里含 `GEOIP` 规则的配置会导致内核启动失败。这是部署文档与 `doctor` 必须覆盖的项（§11、§13）。

---

## 3. 四模型对比矩阵

> 模型定义取自 `docs/phase-0-architecture-discovery.md` R14 章节（第 682–732 行）。

在进入矩阵前，先把四个模型映射到"**什么是必须的、什么是可选的**"：

```text
Model A : Agent + Mihomo + metacubexd(静态)
Model B : A + Sub-Store（作为 Agent 捆绑/托管的组件）
Model C : Agent + Mihomo + 自研 Native Converter + metacubexd
Model D : Agent + Mihomo + Optional Converter + metacubexd   ← 选定
```

> **术语澄清（避免与 R05/R06 冲突）**：Model C 的 "Native Converter" 指**自研**一个订阅转换器；而 R05 §8.2 中出现的 "NativeConverter（降级态）" 指**不做转换**、仅按原生格式直连解析并写入配置。本文严格区分：**§3 的 "Native Converter" = 自研转换器（Model C）**；**§5 的 "Native 解析降级" = 无转换器可用时的直连解析（Model D 的降级态）**。

### 3.1 对比矩阵

| 维度 | **Model A**<br>Agent+Mihomo+metacubexd | **Model B**<br>A + Sub-Store | **Model C**<br>Agent+Mihomo+**自研** Converter+metacubexd | **Model D（选定）**<br>A + **可选** Converter |
|---|---|---|---|---|
| **部署复杂度** | **低**。单 deb、单 unit、零外部服务。 | **高**。默认还要装 Node ≥22（推荐 24）或 Docker；Sub-Store 数据目录必须先 `mkdir -p` 否则启动即崩（`[实测]` R05 §3.3 坑 1）；须额外配置 `SUB_STORE_BACKEND_API_HOST` 否则默认绑 `::`。 | **中高**（装机简单，但**研发成本高**：自研 Converter 是独立产品级工程）。 | **低**（默认路径与 A 相同；Sub-Store 属于"用户可选叠加"）。 |
| **离线可用性** | **完全离线**。Mihomo 自带；metacubexd 是内嵌静态文件，无 CDN。 | **降级**：Sub-Store 缺席时须能直连订阅；且 Sub-Store 基础启动离线可行，但 geodata/IP 库相关能力受限。 | **完全离线**（自研转换器无外部依赖）—— 这是 C 唯一的真实优势。 | **完全离线**（默认路径与 A 同）；配置了 Sub-Store 而它不可达时降级为 Native 解析。 |
| **资源占用** | **最低**：Mihomo ≈11–12 MB `[实测-本次]`；metacubexd 无进程（0 MB 常驻，8.1 MB 磁盘）。 | A + **≈93–95 MB RSS**（Node 直跑）`[实测-文档残留]`。 | ≈ A + 自研 Converter 的常驻（Rust，预期很小）`[推测]`。 | ≈ A（默认）；用户启用 Sub-Store 则自行承担 ≈93 MB。 |
| **故障域** | **2 个**（Agent、Mihomo），且 Mihomo 是 Agent 子进程 → 可统一回收。 | **3 个**，且多出一个**跨进程、无认证、数据敏感**的外部服务故障域。 | 2 个。 | 默认 2 个；启用 Sub-Store 后 3 个，但**该故障域不在关键路径**（订阅更新失败 ≠ 代理中断）。 |
| **外部服务依赖** | **0** | **1（强）**：Sub-Store 成为默认安装的一部分。 | **0** | **0（默认）/ 1（用户可选）** |
| **许可证影响** | 干净：Agent 自研（建议 MIT OR Apache-2.0）；metacubexd 静态产物需处理 **Highcharts 专有许可** + UFL/CC-BY 署名（R13 §1.4）。 | A + **AGPL-3.0 义务面扩大**。独立进程 + 仅 HTTP + 不改源码时不触发 §13；但**若由我们捆绑/托管/分发**，义务与维护负担显著上升。 | 干净（无 AGPL），但**研发投入换来的只是"离线转换"**。 | 同 A。Sub-Store 保持**用户自装** → AGPL 边界最清晰（R13 §1.2）。 |
| **升级路径** | 清晰：Agent deb 升级 + 内核自研升级 + 配置回滚，三条独立。 | 多一条**Sub-Store 升级链路**，且上游发布节奏极快（改 `backend/package.json` 即自动打 tag），跟进成本高（R05 §5）。 | 多一条**自研 Converter 的语义跟随链路**——必须持续追随上游转换语义，否则用户订阅解析回归。 | 清晰，同 A。Sub-Store 升级**由用户负责**，Agent 只做版本检测与提示（R05 §5.2）。 |
| **LXC 适配难度** | **最低**。场景 A（HTTP/SOCKS）零特权即可，不需要任何额外 `lxc.*` 行（R10 §7.1）。 | **中**：需要 Node 运行时或 Docker-on-LXC；后者在 PVE LXC 下代价与不确定性高。 | 低（同 A）。 | **最低**（同 A）。 |
| **何时是正确的** | 用户要"能用的代理 + 配置版本化 + 回滚 + Doctor"，**且订阅能直连解析**时。 | 用户**明确依赖 Sub-Store 的高级转换**（operators/scripts/mergeSources/完整配置产出），且**愿意自己运维一个 Node 服务**时——即"用户自己已经部署了 Sub-Store"的场景。 | **仅当**"离线 + 高级转换 + 不许引入 AGPL 组件"三者**同时**成立，且团队愿意长期维护一个转换器时。**当前本项目不具备该前提**（见下）。 | **本项目 MVP 的正确模型**：默认不含任何外部服务（= A 的体验），同时**保留**接入外部高级 Converter 的能力（⊇ B 的能力），且不强制任何人承担它的成本。 |

### 3.2 为什么 Model C 在本项目不成立

`[实测-文档残留]` + R06 已给出一致结论：

1. **自研转换器是范围爆炸**。产品范围文档列为 WON'T（ADR-006 §3 亦据此拒绝 C）。完整配置产出（`proxy-groups`/`rules`/完整骨架）而非仅节点列表，是 Sub-Store `produce` 语义的核心，非小工程。
2. **R06 已实测排除现成的捷径**：`sub-store-convert` 判定 **Rejected**——能力是 Sub-Store 严格子集（无 operator/script/process、无 `mergeSources`）、产出**不含完整配置**、API 与 Sub-Store 不兼容、且**失败语义严重违反本项目核心不变量**（远端不可达时 `convert()` 仍 resolve 成功并输出空 `proxies:`，调用方无法区分"订阅为空"与"拉取失败"）。
3. **许可阻塞**：其 npm 产物内联了 27 个 Sub-Store（AGPL-3.0）源文件却只标称 MIT、无任何许可声明 → **不得 Bundled、不得 Source Reuse**。
4. **且它的唯一优势（离线、体积小）被官方 bundle 抵消**：官方 Release `sub-store.bundle.js` 3.0 MiB、零 npm 依赖、冷启动 130 ms（R05 §1.2）。

→ **Model C 的"自研"与"借用现成库"两条路都被关闭**。C 只在未来"离线 + 高级转换"成为硬需求时才需要重新评估。

### 3.3 为什么 Model B 不选为默认

Model B 与 Model D 的差别**不是能力差别，而是"谁支付成本"的差别**：

- D 保留了 B 的**全部能力上界**（用户随时可以部署 Sub-Store 并让 Agent 接入）；
- D 不把 Sub-Store 放进**默认路径**，因此默认安装不承担：Node/Docker 运行时、数据目录预创建契约、`::` 默认绑定带来的认证/反代需求、AGPL 义务面扩大、以及跟随上游高频发布的维护负担。

**唯一的反面论点**（诚实记录）：如果目标用户画像**普遍依赖 Sub-Store 高级转换**，那么 D 会让"开箱即用"打折——用户装完 deb 后发现订阅解析能力不如预期。缓解方式：`doctor` 明确输出 `SubStore = NotConfigured`（合法降级，非错误）+ 部署文档给出 Sub-Store 的一键指引（R05 §8.1 已给 Docker 与 Node 两条），并在 Web Admin 的订阅页给出"检测到未配置 Sub-Store，部分订阅格式可能无法解析"的提示。

---

## 4. 默认模型与理由

### 4.1 决策

**默认模型 = Model D。**

```text
默认安装（deb）后的运行态：

  proxy-agent.service   ← 唯一 unit
    ├── Mihomo 子进程    ← 必须
    ├── /ui/* metacubexd ← 内嵌静态产物，无进程
    └── SubscriptionConverter
          ├── NativeConverter        ← 默认生效（未配置 Sub-Store）
          └── SubStoreConverter      ← 用户配置 base_url 后生效，指向外部进程
```

### 4.2 理由（逐条对应 ADR-006 D1）

| # | 理由 | 支撑 |
|---|---|---|
| 1 | **最小可用组合不依赖外部服务**：只装 Agent + Mihomo 即可提供 HTTP/SOCKS/Mixed 代理、配置版本化与回滚、API/CLI/TUI、Doctor。 | §5；R05 §8.1；R10 §7.1 |
| 2 | **避免双 supervisor**：metacubexd 只以静态资源形态内嵌（方法 A），其 `packages/agent` 的 supervisor / profile 生命周期 / 内核热切换**全部不启用**；不使用 `apps/server` 的 All-in-One 形态。 | §2.1；R07 §1.4/§6 方案 C；`[上游源码]` `packages/agent/src/supervisor.ts`、`apps/server/Dockerfile` |
| 3 | **许可证义务最小化**：Sub-Store 保持独立进程（AGPL §13 不触发）且默认不进入分发物；`sub-store-convert` 完全不用（R06 Rejected）；Mihomo 以**独立二进制**分发，**绝不静态链接进 Agent**（否则 GPL-3.0 传染整个二进制）。 | R13 §1.1/§1.2/§1.3/§7 |
| 4 | **降级路径清晰且默认就在降级态**：无 Sub-Store → Native 解析；无 Dashboard → 不影响后端；Sub-Store 故障 → 订阅更新失败但**当前激活配置不变**，**绝不停止 Mihomo**。 | §5；R05 §8.2；设计文档 §44 |
| 5 | **LXC 适配难度最低**：场景 A 在 unprivileged LXC 中零额外配置即可完整工作，是最稳默认档。 | R10 §7.1/§7.4 |

### 4.3 被明确排除的方案

| 方案 | 排除理由 |
|---|---|
| 默认捆绑 Sub-Store（Model B 作为默认） | 默认复杂度 + AGPL 义务面 + 高频上游跟版负担；且失败已可降级 |
| MVP 自研 Native Converter（Model C） | 范围爆炸（产品范围 WON'T）；捷径 `sub-store-convert` 已被 R06 Rejected 且有许可阻塞 |
| metacubexd 官方 All-in-One Server 容器 | **双 supervisor 冲突**（架构冲突，非集成选项） |
| metacubexd `external-ui` 方式作为产品默认 | UI 与 controller 同源同端口、controller 须对浏览器可达、无法叠加我们的 Web 登录态 → 仅作为"用户自行部署"的可选项 |
| 容器镜像作为主分发 | PVE LXC 下 Docker-in-LXC 代价与不确定性高；且与 systemd 托管形成双 supervisor |
| 把 Mihomo 静态链接进 Agent 二进制 | GPL-3.0 传染 → 整个二进制须按 GPL-3.0 分发 |

---

## 5. 最小可用组合与降级路径

### 5.1 最小可用组合（MUST 可用）

```text
proxy-agent + mihomo
  → HTTP / SOCKS / Mixed 代理（默认 7890 系列高位端口，无需任何 capability）
  → Mihomo 生命周期（start/stop/restart/reload/健康检查）
  → 配置版本化：write temp → fsync → 原子 rename → activate → reload → health check
  → 配置回滚
  → REST API / CLI / TUI
  → doctor（能力探测 + 降级状态）
```

`[实测-本次]` 佐证：Mihomo 单独运行（无 Agent、无 Sub-Store、无 Dashboard）即可正常监听 `mixed/socks/http` 三个入站并响应 `/proxies` —— 代理数据面本身不依赖任何其他组件。

### 5.2 分档能力矩阵

| 安装档 | 组成 | 新增能力 | 失败影响 |
|---|---|---|---|
| **最小** | Agent + Mihomo | 代理 + 生命周期 + 配置版本/回滚 + API/CLI/TUI + Doctor | — |
| **+ Dashboard** | 最小 + 内嵌 metacubexd | 可视化面板（代理组、连接、日志、规则） | 前端 404/JS 错误 → 仅面板不可用 |
| **+ 订阅转换（外部）** | + 用户自装 Sub-Store | 高级订阅转换（operators/scripts/mergeSources/完整配置产出） | Sub-Store 不可达 → 该次订阅更新失败，**配置不变** |
| **+ 高级网络** | + TUN / TProxy（需 LXC 侧配置） | 透明代理 | 能力缺失 → 状态为 `Unavailable`，**不得导致其他功能失败** |

### 5.3 降级判定流程（与 R05 §8.2 完全一致）

```text
启动 / 配置变更时：
  Sub-Store endpoint 已配置?
      ├─ NO  → Converter = Native 解析（降级态，非错误）
      │        订阅功能 = 仅原生解析直连订阅（不做高级处理/脚本）
      │        doctor 输出：SubStore = Unavailable/NotConfigured
      └─ YES → Health Check: GET <base_url>/api/utils/env
                 ├─ 2xx 且 data.version 可解析
                 │     → Converter = SubStoreConverter，记录 version（仅提示，不自动升级）
                 └─ 失败 / 超时 / 版本不可解析
                       → SubStore 标记 Misconfigured/Unavailable
                         若曾成功过：继续用缓存的转换结果或 Native
                         绝不因 Sub-Store 不可用而 stop/disable Mihomo
```

**订阅更新失败的完整不变式**（AGENTS.md「Failure behavior」）：

```text
当前激活配置 ──► 订阅更新 ──► 转换 ──► 校验 ──► 新 ConfigVersion ──► 激活 ──► reload ──► 健康检查
                                                                                    │
                              任一步失败 ────────────────────────────────────────────►│
                                                                                    ▼
                                                            旧配置保持激活（必要时回滚）
```

**没有 Sub-Store 时订阅功能的具体表现**（这是"降级路径"必须回答的问题）：

| 订阅形态 | 无 Converter（Native 解析）下的行为 | 证据 |
|---|---|---|
| Clash / Mihomo YAML 订阅（已是完整配置或 proxies 列表） | 可直连下载并解析 | `[推测]` |
| base64 编码的节点 URI 列表 | 可解析为节点，但**不生成完整配置骨架**（无 `proxy-groups`/`rules`） | `[实测-文档残留]` R06 §1.3 指出"产出完整配置"是 Sub-Store `produce` 语义 |
| 需要 operators / script filter / `mergeSources` 的订阅 | **不可用**，须先部署 Sub-Store | `[实测-文档残留]` R06 §1.2 |
| 订阅 URL 需要特殊 UA / 认证头 | Agent 侧直连，按订阅配置发送 | `[推测]` |

> **明确的不确定性**：Native 解析的确切能力边界（支持哪些格式、失败时如何报告"格式不支持"而非"订阅为空"）**尚未定义**——R06 的教训正是"把拉取失败误报为空订阅"会直接破坏核心不变量。该边界必须在实现前用 ADR 或设计文档固定。见 §13。

---

## 6. deb 分发与安装（含目录布局）

### 6.1 目录布局

```text
/usr/bin/proxy-agent                         Agent 二进制（含内嵌 admin UI + metacubexd 静态产物）
/usr/bin/proxyctl                            CLI 客户端
/usr/lib/proxy-agent/mihomo                  Mihomo 二进制（初始版本随包，或由 Agent 下载；见 6.4）
/usr/lib/systemd/system/proxy-agent.service  唯一 unit（非 conffile，升级覆盖）
/usr/lib/systemd/system/proxy-agent.service.d/10-proxy-only.conf   默认档（无 TUN/无 nftables）
/usr/lib/sysusers.d/proxy-agent.conf         声明 proxy-agent 用户 + proxyctl 组
/usr/lib/tmpfiles.d/proxy-agent.conf         可选的目录兜底（推荐做法仍是 RuntimeDirectory=）
/etc/proxy-agent/config.toml                 conffile（dpkg 保护）
/etc/proxy-agent/secrets.toml                0600，**不是** conffile（由用户在 UI 生成）
/var/lib/proxy-agent/                        StateDirectory 0750（持久）
    ├── configs/                             不可变配置版本 v001.yaml, v002.yaml, ...
    ├── subscriptions/
    ├── cache/
    ├── state/                               active 指针
    └── database.sqlite                      元数据（订阅、任务、审计）
/run/proxy-agent/                            RuntimeDirectory 0750（易失，unit 停止即清）
    ├── agent.sock                           本地 API（0660 root:proxyctl 语义）
    └── mihomo.sock                          Mihomo controller
/usr/share/doc/proxy-agent/                  copyright(DEP-5)、THIRD-PARTY-NOTICES、licenses/
```

> **`active` 指针**：`/var/lib/proxy-agent/state/active` 指向 `configs/vNNN.yaml`。配置**永不原地编辑**，一律 `write temp → flush/fsync → 原子 rename → activate`。

### 6.2 与 R09 结论的一致性核对

| R09 结论 | 本文是否一致 | 说明 |
|---|---|---|
| C10：`/etc/proxy-agent/config.toml` 走 dpkg **conffile** | ✓ 一致 | §6.1 保留；§6.3 的 postinst 不覆盖 |
| unit 文件放 `/usr/lib/systemd/system/`（**不**放 `/etc/systemd/system/`） | ✓ 一致 | 该目录是包管理目录，升级被 dpkg 覆盖正是我们想要的；用户自定义走 `/etc/systemd/system/<unit>.d/*.conf` drop-in |
| postinst **不 enable、不 start、不覆盖用户配置** | ✓ 一致 | §6.3 |
| 用 `sysusers.d` 而**不是** postinst 命令创建用户 | ✓ 一致 | 声明式、幂等、可被 `systemd-sysusers` 重放；Debian 的 systemd 包已在 postinst 调用它，我们只需 drop 文件 |
| **静态用户**（非 `DynamicUser=`） | ✓ 一致 | R09 已用官方依据排除 `DynamicUser=`（隐含 `ProtectSystem=strict`、与 D-Bus 策略不兼容、动态 UID 会被回收 → 与"长期保存配置版本与 SQLite"天然冲突） |
| `StateDirectory=` / `RuntimeDirectory=` / `ConfigurationDirectory=` 而非手工 mkdir | ✓ 一致 | `[上游文档]` systemd.exec：这些指令启动时按表创建目录、设为 `User=`/`Group=` 属主、隐含 `BindPaths=`，并把目录从 `ProtectSystem=strict` 的只读效果中排除 |
| `/var/lib/proxy-agent/**` 升级不得触碰 | ✓ 一致 | dpkg 不管该目录 |
| `config.toml` 不放 secret（secret 走 `secrets.toml` 0600，非 conffile） | ✓ 一致 | §6.1 |

`[实测-文档残留]` **证据目录中的 `systemd.exec.html`**（383,644 B，`systemd 261.2` man 页抓取）正是原调研者为核实上述 `*Directory=` 语义而抓取的原文——本文 §6.2 关于 `*Directory=` 的表述可直接回溯到该文件。

### 6.3 postinst 职责边界

**应当做**（全部幂等）：

```bash
#!/bin/sh
set -e
# 1. 应用 sysusers.d 声明（幂等）
systemd-sysusers proxy-agent.conf || true

# 2. 创建持久目录（幂等；绝不 chown -R 整个 /var/lib）
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/configs
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/subscriptions
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/cache
install -d -o proxy-agent -g proxy-agent -m 0750 /var/lib/proxy-agent/state
install -d -o proxy-agent -g proxy-agent -m 0750 /etc/proxy-agent

# 3. 只放默认配置模板，绝不覆盖已存在的用户配置
if [ ! -e /etc/proxy-agent/config.toml ]; then
    install -o proxy-agent -g proxy-agent -m 0640 \
        /usr/share/proxy-agent/config.toml.default /etc/proxy-agent/config.toml
fi

# 4. 刷新 systemd —— 不 enable、不 start
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
```

**绝不应该做**：

| 反模式 | 为什么 |
|---|---|
| `systemctl enable` / `systemctl start` | 静默改变系统启动行为；首次启动应由 `doctor` 前置 |
| 覆盖 `/etc/proxy-agent/config.toml` | 破坏用户配置，绕过 dpkg 三方合并 |
| `--force-confnew` 之类 dpkg 选项 | 同上 |
| **在 postinst 里改用户网络配置**（写 nftables/iptables 规则、改路由、启 TUN、写 sysctl） | **安全红线**。网络变更必须是用户在 UI/CLI 中显式触发、且可回滚的 Application Use Case |
| 在 postinst 里下载 Mihomo 二进制 | 安装脚本不应联网拉取不受信任产物；应由 `proxy-agent mihomo update` 显式执行并校验 |
| `chown -R proxy-agent /var/lib/proxy-agent` | 可能改变用户手动放入文件的属主；只 chown 我们创建的目录 |

### 6.4 是否静态链接（musl）与 glibc 兼容风险

**现状事实**：

- `[实测-文档残留]` 官方 mihomo deb 内的 `usr/bin/mihomo` 是 **`statically linked, stripped`** —— 但这是 **Go 二进制的静态链接**（Go runtime 默认静态，与 musl/glibc 之争无关），**不能**据此推断 Rust 侧的链接策略。
- `[实测-文档残留]` 证据目录含 `art/fd-v10.2.0-x86_64-unknown-linux-{gnu,musl}.tar.gz`、`rg-musl.tar.gz` 等**双变体下载对照**，以及 `xlink/target/x86_64-unknown-linux-musl/` 的 cargo 构建残留（`r14hello` 构建指纹）—— 说明**原调研者确实在评估 musl 交叉构建路径，但未留下可判定其结论的产物或日志**（`xlink/target/.../release/` 下无最终二进制，仅 `.cargo-lock` 等空壳）。

**因此本文的判断**：

| 选项 | 优点 | 风险 | 本文倾向 |
|---|---|---|---|
| `x86_64/aarch64-unknown-linux-**gnu**` | 与目标发行版（Debian 12/13、Ubuntu 22.04+）的工具链一致；Rust 生态兼容性最好（`sqlx` + TLS + DNS 解析等） | 需在**最老支持发行版**上构建以保证 glibc 符号版本兼容；若在过新的机器上构建，目标机可能报 `GLIBC_2.xx not found` | **MVP 采用 gnu**，并在容器化的老基线（如 Debian 12）中构建 |
| `..-unknown-linux-**musl**` | 单文件、无 glibc 版本约束，可在任意发行版运行 | 需要 dns 解析/TLS 的 musl 适配（如 `rustls` + ring/aws-lc）；某些 crate 在 musl 下有坑；体积与调试体验略差 | **备选**，需实测后再决定 |

> **本条在设计文档/ADR 中的状态**：ADR-006 D4 已记录"是否改用 musl 静态链接**待评估**，列入 open-questions"。本文**不擅自定论**——因为原调研者留下的证据不足以支撑任一方。**明确写为 `[未验证]`。**

### 6.5 `install.sh` 职责边界

`install.sh` **不是主分发形态**（deb 才是），仅作为无包管理/不便用 dpkg 环境的次选。

**允许**：

```text
1. 检测架构（uname -m → x86_64 / aarch64）与发行版
2. 从发布源下载对应 deb / tarball，并**校验校验和与（若有）签名**
3. 调用 dpkg -i（若为 deb）或释放到 /usr/bin、/usr/lib
4. 创建用户/组、目录（与 postinst 同语义，幂等）
5. 运行 proxy-agent doctor 并把结果打印给用户
6. **在 doctor 通过且用户明确确认后**，执行 systemctl enable --now
7. 打印后续步骤（如何加 Sub-Store、如何加入 proxyctl 组）
```

**禁止**：

```text
✗ 静默修改用户网络配置（nftables/iptables/路由/sysctl/TUN）
✗ 静默 enable/start 而不告知
✗ 覆盖已存在的 /etc/proxy-agent/config.toml
✗ 未经用户确认就联网拉取额外的第三方组件（含 Sub-Store）
✗ curl | sh 中把远端脚本直接喂给 shell 而不做完整性校验
```

> 与 R09 §9.7 一致："包本身不 enable；`install.sh` 在完成 `doctor` 并取得用户确认后可以 `systemctl enable --now`，这样『deb 用户』与『一键脚本用户』的行为差异是显式的。"

### 6.6 构建矩阵与产物命名

```text
目标平台（MVP）：x86_64 与 aarch64
   x86_64-unknown-linux-gnu
   aarch64-unknown-linux-gnu
（musl 变体：待评估，见 6.4）

deb Architecture 字段：
   x86_64  → amd64
   aarch64 → arm64

产物命名建议（含版本 + 架构 + 校验和）：
   proxy-agent_<version>_amd64.deb
   proxy-agent_<version>_arm64.deb
   proxy-agent_<version>_amd64.deb.sha256
   proxy-agent_<version>_arm64.deb.sha256
   proxyctl_<version>_<arch>            （若 CLI 独立分发）

Mihomo 内核产物命名（**注意命名限制**）：
   遵循上游 release 资产命名（如 mihomo-linux-amd64），
   但我们**自己的**包名/产品名不得包含 "mihomo"（R13 §1.6）。
```

**命名限制（必须遵守）**：`[上游文档]` Mihomo README 明确 "any downstream projects not affiliated with `MetaCubeX` shall not contain the word `mihomo` in their names"。故产品名用 `proxy-agent` / `proxyctl`；文档中描述性引用 "Mihomo" 属 nominative use，可以保留。

---

## 7. 升级与回滚的三条独立链路

`[上游文档]` 设计文档 §39：

> ```text
> Mihomo Update = kernel update
> Config Update = proxy/config update
>
> UI 不应该混成一个 "Update"，而应该是：
>   Kernel       └── Check Update
>   Subscription └── Update Now
> ```

### 7.1 三条链路

```text
① Agent 自身升级
   形态: deb 升级（apt install --only-upgrade proxy-agent）或二进制替换
   影响: 替换 /usr/bin/proxy-agent；systemd 重启 unit
   不变式: 不影响 /var/lib/proxy-agent 下的配置版本；重载后仍激活原 ConfigVersion
   失败处理: dpkg 保留旧包可回滚；配置数据不在包内，天然不受损

② Mihomo 内核升级
   形态: 下载 → 校验(sha256，若有签名则验签) → 原子替换 → 重启 → 健康检查 → 失败回滚
   关键: **必须自研**，禁止依赖 POST /upgrade
   隔离: 内核版本与配置版本**正交** —— 换内核不改配置，换配置不换内核

③ 配置回滚
   形态: 选择历史 ConfigVersion → activate → reload → 健康检查 → 失败回滚
   隔离: 不触碰内核二进制
```

### 7.2 为什么内核升级必须自研（禁止 `/upgrade`）

`[实测-文档残留]` R01 §1/§5 与 ADR-006 C10 已定稿，证据要点：

| 事实 | 说明 |
|---|---|
| `POST /upgrade?force=false` 实测返回 `500 {"message":"update error: already using latest version v1.19.30"}`，binary 哈希未变 | 把"已是最新"当作错误返回，语义不可用 |
| 依赖 GitHub Releases API 可达 | 受限网络下不可用（本次调研环境即不可达） |
| `force=true` **直接替换运行中的二进制**，无签名校验、无 A/B 回滚 | 存在把自身替换成半截二进制的风险 |
| `POST /upgrade/ui` 实测 30s 超时无响应 | 无超时保护 |
| `POST /configs/geo` 返回 `204` 但实际是 fire-and-forget，无失败回传 | 调用方无法从此判断成功 |

→ 若把 `/upgrade` 作为内核升级通道，等于把项目**最核心 Use Case 之一**（内核更新）的失败语义交给内核自己，且无法保证"失败时保留上一个可用版本"。因此 Agent 自研：**下载 → 校验 → 原子替换 → 重启 → 健康检查 → 失败回滚**。

### 7.3 三条链路为什么必须互不耦合（而非"最好解耦"）

1. **失败域**：内核升级失败不应影响配置可用性（旧内核 + 旧配置仍是可用的）；配置回滚不应触发内核变更。
2. **回滚语义**：内核是**单版本槽位**（磁盘上只有一个 `/usr/lib/proxy-agent/mihomo`），必须靠"保留上一份 + 原子 rename"实现回滚；配置是**多版本不可变仓库**（`configs/vNNN.yaml`），回滚是"改 active 指针"。二者机制不同，混在一起会互相破坏。
3. **UI 与 API 的契约**：`GET /api/v1/mihomo` 与 `GET /api/v1/configs` 是两组独立资源，`POST /api/v1/mihomo/*` 与 `POST /api/v1/configs/*/rollback` 是两组独立用例。禁止出现 `POST /api/v1/update` 这类混合端点。
4. **GPL 边界**：内核升级是"替换一个独立 GPL-3.0 二进制"，与 Agent 自身升级（我们自己的包）在**分发与许可义务上完全不同**，混在一起会让合规边界模糊。

---

## 8. 容器/镜像策略

### 8.1 决策：MVP **不提供官方 Docker/Podman 镜像**

只提供 **deb 包 + `install.sh`（次选）**。

### 8.2 理由

**(a) PVE LXC 下 Docker-in-LXC 的代价高且不确定（`[未验证]`）**

- PVE LXC 容器内跑 Docker 需要额外特权与配置（通常需启用 `nesting`、可能需 cgroup 透传、AppArmor/seccomp 调整），而 `nesting=1` 本身会把宿主 procfs/sysfs 更多暴露给 guest（R10 §7.2 坑 6）。
- R10 已明确：`nesting` 与容器内 systemd 可用性强相关；但**未开 nesting 时容器内 init 不是 systemd**，`systemctl` 不可用（`[实测-文档残留]` R10 §4.2：四种模式下 `/proc/1/comm` 均为 `sh`，`systemctl` 均**不存在**）。
- **`[未验证]`：本次调研没有在真实 PVE 上验证 Docker-in-LXC 的可用性、性能与资源开销。** R10 §9 已把这类验证列入"真实 PVE 补测清单"，至今未完成。**因此"代价高"这一判断目前是 `[推测]`，而非实测结论。**

**(b) 容器形态会与 systemd 托管冲突（`[上游源码]` + `[实测-文档残留]`）**

若用户用 metacubexd 官方 All-in-One 镜像，容器内它会 `spawn()` 并托管自己的 mihomo；而我们的 Agent 也要托管 mihomo → **双 supervisor**。这不是"配置不同的选项"，而是架构冲突。

**(c) 但必须支持"用户自行容器化"**

- Agent 必须能**检测"无 systemd"并回退 direct-process 模式**（见 §2.3）。这是硬性要求，因为目标环境 PVE LXC 本身就常常没有 systemd。
- 换句话说：**我们不发布镜像，但我们不阻止也不破坏容器化运行。**

### 8.3 明确排除

```text
✗ ghcr.io/metacubex/metacubexd-server   （All-in-One Server：双 supervisor）
✓ 静态产物由 Agent 的 /ui/* 提供        （方案 A，见 R07 §6）
✗ 把 Mihomo 打进我们的容器镜像再分发     （GPL-3.0 义务面 + 双 supervisor 风险）
```

### 8.4 将来重新评估的条件

若出现以下证据，应重新评估是否提供官方镜像：① 真实 PVE 上 Docker-in-LXC 的实测开销数据；② 用户群体对 LXC 之外的部署形态（纯 VM / 裸机 + 用户自管容器编排）需求上升；③ AGENTS.md 的 MVP 范围变更。

---

## 9. 数据、备份与迁移

### 9.1 三个目录的职责划分

| 目录 | systemd 指令 | 生命周期 | 内容 | 备份 |
|---|---|---|---|---|
| `/etc/proxy-agent` | `ConfigurationDirectory=` 0750 | 持久，unit 停止不删 | `config.toml`（**conffile**）、`secrets.toml`（0600，非 conffile） | ✓ 必须 |
| `/var/lib/proxy-agent` | `StateDirectory=` 0750 | 持久，unit 停止不删 | `configs/`（不可变版本，**核心资产**）、`state/active`、`subscriptions/`、`cache/`、`database.sqlite` | ✓ 必须 |
| `/run/proxy-agent` | `RuntimeDirectory=` 0750，`RuntimeDirectoryPreserve=no` | **易失**，unit 停止时清除 | `agent.sock`、`mihomo.sock` | ✗ 不需要 |

**职责边界原则**：

- **配置（人写的）在 `/etc`，状态（程序写的）在 `/var/lib`，运行时（易失的）在 `/run`。**
- **绝不**把运行时 socket 放到 `/var/lib`（会导致陈旧 socket 文件残留 → 启动失败）。
- **绝不**把生成的大 YAML 放进 SQLite（AGENTS.md「Persistence」）；SQLite 只存元数据，配置文件以文件形式进 `configs/`。
- `secrets.toml` **不是 conffile**，避免 dpkg 在升级时把用户生成的密钥与包内模板做三方合并。

### 9.2 最小 Backup / Restore

```text
Backup（最小集）：
  /etc/proxy-agent/config.toml
  /etc/proxy-agent/secrets.toml        （加密传输/存储，含凭据）
  /var/lib/proxy-agent/configs/        （不可变版本，核心）
  /var/lib/proxy-agent/state/          （active 指针；否则恢复后不知激活哪个版本）
  /var/lib/proxy-agent/database.sqlite （订阅、任务、审计元数据；需 WAL checkpoint 后拷）

Restore：
  1. systemctl stop proxy-agent
  2. 恢复上述目录，保持属主 proxy-agent:proxy-agent 与 0750 权限
  3. systemctl start proxy-agent
  4. proxyctl doctor          # 校验能力与环境
  5. proxyctl config list     # 校验配置版本完整性
  6. 校验 active 指针指向的版本存在且能通过 validate
```

**一致性要求**：`database.sqlite` 建议先做 WAL checkpoint（或使用 `sqlite3 .backup`）再拷贝，否则可能取到不一致快照。`configs/` 是**不可变文件**，可以安全地在运行中拷贝。

### 9.3 迁移到新机器

```text
迁移路径（与 ADR-006 D7 一致）：
  源机：proxyctl ... 停机或确保无进行中的写
  1. tar /etc/proxy-agent /var/lib/proxy-agent
  2. 新机安装同版本 deb（先装包，让 sysusers/sysdirs 就位）
  3. systemctl stop proxy-agent
  4. 解包覆盖（保持属主与权限）
  5. proxyctl doctor        # 新机能力可能不同（TUN 可用性、LXC 形态）
  6. proxyctl config list && proxyctl config validate --active
  7. 按需重新执行 Mihomo 内核下载（若新机架构不同：x86_64 ↔ aarch64！）
  8. systemctl start proxy-agent
```

**迁移的已知陷阱**：

| 陷阱 | 说明 |
|---|---|
| **架构变更** | 若从 x86_64 迁到 aarch64（或反之），`/usr/lib/proxy-agent/mihomo` 二进制不可复用，必须重新下载与校验。 |
| **能力变更** | 新机的 TUN / CAP_NET_ADMIN / nftables 能力可能不同 → 迁移后必须重跑 doctor，并把能力状态从 `Supported` 降为 `Unavailable` 等，而**不是**沿用旧结论。 |
| **socket 路径** | `/run/proxy-agent/*` **不迁移**（易失）。 |
| **Sub-Store** | 外部组件，`base_url` 通常指向本机回环地址 → 迁移后需同步迁移 Sub-Store 或更新 `base_url`；其数据目录（含订阅凭据）**不在我们的备份范围内**（R05 §4.2）。 |

### 9.4 升级时不得触碰

```text
/var/lib/proxy-agent/**                       永久用户数据（dpkg 不管理）
/run/proxy-agent/**                           tmpfs
/etc/systemd/system/proxy-agent.service.d/**  用户 drop-in
/etc/proxy-agent/secrets.toml                 非 conffile，用户生成
/etc/proxy-agent/config.toml                  conffile 语义保护（dpkg 三方合并）
```

---

## 10. 默认端口与暴露面

### 10.1 默认绑定与端口

| 服务 | 默认绑定 | 端口 | 理由 |
|---|---|---|---|
| **Agent REST API / Web Admin / `/ui`** | `127.0.0.1:8765` | 8765 | 远程可达时必须经反代 + 认证（ADR-005 D2；R12） |
| **Agent 本地 API（unix socket）** | `/run/proxy-agent/agent.sock`（0660） | — | 本地 CLI/TUI 首选；文件权限即安全边界 |
| **Mihomo controller** | **Unix socket `/run/proxy-agent/mihomo.sock`（MVP 优先）** 或 `127.0.0.1:9090` | — / 9090 | 见 10.2 —— 两种形态的安全语义**不同** |
| **Mihomo HTTP/SOCKS/Mixed 入站** | 用户可配；默认 `7890`（Mixed）、`7891`（SOCKS）、`7893`（HTTP） | 7890 系列 | **这是代理端口，按定义需对外提供服务**；`allow-lan`/`bind-address` 由用户显式配置 |
| **metacubexd** | 由 Agent 同源提供（`/ui/*`） | — | **无独立端口、无独立进程**（方案 A） |
| **Sub-Store（可选，外部）** | `127.0.0.1:3001`（**必须显式设置 host**） | 3001 | 见 10.3 |

### 10.2 红线：绝不默认 `0.0.0.0`

**Mihomo controller**：

- `[实测-文档残留]` R12 §1.5：`external-controller: ":19097"` 实际监听 `[::]:19097`（等价全网卡）；只有 `127.0.0.1:19097` 才只监听 loopback。
- `[上游源码]` `config.DefaultRawConfig()` **不设置** `external-controller` 字段；文档表格里的 `127.0.0.1:9090` 只是**推荐示例**，不是内建默认。因此 **Agent 必须显式写入 `127.0.0.1:<port>`**，不能假定默认就是 loopback。
- `[实测-文档残留]` R12 §1.1/§1.2：`secret` 为空（默认值）即**完全无鉴权**；且 **unix socket 形式下即使设置了 `secret` 也完全跳过鉴权**，socket 文件权限为 `0666`。官方文档中文原文承认："从 Unix socket 访问 api 接口不会验证 secret，如果开启请自行保证安全问题"。
- `[实测-本次]` 佐证：以 `secret: "r14m"` 运行 Mihomo 后，不带 `Authorization` 头访问 `/version` 返回 **HTTP 401** → 说明 TCP + secret 路径的鉴权确实生效（与 unix socket 的无鉴权形成对比）。

→ **结论**：TCP controller 形态必须同时满足「host 为 loopback」+「`secret` 非空」；unix socket 形态必须依赖**文件权限**（0660，owner `proxy-agent`，group `proxyctl`）作为唯一安全边界，不得依赖 `secret`。

**Agent API**：

- 默认 `127.0.0.1:8765`。判定必须基于**实际 bind 的 `SocketAddr`**（`0.0.0.0`/`::`/非 loopback 全部视为"远程可达"），而不是配置文件字符串。
- `0.0.0.0`/`::`/非 loopback 且无 token ⇒ **拒绝启动（fail-closed）**，并在日志给出明确原因。
- 只信任来自 `127.0.0.1` 的连接；**默认不信任** `X-Forwarded-For`（除非显式配置 `trusted_proxies`）。

### 10.3 Sub-Store 的绑定（最容易被忽略的高危项）

`[实测-文档残留]` R05 §1.4 / §3.3 坑 2：

```text
源码默认监听地址是 '::'（等价 0.0.0.0，所有网卡），而**官方文档写的是 127.0.0.1**。
不显式设置 SUB_STORE_BACKEND_API_HOST 时，实测日志为：
  [sub-store] INFO: [BACKEND] listening on :::3000
```

`[实测-文档残留]` 本次证据目录中的 `ss_mem.log` 与 `ss_mem2.log` **独立复现**了同一行：

```text
9/12/2026, 12:42:45 PM [sub-store] INFO: [BACKEND] listening on :::3000
9/12/2026, 12:43:44 PM [sub-store] INFO: [BACKEND] listening on :::3000
```

叠加 R05 实测的"**后端 API 默认无任何认证**"（无凭据 `GET /api/subs` → 200；无凭据 `POST /api/subs` → 201 且已落盘），构成：**任意同网段访问者即可读取订阅凭据并写入数据**。

→ **部署模板必须显式设置 `SUB_STORE_BACKEND_API_HOST=127.0.0.1`（或 `::1`）；Agent 的 doctor 应把"默认 `::`"判定为 `Misconfigured` 并告警。**

### 10.4 metacubexd 的暴露面

- 内嵌静态产物由 Agent 在 `/ui/*` 提供，**与 Agent 同源、同端口**，不需要独立端口的暴露决策，也不存在 CORS 问题。
- Clash API 通过 `/clash-api` **同源反代**：真实 `secret` 只存在于 Agent，**浏览器永远拿不到**；Mihomo controller 不对网络暴露，反代即鉴权边界（复用 Agent 的 Web 登录态）。
- 因为同源，**完全不需要** `external-controller-cors`，也**不需要** `external-ui`。
- `[实测-文档残留]` R12 §1.4：Mihomo 的 CORS 默认 `allow-origins: ['*']` + `allow-private-network: true`，即使显式写成 `[]` 实测仍返回 `Access-Control-Allow-Origin: *` → **绝不能把 controller 直接暴露给浏览器**，这正是否决 `external-ui` 方案作为默认的技术理由。

---

## 11. 对 Agent 架构的影响

### 11.1 Port 的影响

| Port | 影响 | 来源 |
|---|---|---|
| `ProcessManager` | **必须有两个适配器**：`SystemdProcessManager` + `DirectProcessManager`（容器内无 systemd 是常态）。选择依据是 doctor 的 `InitSystem`/`ContainerEnvironment` 探测结果，**不是编译期分支**。 | §2.3；R10 §4.2 |
| `MihomoController` | 传输实现必须**同时支持 HTTP 与 unix socket**。注意：上游 metacubexd UI 不支持 unix socket，但我们的反代在后端做，因此 unix socket 仍然可用——这是我们相对"浏览器直连 controller"的安全优势。 | R07 §8.2 |
| `ClashApiProxy`（**新构件**） | 复用上游 UI 的必要代价：需实现 HTTP + **WebSocket upgrade** 反代（`/clash-api` → controller）。应作为**独立 Port** 设计，位于 interfaces/infrastructure。 | R07 §6/§8.5 |
| `SubscriptionConverter` | 实现集合 = `{ SubStoreConverter, NativeConverter }`。Application 只依赖该 trait，**不得**知道 Sub-Store 的端点路径、Node/Bun、`sub-store-convert`、Sub-Store 数据库结构。 | AGENTS.md；R05 §9.1 |
| `ConfigRepository` | 必须支持不可变版本列表 + activate + rollback；`active` 通过 `/var/lib/proxy-agent/state/active` 指针表达，**不通过 SQLite 存全量 YAML**。 | AGENTS.md；§6.1/§9.1 |
| **Bundled 静态资源** | metacubexd 静态产物作为**构建期 pin 的可替换构件**（建议 `frontend/metacubexd/` 只放构建产物 + 我们剪裁的 `config.js` + `UPSTREAM_VERSION` 记录文件）。**不 fork 其源码**（MIT 允许，但长期维护负担大）。 | R07 §6 推荐结论/§8.6 |

### 11.2 配置项（`/etc/proxy-agent/config.toml` 相关新增/确认）

```toml
[agent]
bind        = "127.0.0.1"      # 绝不允许默认 0.0.0.0
port        = 8765
socket_path = "/run/proxy-agent/agent.sock"
require_auth_when_remote = true   # 启动期硬校验，fail-closed

[mihomo]
binary_path = "/usr/lib/proxy-agent/mihomo"
controller  = "unix:///run/proxy-agent/mihomo.sock"   # 或 "127.0.0.1:9090"
# 生成配置时的硬校验：external-controller 的 host 必须非空且属于 loopback
secret_required_over_tcp = true

[ui]
serve_metacubexd   = true       # 内嵌静态产物，默认开
metacubexd_path    = "/ui"
clash_api_proxy    = "/clash-api"
upstream_version   = "v1.273.1" # 构建期 pin，见 frontend/metacubexd/UPSTREAM_VERSION

[subscription.backends.substore]
enabled  = false                # ← 默认 false（可选外部组件）
base_url = "http://127.0.0.1:3001/<prefix>"
health_path = "/api/utils/env"
timeout_ms  = 10000
version_check = true            # 只检测与提示，不自动升级
managed = "external"            # external | docker | node（MVP 只允许 external）
data_path_hint = "/var/lib/sub-store"   # 仅用于 Doctor 提示，不由 Agent 创建

[process]
manager = "auto"                # auto | systemd | direct
```

### 11.3 doctor 检查项的部署相关增量

在 R05 §9.2、R10 §6.2 已有检查项之外，部署模型引入以下增量：

```text
Deployment:
  - InitSystem                   systemd / other(→ direct-process 回退) / Unknown
  - ContainerEnvironment         none | lxc | docker | unknown（含 privileged vs unprivileged）
  - ProcessManagerMode           实际选中的适配器（systemd | direct）
  - UnitPresent                  /usr/lib/systemd/system/proxy-agent.service 是否存在且已 daemon-reload
  - RuntimeDirWritable           /run/proxy-agent 可写（socket 创建前提）
  - StateDirWritable             /var/lib/proxy-agent 可写
  - ConfigDirWritable            /etc/proxy-agent 可写
  - AgentBindSafety              API 实际 bind 地址是否 loopback；非 loopback 且无 token ⇒ 启动即拒绝
  - MihomoControllerBindSafety   controller host 非空且为 loopback；unix socket 文件权限是否为 0660
  - MihomoBinaryPresent          内核二进制存在、可执行、版本可读
  - GeodataPresent               geoip/geosite 是否已预置（**缺失 + 订阅含 GEOIP 规则 ⇒ 内核启动 fatal**，见 §2.4）
  - MetacubexdAssetPresent       静态产物是否随包存在（缺 → 仅 Dashboard 不可用，非错误）

SubStore（承接 R05 §9.2）:
  - Configured? / EndpointReachable? / Version / NodeCompatibility
  - BindSafety                    SUB_STORE_BACKEND_API_HOST 是否为 127.0.0.1（默认 '::' ⇒ Misconfigured + 告警）
  - DataDirWritable               SUB_STORE_DATA_BASE_PATH 存在且可写（缺失会导致对端启动崩溃）
  - CORSConfig / ExposureWarning
```

### 11.4 架构红线（部署视角，须在 ADR/实现中写死）

```text
1. Agent API 与 Mihomo controller 绝不默认 0.0.0.0（ADR-005）
2. 绝不启用 metacubexd 的 agent / all-in-one 形态（双 supervisor）
3. Sub-Store 不由 Agent 托管、不自动安装、不自动升级；部署模板必须显式设 127.0.0.1
4. postinst / install.sh 不 enable、不 start、不覆盖用户配置、**不改用户网络配置**
5. 内核升级与配置回滚必须独立（三条链路），UI 不得出现单一 "Update" 按钮
6. Bundled Mihomo 必须履行 GPL-3.0 源码提供义务；产品命名避免 "mihomo"
7. 没有 systemd 时必须能回退 direct-process，而不是拒绝启动
8. Mihomo Bundled 时不得静态链接进 Agent 二进制（GPL 传染）
```

---

## 12. 证据与来源

### 12.1 本次实测（`[实测-本次]`）

| 实验 | 结果 | 备注 |
|---|---|---|
| Mihomo darwin arm64 极简配置空闲内存 | **11 MB** footprint（T+0/5/10/15s 稳定） | `mihomo-darwin-arm64`（28,883,922 B），`Mihomo Meta v1.19.13`，go1.25.0，with_gvisor；macOS `footprint` |
| Mihomo darwin arm64 较真实配置空闲内存 | **12 MB** footprint（3 proxy / 2 group / 4 rule / 3 listener / fake-ip DNS） | 监听 `mixed 17901`, `socks 17902`, `http 17903`, controller `127.0.0.1:19098`；`/proxies` 返回 11 项 |
| TCP controller + secret 鉴权 | 无 `Authorization` → **HTTP 401**；带 `Bearer` → `{"meta":true,"version":"v1.19.13"}` | 与 R12 记录的"unix socket 下 secret 被跳过"形成对照 |
| 离线环境 + `GEOIP` 规则 | Mihomo **fatal 退出**：`can't download MMDB`（GitHub TLS handshake timeout） | 证明离线部署必须预置 geodata |
| 环境限制 | `ps` 被沙箱禁止（`Operation not permitted`），改用 `footprint` | 说明：本机 `ps -o rss=` 不可用，故 RSS 数字用 footprint 表达 |

**本次新增实验产生的临时文件（`/tmp/r14-measure/`）已删除；所有测试进程已终止。`/tmp/r14-deploy/` 既有证据未被修改或删除。**

### 12.2 原调研证据残留（`[实测-文档残留]`，`/tmp/r14-deploy/`）

| 文件 / 目录 | 内容 | 本文引用处 |
|---|---|---|
| `ss_mem.log` | Sub-Store 2.39.6 Node 直跑内存采样：T+3s `rss_mb:93` → T+45s `93.2`；heapUsed 17.6–18.1 MB | §2.4 |
| `ss_mem2.log` | 同上 + 一次订阅加载：`idle+10s 93.5` → `after-load 94.9`；含 `[BACKEND] listening on :::3000` | §2.4、§10.3 |
| `ss_wrap.js` / `ss_wrap2.js` | 测量脚本（`process.memoryUsage()` 定时打印）；`SUB_STORE_BACKEND_PORT` 设 13000 | §2.4（方法可复现） |
| `sub-store.min.js` | 官方 bundle 1,372,614 B | §2.4（与 R05 的 3.0 MiB bundle 非同一次测量，仅作参照） |
| `package.json` / `ss_pkg_prod.json` | `sub-store@2.39.6`，`license: AGPL-3.0` | §3.1（许可） |
| `root.json` / `sub-store.json` | Sub-Store 数据文件：`root.json` = 三个 cached-resource 空对象；`sub-store.json` 含 `subs/collections/artifacts/rules/files/tokens/settings/archives/modules` + `schemaVersion: "2.0"` | §9（数据/备份边界） |
| `npm_install.log` | `added 130 packages in 41s`；`jsrsasign` deprecated 警告 | §2.4（`node_modules` 路径） |
| `node_modules/` | 120 个顶层项 / 46 MB | §2.4 |
| `config.yaml` / `cfg2.yaml` | Mihomo 测试配置（`mixed-port 17890/17891`、`external-controller 127.0.0.1:19090`、`secret` 非空） | §10.2（loopback 绑定实践） |
| `mihomo.log` / `mihomo2.log` | Mihomo 启动日志：`Geodata Loader mode: memconservative`、`Geosite Matcher implementation: succinct`、`Initial configuration complete, total time: 0ms` | §2.4 |
| `run2/` | `mm`（同 darwin 二进制副本）、`cfg.yaml`（`mixed-port 17893`、controller `127.0.0.1:19093`）、`cache.db`、`mm.log` | §10.2 |
| `cache.db` | 65536 B，**`file is not a database`** —— 实为 Mihomo 在测试目录生成的**占位/未成形文件**（未走完 SQLite 初始化即被中断），不是有效 SQLite 库。**不作为任何结论的证据** | §13（证据整理说明） |
| `art/mihomo-linux-amd64` | 32,043,156 B，`ELF 64-bit LSB executable, x86-64, statically linked, stripped` | §2.4、§6.4 |
| `art/mihomo.deb` | 官方 deb：`Package: mihomo`、`Version: 1.19.13`、`License: GPL-3.0-or-later`、`Architecture: amd64`、`Installed-Size: 31327`；`conffiles` = `/etc/mihomo/config.yaml`；含 `/usr/lib/systemd/system/mihomo.service` 与 `mihomo@.service`、`/usr/share/licenses/mihomo/LICENSE` | §6.1、§6.4 |
| `art/mcxd/` | metacubexd 静态产物解压：**8.1 MB / 158 文件**（`_nuxt/` 112 项）；`config.js` = `window.__METACUBEXD_CONFIG__ = { defaultBackendURL: '', githubToken: '' }` | §2.4、§10.4 |
| `art/compressed-dist.tgz` | 2,539,072 B（**3.0 MB**） | §2.4 |
| `mcxd.html` | GitHub Release 页 `Release v1.273.1 · MetaCubeX/metacubexd` | §2.4 |
| `systemd.exec.html` | **383,644 B**，`systemd 261.2` 的 `systemd.exec(5)` man 页抓取（含 `RuntimeDirectory=`/`StateDirectory=`/`CacheDirectory=`/`LogsDirectory=`/`ConfigurationDirectory=` 的隐含依赖与属主语义） | §6.2 |
| `art/fd-v10.2.0-x86_64-unknown-linux-{gnu,musl}.tar.gz`、`rg-musl.tar.gz`、`xlink/target/x86_64-unknown-linux-musl/` | 双变体下载对照 + cargo musl 构建残留（`r14hello` 指纹，无最终二进制）→ 说明原调研者评估过 musl 路径但**未留结论** | §6.4 |
| `ss_files.json` | Sub-Store 仓库文件清单（`sub-store-org/Sub-Store` v2.39.6） | §3.1（上游版本基线） |
| `mihomo_rel.json` | GitHub API **rate limit exceeded** | §13（原调研的取证受限点） |
| `art/just-gnu.tar.gz`、`art/rg-gnu.tar.gz` | 内容为 `Not Found`（各 9 B）→ 原调研的若干下载尝试失败 | §13 |

### 12.3 已完成的调研文档（`[上游文档]` / 交叉引用）

- `docs/research/05-sub-store-deployment.md` — §1（bundle 3.0 MiB / 零 npm 依赖 / 冷启动 130 ms / 空闲 60 MB）、§1.2（bundle 路径）、§3.3（默认 `::`、无认证、数据目录须预创建）、§4.2（数据/迁移）、§5.2（只检测不自动升级）、§8（默认方案与降级）、§9（Adapter/Doctor/配置项）
- `docs/research/07-metacubexd.md` — §1.4（双 supervisor）、§3.4（产物与镜像）、§6（方案 A/B/C 对比与推荐）、§7（自研边界）、§8（对架构的影响）
- `docs/research/09-linux-runtime.md` — C1/C2/C3（单 unit、静态用户）、C10（conffile）、§9.1（文件布局）、§9.2（sysusers.d）、§9.3（postinst 该做什么/不该做什么）、§9.4（conffile 语义）、§9.5（升级不动的东西）、§9.7（enable 时机）、§10（OpenRC defer）
- `docs/research/10-pve-lxc.md` — §1 结论 6（容器内无 systemd）、§4.2（四种模式实测，`/proc/1/comm=sh`、无 `systemctl`）、§7.1/§7.4（场景 A 零特权为最稳默认档）、§9（真实 PVE 补测清单）
- `docs/research/12-security.md` — §1.1（secret 空 = 无鉴权）、§1.2（unix socket 跳过鉴权 + 0666）、§1.4（CORS 默认 `*`）、§1.5（`":9090"` 绑全网卡）、§3（暴露面）、§5（Sub-Store 无认证）
- `docs/research/13-licenses.md` — §1.1（Mihomo GPL-3.0 + 命名限制）、§1.2（Sub-Store AGPL-3.0 与 §13 边界）、§1.3（sub-store-convert 风险）、§1.4（metacubexd MIT + Highcharts 专有 + UFL/CC-BY）、§1.6（命名限制）、§7（分发合规清单）
- `docs/research/06-sub-store-convert.md` — §1.2/§1.3/§1.5/§1.6（能力子集、无完整配置产出、失败语义、许可阻塞）
- `docs/research/01-mihomo.md` — §1.9（`/upgrade` 不可依赖）、§1.6（`/configs/geo` fire-and-forget）、§4（端点表）、§5（内核自升级不自研也不应依赖）
- `docs/Mihomo Linux Management Agent — 项目设计文档.md` §39（Config Update 与 Mihomo Update 必须独立）、§40（Health Check）、§41（Config Validation 三层）
- `docs/adr/ADR-006-deployment-model.md`（Accepted，已先行定稿）— D1–D8
- `docs/phase-0-architecture-discovery.md` R14 章节（第 682–732 行）— 四模型定义

### 12.4 上游文档 / 源码

- `[上游文档]` systemd `systemd.exec(5)`（经由证据目录 `systemd.exec.html`，版本 `systemd 261.2`）— `RuntimeDirectory=`/`StateDirectory=`/`ConfigurationDirectory=` 语义
- `[上游文档]` systemd `sysusers.d(5)`、`systemd.unit(5)`、`systemd.service(5)`（R09 §9.2 引用）
- `[上游文档]` deb(5) / deb-conffiles(5)（R09 §9.4 引用）
- `[上游文档]` Mihomo README（GPL-3.0 声明 + 下游命名限制）
- `[上游源码]` metacubexd `packages/agent/src/supervisor.ts`、`packages/agent/src/profiles.ts`、`packages/agent/src/kernel/fetch-kernel.ts`、`apps/server/*`（双 supervisor 的证据）
- `[上游源码]` Sub-Store `backend/src/restful/index.js`（`SUB_STORE_BACKEND_API_HOST || '::'`）、`backend/package.json`（AGPL-3.0）
- `[上游源码]` Mihomo `config/config.go`（`DefaultRawConfig()` 不设 `external-controller`）、`hub/route/server.go`（unix socket `Chmod 0666` + secret 置空）、`hub/route/upgrade.go`（`/upgrade` 语义）

---

## 13. 未验证假设与开放问题

### 13.1 未验证假设（明确标注）

| # | 假设 | 为什么未验证 | 影响 | 建议验证方式 |
|---|---|---|---|---|
| U1 | **Mihomo 在 Linux + TUN + 完整 geodata 下的常驻内存** | 本次与证据目录内均只有 darwin、无 TUN、无 geodata 的测量 | LXC 内存预算规划 | 在 Debian 12 LXC 内以真实订阅配置运行，采样 RSS |
| U2 | **Sub-Store 的 Docker 路径资源占用** | R05 §3.1：Docker Hub 不可达，`docker pull xream/sub-store` 失败；R05 的 60–120 MB 是 `[推测]` | 若用户选 Docker 部署，容量规划无实测依据 | 在有镜像源的机器上实测 |
| U3 | **`[推测]` PVE LXC 下 Docker-in-LXC 的实际代价** | 未在真实 PVE 上验证（R10 §9 补测清单未完成） | §8.1 "不提供官方镜像"的核心论据之一**目前是推测** | 在真实 PVE（含 unprivileged + nesting）中实测 |
| U4 | **musl vs glibc 的最终选择** | 证据目录有 musl 构建残留但无结论产物 | 分发兼容性与体积 | 两条工具链各构建一次，在 Debian 12 / Ubuntu 22.04 上跑 E2E |
| U5 | **`DirectProcessManager` 的完整行为** | 未做实验（尤其 Agent 被 SIGKILL 后孤儿 Mihomo 的回收） | 容器内无 systemd 场景的可靠性 | 在无 systemd 容器中做 kill 测试 |
| U6 | **Clash API 反代的 WS upgrade 可行性** | R07 §10.3：未做端到端联调；`ky` 在 `/clash-api` 相对地址下的行为、axum 侧 WS 透传写法均为设计推断 | Dashboard 实时功能能否工作 | 起真实 Mihomo + 浏览器实际操作 |
| U7 | **metacubexd 静态产物能否接受"占位 secret + 反代覆盖 Authorization"** | R07 §10.3，未验证 | 内嵌方案 A 的前置条件 | 端到端联调 |
| U8 | **Native 解析（降级态）的确切能力边界** | 本文 §5.3 只能给出"支持直连格式、不支持 operators/scripts/mergeSources"的粗粒度边界 | 影响"开箱即用"预期与错误报告语义 | 明确格式白名单 + 失败语义规范，写进设计文档/ADR |
| U9 | **`/tmp/r14-deploy/cache.db` 的性质** | `file is not a database`（65,536 B 全部为零/非 SQLite 头）→ 实为 Mihomo 未完成初始化的占位文件 | 无。**本文未基于它做任何结论** | 无需验证；记录以澄清它不是"SQLite 存储方案"的证据 |
| U10 | **原调研者是否已选定 musl** | 仅留下下载对照与 cargo 空壳 target 目录，无结论文件 | 见 U4 | 无需验证；本文明确写为未验证 |
| U11 | **Mihomo Bundled vs 首次运行时下载** | ADR-006 D4 记录"二者取一需在实现前定" | 影响 GPL-3.0 源码提供义务的落地位置（`source-offer.txt`） | 实现前决策 + E2E 验证 |
| U12 | **metacubexd 内嵌的 Highcharts 专有许可如何处置**（替换图表组件 / 取得授权 / 不内嵌该页面） | R13 §1.4 标 `[法律问题-待确认]` | **可能阻塞静态内嵌分发** | 法务/上游确认；或评估移除该图表依赖的构建变体 |
| U13 | **离线环境必须预置的 geodata 清单与体积** | 本次只证明了"缺 geodata + GEOIP 规则 ⇒ fatal"，未枚举所需文件与体积 | 离线部署包大小与 doctor 检查项 | 枚举 `geoip.metadb`/`geosite.dat`/`geodata` 用途与体积 |

### 13.2 开放问题（建议进 `docs/research/open-questions.md`）

1. **Native 解析的能力白名单与失败语义**：如何严格区分"订阅为空"与"拉取/解析失败"？（R06 的失败模式正是反例）这直接关系核心不变量，建议在实现前用 ADR 固定。
2. **`DirectProcessManager` 的孤儿回收**：Agent 被 SIGKILL 后，如何保证 Mihomo 不成为无人管理的孤儿进程？（PID 文件 + 启动期对账？systemd 场景下用 `KillMode=control-group`，无 systemd 场景靠什么？）
3. **`doctor` 是否应主动纠正 Sub-Store 的 `::` 绑定**：仅告警，还是提供"生成一个正确绑定的 compose/systemd 模板"？后者会引入"Agent 生成外部服务的部署产物"这一新职责，需评估是否越界。
4. **metacubexd 版本跟随策略**：固定 pin（推荐）还是允许用户配置 tag？如何做回归验证？（R07 §10 问题 5）
5. **是否提供 `proxyctl install-substore` 之类的辅助命令**：这会越过 AGENTS.md "外部集成可替换 / 不托管外部服务"的界线，需要明确是"生成模板"还是"代用户执行"。
6. **多实例预留**：R09 §Q6 建议现在不加 `proxy-agent@.service` 模板，但目录命名要避免成为障碍。R14 的默认端口分配（8765/9090/7890 系列）在多实例下必须有偏移规则——目前**没有**。
7. **`x86_64` 与 `aarch64` 的 Mihomo 内核分发**：deb 里各带一份会让包体积翻倍（每份约 30 MiB）；是否改为首次运行时按架构下载（并因此需要处理"离线安装"路径）？与 U11 联动。
8. **容器化用户的文档路径**：我们不发布镜像，但用户会自己容器化。是否需要一份"容器内无 systemd 时的 direct-process 模式"的运行说明？

---

## 附：一句话交付

**R14 选定 Model D：`proxy-agent` 单 unit 托管 Mihomo 子进程（实测空闲 11–12 MB）、metacubexd 以内嵌静态产物 + 同源反代形态存在（明确排除官方 All-in-One Server 的双 supervisor 冲突）、Sub-Store 为默认不安装的可选外部组件（实测 Node 直跑空闲约 93–95 MB，AGPL-3.0 边界靠"独立进程 + 仅 HTTP"保持）；最小可用组合仅需 Agent + Mihomo，主分发形态为 deb（conffile + sysusers.d + StateDirectory，postinst 不 enable/不 start/不改网络），三条升级链路（Agent / 内核 / 配置回滚）互不耦合且内核升级禁止依赖 `/upgrade`，MVP 不提供官方容器镜像。**
