# ADR-006 — 部署模型与分发

| 字段 | 值 |
|---|---|
| Status | Accepted（Phase 0 定稿） |
| Date | 2026-09-12 |
| Related | ADR-001、ADR-002、ADR-003、ADR-005、`docs/research/14-deployment-model.md`、`05-sub-store-deployment.md`、`07-metacubexd.md`、`09-linux-runtime.md`、`13-licenses.md` |

---

## 1. Context

### 1.1 目标环境

```text
Debian / Ubuntu + systemd + PVE LXC，x86_64 / aarch64
```

### 1.2 候选模型（Phase 0 规范 §R14）

| 模型 | 组成 |
|---|---|
| A | Rust Agent + Mihomo + metacubexd |
| B | A + Sub-Store |
| C | Rust Agent + Mihomo + Native Converter + metacubexd |
| D | Rust Agent + Mihomo + **Optional** Converter + metacubexd |

### 1.3 决定性调研结论

| # | 结论 | 证据 |
|---|---|---|
| C1 | **Sub-Store 可完全可选且成本极低**：官方 bundle 3.0 MiB、零 npm 依赖、冷启动 130 ms、空闲 60 MB → 但**没有必要默认捆绑** | R05 §1 |
| C2 | **Sub-Store 是 AGPL-3.0**；"独立进程 + 仅 HTTP + 不修改"不触发 §13 | R13 §1/§3 |
| C3 | **sub-store-convert 判定 Rejected**（含许可阻塞） | R06 §1/§7、ADR-002 |
| C4 | **metacubexd 已含 agent/supervisor**，其官方 All-in-One Server 会与我们的 systemd 托管形成**双 supervisor 冲突** → 只能取其**静态产物** | R07 §1/§4 |
| C5 | metacubexd 应用 MIT，但**依赖含 Highcharts（专有）**+ UFL-1.0 字体 + CC-BY-4.0 资源 → 内嵌分发前必须处理 | R13 §1 |
| C6 | **容器内常常没有 systemd**（`/proc/1/comm=sh`）→ 必须有 direct-process 回退 | R10 §1 |
| C7 | **单 unit 不双 unit**；静态用户 `proxy-agent`（非 `DynamicUser=`）；Mihomo 是 Agent 子进程 | R09 C1/C2/C3 |
| C8 | deb 用 conffile 保护用户配置；unit 放 `/usr/lib/systemd/system/`；postinst **不 enable/不 start/不覆盖** | R09 C10 |
| C9 | Mihomo 为 **GPL-3.0**，且 README 有**命名限制**（下游项目名不得含 `mihomo`） | R13 §1 |
| C10 | 内核更新必须自研（禁依赖 `/upgrade`） | R01 §1、ADR-003 |
| C11 | PVE LXC 下 Docker-in-LXC 的代价与不确定性高（未验证） | R10 |

---

## 2. Decision

### D1. **默认模型 = Model D**（Agent + Mihomo + Optional Converter + metacubexd 静态产物）

```text
                    ┌──────────────────────────────┐
   Browser  ──────► │  proxy-agent :8765           │
                    │  ├── /api/v1/*   (REST)      │
                    │  ├── /ws/v1/*    (WS)        │
                    │  ├── /admin/*    (自研 UI)    │
                    │  ├── /ui/*       (metacubexd │  ← 静态产物内嵌，同源反代
                    │  │               静态资源)    │
                    │  └── /clash-api  (反代)       │
                    └───────┬──────────────┬───────┘
                            │              │
                ┌───────────▼───┐   ┌──────▼─────────┐
                │ Mihomo 子进程  │   │ Sub-Store（可选）│
                │ (systemd 经    │   │ 外部组件，      │
                │  Agent 托管)   │   │ 默认不安装       │
                └───────────────┘   └────────────────┘
```

**选择 D 的理由**

1. **最小可用组合不依赖外部服务**：只装 Agent + Mihomo 即可提供 HTTP/SOCKS/Mixed 代理（C1/C3 允许 Sub-Store 缺席）。
2. **避免双 supervisor**：metacubexd 只以静态资源形态内嵌，其 agent/内核管理**不启用**（C4）。
3. **许可证义务最小化**：Sub-Store 作为独立进程（C2），sub-store-convert 不用（C3），Mihomo 以独立二进制分发（C9）。
4. **降级清晰**：没有 Sub-Store 时订阅转换降级为 Native 直连解析；没有 Dashboard 不影响核心功能。

### D2. 组件职责与进程模型

| 组件 | 形态 | 谁拉起 | 运行用户 | 必须 | 失败域 |
|---|---|---|---|---|---|
| `proxy-agent` | systemd unit（唯一） | systemd | `proxy-agent` | ✓ | Agent 挂了 → Mihomo 子进程随之停止 |
| Mihomo | Agent 的 fork/exec 子进程 | `proxy-agent` | 同 unit 用户（ambient cap 传权） | ✓ | 崩溃由 Agent 退避重启 |
| metacubexd | **静态文件**（无进程） | — | — | 否 | 前端加载失败不影响后端 |
| Sub-Store | 外部 Node/Docker 进程 | **用户自行** | 用户自定 | 否 | 不可达 → 订阅更新失败，**旧配置保持** |

**关键**：metacubexd **不是进程**；Sub-Store **不由 Agent 托管**（不自动安装/不自动升级）。

### D3. 最小可用组合与降级路径

```text
最小组合（MUST 可用）:  proxy-agent + mihomo
   → HTTP/SOCKS/Mixed 代理、配置版本化与回滚、CLI/TUI/API、Doctor

+ metacubexd 静态产物   → Dashboard（可选，Agent 已内置则无需额外部署）

+ Sub-Store（可选）      → 订阅转换能力完整
   缺失时: Native 直连解析（可能仅支持部分订阅格式）
   Sub-Store 故障时: 订阅更新失败 → 当前激活配置不变（绝不停止 Mihomo）
```

### D4. 分发与安装

**主分发形态：deb 包**（install.sh 作为无包管理环境的次选）。

```text
/usr/bin/proxy-agent                  二进制（Agent；含内嵌 admin UI 与 metacubexd 静态资源）
/usr/bin/proxyctl                     CLI
/usr/lib/systemd/system/proxy-agent.service
/usr/lib/sysusers.d/proxy-agent.conf  创建 proxy-agent 用户与 proxyctl 组
/etc/proxy-agent/config.toml          conffile（dpkg 保护，升级不覆盖用户修改）
/var/lib/proxy-agent/                 StateDirectory（configs/ state/ database.sqlite）
/run/proxy-agent/                     RuntimeDirectory 0750（agent.sock、mihomo.sock）
/usr/share/doc/proxy-agent/           copyright(DEP-5)、THIRD-PARTY-NOTICES、licenses/
```

**postinst 不得**：enable、start、写用户配置、改网络（C8）。是否启用交给用户显式操作。

**Mihomo 二进制**：作为 deb 的一部分（Bundled）或首次运行时由 Agent 下载，二者取一需在实现前定；若 Bundled 则必须满足 GPL-3.0 的源码提供义务（C9、R13 §7）。**E2E 验证项**。

**构建目标**：`x86_64-unknown-linux-gnu` 与 `aarch64-unknown-linux-gnu`；是否改用 musl 静态链接待评估（glibc 兼容性 vs 体积），列入 open-questions。

### D5. 三条独立升级链路（不可耦合）

```text
① Agent 自身升级     : deb 升级 / 二进制替换（不影响运行中的 Mihomo 配置）
② Mihomo 内核升级     : 下载 → 校验 → 原子替换 → 重启 → 健康检查 → 失败回滚
③ 配置回滚           : 激活历史 ConfigVersion → reload → 健康检查 → 失败回滚
```

UI 不得出现单一 "Update" 按钮（设计文档 §39、ADR-004 D6）。

### D6. 容器/镜像策略

- **MVP 不提供官方 Docker 镜像**。理由：PVE LXC 下 Docker-in-LXC 代价与不确定性高（C11）；且容器形态会与 systemd 托管形成双 supervisor（C4）。
- 允许用户自行容器化，但 Agent 必须能检测"无 systemd"并回退 direct-process 模式（C6）。
- metacubexd 官方 All-in-One Server 镜像**明确不采用**。

### D7. 数据、备份与迁移

| 目录 | 内容 | 是否需备份 |
|---|---|---|
| `/etc/proxy-agent` | `config.toml`（conffile） | ✓ |
| `/var/lib/proxy-agent/configs` | 不可变配置版本 | ✓（核心资产） |
| `/var/lib/proxy-agent/state` | active 指针 | ✓ |
| `/var/lib/proxy-agent/database.sqlite` | 元数据（订阅、任务、审计） | ✓ |
| `/run/proxy-agent` | socket（易失） | — |

**迁移到新机器**：`/etc/proxy-agent` + `/var/lib/proxy-agent` 整体拷贝 + `proxyctl doctor` + `proxyctl config list` 校验即可；不依赖机器特定状态（除 socket 路径）。

### D8. 默认端口与暴露面

| 服务 | 默认绑定 | 端口 |
|---|---|---|
| Agent API/Web | `127.0.0.1:8765`（远程需反代 + 认证） | 8765 |
| Mihomo controller | Unix socket `/run/proxy-agent/mihomo.sock`（MVP 优先）或 `127.0.0.1:9090` | — |
| Mihomo Mixed 入站 | `0.0.0.0:7890`（**用户可配**；这是代理端口，需对外） | 7890 |
| Sub-Store | `127.0.0.1:3001`（**必须显式设置 host**，上游默认 `::`） | 3001 |
| metacubexd | 由 Agent 同源提供（`/ui`） | — |

**红线**：Agent API 与 Mihomo controller **绝不默认 0.0.0.0**（ADR-005 D2）。

---

## 3. Alternatives

| 方案 | 拒绝理由 |
|---|---|
| Model B（默认捆绑 Sub-Store） | 增加默认部署复杂度与 AGPL 义务面；Sub-Store 不在关键路径，失败已可降级（C1/C2） |
| Model C（MVP 自研 Native Converter） | 范围爆炸（产品范围 WON'T）；上游已成熟 |
| 使用 metacubexd 官方 All-in-One Server | 双 supervisor 冲突（C4） |
| 双 unit（systemd 管 mihomo） | 跨用户后 Agent 无法 kill/读写其 socket；polkit 复杂（R09 C2/C3） |
| 提供官方 Docker 镜像作为主分发 | PVE LXC 下不确定性 + 双 supervisor（C4/C11） |
| `install.sh` 作为主分发 | 无包管理语义，升级/卸载/依赖不可追溯；deb 才是 Debian/Ubuntu 的正确形态 |
| 把 Mihomo 静态链接进 Agent 二进制 | GPL-3.0 传染（R13 §1）→ 整个二进制须 GPL |
| 用 `NOPASSWD: ALL` 让 Agent 提权 | 等于把 root 交给 Web 层（ADR-005 D4） |

---

## 4. Consequences

### 4.1 正面

- 默认安装即得可用代理 + 配置版本化 + Doctor，**不强制引入任何外部服务**。
- 许可证边界清晰：Sub-Store 独立进程、metacubexd 静态产物（需处理 Highcharts）、Mihomo 独立二进制。
- 升级/回滚三条链路独立，失败域隔离。
- 无 systemd 环境仍可运行（direct-process 回退）。

### 4.2 负面 / 成本

- 用户要 Dashboard 之外的订阅能力需自行部署 Sub-Store（文档需提供明确的部署指引）。
- 内嵌 metacubexd 静态产物需处理 **Highcharts 专有许可**（C5）——可能需替换该图表组件或取得授权。
- Mihomo Bundled 时的 GPL-3.0 源码提供义务需要 deb 打包流程支持（`source-offer.txt`）。
- direct-process 回退模式意味着我们要自己实现重启与生命周期（systemd 场景下是免费的）。

### 4.3 必须遵守

```text
1. Agent API 与 Mihomo controller 绝不默认 0.0.0.0（ADR-005）
2. Sub-Store 不由 Agent 托管、不自动升级；部署模板必须显式设 127.0.0.1
3. 不采用 metacubexd agent/all-in-one 形态（双 supervisor）
4. postinst 不 enable/不 start/不覆盖用户配置
5. 内核升级与配置回滚必须独立（三条链路）
6. Bundled Mihomo 必须履行 GPL-3.0 义务；命名避免 "mihomo"（C9）
```

---

## 5. Evidence

- `docs/research/14-deployment-model.md`（四模型对比与默认模型）
- `docs/research/05-sub-store-deployment.md` §1（bundle 体积/内存/零依赖/`::` 默认绑定）
- `docs/research/07-metacubexd.md` §1/§4（静态产物方案、双 supervisor 冲突、官方 server 不采用）
- `docs/research/09-linux-runtime.md` C1/C2/C3/C8/C10（单 unit、静态用户、socket、conffile、目录布局）
- `docs/research/10-pve-lxc.md` §1 C6（容器内无 systemd）
- `docs/research/13-licenses.md` §1/§3/§7（GPL-3.0 义务、AGPL 隔离、metacubexd 依赖许可、命名限制）
- `docs/research/01-mihomo.md` §1（`/upgrade` 不可依赖）
- 设计文档 §32–§34、§46–§47、§54–§55（数据布局、Web 与 Agent 分离、安装流程、用户权限）
