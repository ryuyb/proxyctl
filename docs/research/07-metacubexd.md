# R07 — metacubexd 能力分析与复用策略

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：`[实测]` + `[上游源码]` + `[上游文档]`
> 关键结论一句话：**metacubexd 已经不是"纯前端 Dashboard"，它自带 Control Agent（kernel supervisor + profile/配置生命周期 + 订阅调度），并有 Desktop 与 All-in-One Server 两种"自带内核"形态；因此我们的 Agent 必须避开"进程管理 + 配置生命周期"这条重叠带，只做 Linux 原生编排（systemd / LXC / nftables / Doctor / 多节点），Web 端则复用其 Dashboard 并自研管理面。**

---

## 1. 结论摘要（TL;DR）

1. `[实测]` **设计文档假设的仓库结构基本正确，但少列了一个包**：实际顶层是 `packages/ui`、`packages/agent`、`packages/config-editor`、`apps/server`、`apps/desktop`（pnpm workspace：`packages/*` + `apps/*`）。设计文档（R07 章节第 459-464 行）漏了 `packages/config-editor`，并且把 `apps/desktop` 与 `packages/agent` 的职责理解反了——**Agent 逻辑不在 `apps/*` 里，而在 `packages/agent` 这个框架无关的库**；`apps/desktop` 只是 Electron host + 特权 TUN helper + 内核打包。
2. `[实测]` **metacubexd 已经覆盖"进程管理 + 配置生命周期 + 内核版本"**：`packages/agent/src/supervisor.ts` 直接 `spawn()` 托管 mihomo 子进程（含启动/停止/重启/`mihomo -t` 校验/崩溃自动重启/SSE 子进程日志），`profiles.ts` 实现 profile 的 compose/activate/.bak 回滚，`kernel/fetch-kernel.ts` + desktop `kernel-manager.ts` 实现从 GitHub Releases **下载并热切换内核版本**。这与我们设计的 `MihomoController` / `ConfigRepository` / `ProcessManager` 大面积重叠。
3. `[实测]` **但它的"进程管理"只面向 POSIX 通用语义与容器/桌面场景**：没有 systemd 单元管理，没有 PVE LXC 能力探测，没有 nftables/TProxy/route 编排，没有多实例（一个进程 = 一个 profile store + 一个 supervisor + 一份 `active.yaml`），没有内核与配置的**多版本历史**（回滚只有单槽 `.bak`）。这正是我们的不可替代价值区。
4. `[实测]` **关键分界线是"谁托管内核"**：官方 All-in-One Server 镜像（`ghcr.io/metacubex/metacubexd-server`）会自带 mihomo 并在容器内托管它。若我们再用它，就会出现**两个都想做 supervisor 的进程**（我们的 systemd 管 mihomo vs 容器内 supervisor 管 mihomo），这是架构冲突，不是集成选项。
5. **推荐默认方案（方案 A'）**：我们自研 Agent 以**静态资源**方式托管 metacubexd 构建产物（`/ui` 子路径，hash 路由 + 相对资源），并提供 **same-origin 的 Clash API 反向代理**（`/clash-api` → 本地 mihomo controller，优先 unix socket）；Mihomo 的 `external-controller` 只绑 `127.0.0.1`/unix socket，**不依赖 `external-ui`、不依赖 Mihomo 侧 `external-controller-cors`、不把 controller 暴露到网络**。metacubexd 的 `/api/control`（Agent 形态）**不启用**。

---

## 2. 仓库真实结构（与设计文档假设的差异）

### 2.1 实际结构 `[实测]`

来源：`git clone` / tarball 解包（`main` 分支，commit 时间 2026-09-10，版本 `1.273.1`）+ 本地验证。工作副本位于 `/tmp/r07-metacubexd/src/metacubexd-main`（临时目录，任务结束后清理）。

| 路径 | 类型 | 用途（证据） |
| --- | --- | --- |
| `packages/ui` | Nuxt 4 / Vue 3 CSR Dashboard | 纯前端面板，`ssr: false` + `router.options.hashMode: true`（`packages/ui/nuxt.config.ts`） |
| `packages/agent` | **框架无关的 Control Agent 库**（非 Node 服务） | supervisor / profiles / profile-editor / scheduler / kernel 下载 / geo / WebDAV / TUN 抽象 / 兼容层 / `/api/control` 路由（h3） |
| `packages/config-editor` | 共享配置补丁模型 | 只有 `src/index.ts`（`ConfigPatchV1` 等），被 ui 与 agent 同时依赖 |
| `apps/server` | Nitro all-in-one server | 静态托管 UI（`publicAssets`）+ 把 `getAgent().router` 挂到 `/api/control`（`apps/server/routes/api/control/[...].ts`） |
| `apps/desktop` | Electron 桌面应用 | 主进程 kernel-manager / TUN helper / sysproxy / 托盘 / 更新；打包 mihomo 二进制 |
| `Casks/metacubexd.rb` | Homebrew cask | macOS 分发 |
| `docs/` | 上游文档 | `config.yaml`（示例 mihomo 配置）、`docker-compose.yml`、截图 |

`pnpm-workspace.yaml`：`packages: ['packages/*', 'apps/*']`，全部依赖走单一 `catalog:`；根 `package.json` 版本 `1.273.1`，`packageManager: pnpm@10.34.1`，`.node-version` = `24`。

上方 `[上游源码]` 的权威自述见 `.github/copilot-instructions.md`「Monorepo Map」表（四 workspace）与 `CONTEXT.md`（术语表：Clash API / Control API / Control Agent / Kernel / Bundled Kernel）。

### 2.2 与设计文档假设的差异（明确列出）

| 设计文档假设（`docs/phase-0-architecture-discovery.md` L459-464） | 实际 | 影响 |
| --- | --- | --- |
| `packages/ui` | ✅ 存在 | 无 |
| `packages/agent` | ✅ 存在，且**是核心**：包含 kernel supervisor / 配置 compose+activate / 订阅调度 / `/api/control` | **重大**：设计文档低估了它的职责 |
| `apps/server` | ✅ 存在（Nitro），all-in-one 形态，自带内核 | 无 |
| `apps/desktop` | ✅ 存在（Electron），但只是 host；内核管理逻辑在 `packages/agent` + `apps/desktop/src/main/kernel-manager.ts` | 中 |
| （未提及）`packages/config-editor` | ✅ 实际存在，共享 `ConfigPatchV1` 补丁协议 | 小 |
| 设计文档 §29（L1387）"官方仓库当前也包含独立面板与服务器模式" | ✅ 属实，且**还包含桌面模式与两个官方镜像** | 需修正"第一阶段简单静态托管"的定位描述 |

---

## 3. 能力盘点（能力 × 证据路径）

### 3.1 Dashboard 功能（Clash API consumer，纯前端）`[上游源码]`

| 能力 | 路由/页面 | 主要来源 |
| --- | --- | --- |
| 概览（流量实时图 + 内核健康） | `packages/ui/pages/overview.vue` | `components/RealtimeLineChart.vue`、`GlobalTrafficIndicator.vue` |
| 代理组管理 / 选中 / 测速 | `pages/proxies.vue` | `components/ProxyNode*.vue`、`composables/useLatencyTest.ts`、`useBatchLatencyTest.ts` |
| 连接列表 / 关闭单条或全部 | `pages/connections.vue` | `components/connections/*`、`useApi.ts: closeSingleConnectionAPI/closeAllConnectionsAPI` |
| 规则查看 / 编辑 | `pages/rules.vue` | `composables/useRuleEditor.ts` |
| 实时日志（Clash WS） | `pages/logs.vue` | `composables/useWebSocket.ts` |
| 流量统计（含上周区间） | `pages/traffic.vue` | `pages/traffic.vue`、`composables/useDataUsage.ts` |
| 配置查看与热改（Clash API） | `pages/config.vue` | `useApi.ts: fetchBackendConfigAPI / updateBackendConfigAPI / reloadConfigFileAPI` |
| Profile 管理（Agent 形态） | `pages/profiles.vue`、`profiles/[id]/edit.vue` | `composables/useProfiles.ts`、`useActiveProfileEditor.ts` |
| 控制中心（Agent 形态） | `pages/control.vue` | `components/KernelControlPanel.vue`、`KernelVersionPanel.vue`、`KernelLogView.vue`、`SystemProxyControlPanel.vue` |
| 后端连接入口 / 多 endpoint | `pages/index.vue`、`pages/setup.vue` | `stores/endpoint.ts`、`components/ConnectForm.vue` |
| 其他 | 32 主题 / 7 语言 / PWA / 移动端适配 | `README.md`、`nuxt.config.ts`（i18n、pwa） |

### 3.2 与 Mihomo 通信方式 `[实测]` + `[上游源码]`

- **两条独立通道**（`CONTEXT.md` + `.github/copilot-instructions.md`「API Boundary」）：
  - **Clash API**：mihomo `external-controller` 的 HTTP + WebSocket，Dashboard 直连，不经 Agent 代理。
  - **Control API**：`/api/control/**`，h3 路由，由 Agent（server/desktop）提供。
- **endpoint 配置**：用户填 `{url, secret}`，存浏览器 `localStorage`（`stores/endpoint.ts` 的 `useLocalStorage('endpointList')`；`types/index.ts: Endpoint {id,url,secret,label}`）。
- **secret 传递**：每个请求带 `Authorization: Bearer <endpoint.secret>`（`useApi.ts` L224-232）。
- **WS 地址推导**：`http:`→`ws:`、`https:`→`wss:`（`stores/endpoint.ts: wsEndpointURL`）。
- **CORS**：Browser 直连 controller 是**跨源**请求，需 mihomo 侧 `external-controller-cors.allow-origins` 放行 Dashboard origin（`README.md` "Unable to connect to backend when self-hosting (CORS)"；示例 `docs/config.yaml`）。注意上游 `config.yaml` 模板里该字段默认是 `allow-origins: ["*"]` `[上游源码: MetaCubeX/mihomo docs/config.yaml L72-75]`，但官方 README 仍要求自托管者显式配置。
- **unix socket：不支持** `[实测/源码推断]`。`packages/ui/utils/index.ts: transformEndpointURL` 对非 `http(s)://` 开头的输入会拼成 `${window.location.protocol}//${url}`，`checkEndpointAPI` 直接 `ky.get(url + '/version')`，`wsEndpointURL` 用 `new URL()` 解析；因此 `unix:///run/mihomo.sock` 这类地址无法作为 endpoint 使用。→ **若我们坚持 unix socket 作为默认控制通道，必须由 Agent 提供 HTTP/WS 反向代理**。
- **API proxy**：`apps/server/nitro.config.ts` 明确写了 **故意不做 Clash API 代理**（"NOTE: Intentionally NO Clash-API proxy here"，原因：nitro routeRules proxy 不能升级 WebSocket，见 nitrojs/nitro#2886）。

### 3.3 是否含"内核/进程/配置写入"能力（与 Agent 的分界线）`[实测]`

**结论：全都包含，且是生产级实现，不是玩具。** 证据：

| 能力 | 证据路径 | 要点 |
| --- | --- | --- |
| 进程托管 | `packages/agent/src/supervisor.ts` L272/L370 `spawn(binaryPath, ['-d', homeDir, '-f', activeConfigPath])` | 启动/停止/重启、`intentionalStop` 区分用户操作与崩溃、启动就绪轮询（读 `GET /version`）、SSE 日志 |
| 崩溃自愈 | 同上，`autoRestart`/`maxRestarts`/`restartBackoffMs`/`stableRestartMs` | 连续崩溃计数 + 退避 + 稳定性窗口，超过上限停在 `errored` |
| 生命周期串行化 | `supervisor.ts` 顶部 "Tiny async mutex: serializes lifecycle ops so two tabs can't double-spawn" | 与我们"per-instance lock"设计意图一致 |
| 配置校验 | `supervisor.ts: validate()` → `spawn(binaryPath, ['-t','-d',homeDir,'-f',configPath])`，默认 300s（首次可能下载 GEO） | 真正的 `mihomo -t`，失败返回 stderr |
| 配置写入/注入 | `supervisor.ts: injectClashConfig()` | spawn 前**就地重写 active.yaml**：剥离 profile 里的 `external-controller`/`secret`/`mixed-port` 与冲突的 `port/socks-port/redir-port/tproxy-port`，再前置托管值 |
| 配置生命周期 | `packages/agent/src/profiles.ts` | profile CRUD / duplicate / importFromUrl（UA `clash.meta`）/ refresh / compose（base + merge + script 分层）/ setActive（先 `copyFile(active.yaml → .bak)`，再原子写）/ rollback / resetActive |
| 配置版本/回滚 | 同上 L322-324、L405-421 | **只有单槽 `.bak`**（"the previous setActive"），没有多版本历史、没有 diff、没有 checksum |
| 订阅 | `profiles.ts` + `scheduler.ts`（默认 60s tick，按 `updateInterval` 分钟数刷新；`refresh-apply.ts` 命中 active 时重新 compose + 重启） | 有自动更新调度；有 `Subscription-Userinfo` 解析（`upload/download/total/expire`） |
| 内核版本管理 | `packages/agent/src/kernel/fetch-kernel.ts`（下载 + gunzip/unzip + chmod 0755）、`listMihomoVersions()`（过滤 semver tag）；desktop `apps/desktop/src/main/kernel-manager.ts`（下载 → 写冷启动 override → `setBinaryPath` + `restart()`） | **UI 面板 `components/KernelVersionPanel.vue` 依赖 `kernel-version` feature** |
| `/api/control` 路由全貌 | `packages/agent/src/http.ts` | 见下表 |

`/api/control` 路由清单（`packages/agent/src/http.ts`，`PREFIX = '/api/control'`）`[实测]`：

```text
GET  /health                              (public)
GET  /info                                (public, 能力声明 features[])
GET  /kernel/status
POST /kernel/start | stop | restart
POST /kernel/rollback                     (恢复 active.yaml.bak)
POST /kernel/recover                      (重置为最小配置 + 重启)
GET  /kernel/logs                         (SSE；支持 ?token=)
GET/POST/PUT/DELETE /profiles[...]        (list/create/read/update/delete/duplicate/import/refresh/
                                           refresh-and-activate/activate/validate)
GET/PUT/DELETE /profiles/:id/editor[...]  (Monaco 可视化编辑补丁 + preview + overlay)
GET/PUT /config, GET /config/runtime, GET/PUT /config/section
POST /geo/update
POST /backup | /restore                   (WebDAV)
GET/POST /sysproxy                        (需 systemProxy 注入；否则 404/handler)
GET/POST /kernel/versions | /kernel/switch(需 kernelManager 注入)
GET/POST /tun, POST /tun/uninstall        (需 tunController 注入)
```

能力按 host 注入 **capability-gated**（`packages/agent/src/index.ts: createAgent()` 的 `features[]`：`profiles` / `logs-sse` / `kernel-control` / `geo-assets` / `webdav-backup` / `runtime-config` / `config-sections` / `visual-config-editor`，以及可选的 `system-proxy` / `kernel-version` / `tun`）。

**`[实测]` 重要边界差异：`apps/server` 并不注入 `kernelManager` / `systemProxy` / `tunController`。**
`apps/server/lib/supervisor.ts: getAgent()` 只传 `binaryPath / homeDir / profilesDir / activeConfigPath / agentToken / externalController / secret / mixedPort`。
→ 因此 **All-in-One Server 没有"内核版本切换"能力**（内核是构建期 `MIHOMO_VERSION=v1.19.27` 烧进镜像的，见 `apps/server/Dockerfile` L31-37 与 `.github/workflows/release.yml` L128；只能通过 `MIHOMO_BIN` 指向自定义二进制），也**没有** system-proxy / TUN。只有 **Desktop** 有内核版本切换、TUN、系统代理。

### 3.4 部署方式与产物 `[实测]`

| 形态 | 入口 | 产物/镜像 |
| --- | --- | --- |
| Hosted panel（静态） | `pnpm build:ui` → `nuxt generate` | `packages/ui/.output/public` —— 我本地构建成功：**7.6 MB / 126 个文件**，含 `index.html`、`200.html`、`404.html`、`config.js`、`sw.js`（PWA precache 118 entries / 6116 KiB） |
| Hosted panel（容器） | `packages/ui/Dockerfile` | `ghcr.io/metacubex/metacubexd`（Nuxt node-server，仅 Dashboard，端口 80） |
| All-in-One Server | `apps/server/Dockerfile` | `ghcr.io/metacubex/metacubexd-server`（Nitro + Agent + 内核；端口 8080/9090/7890；`tini` 做 PID 1 回收内核） |
| Desktop | `apps/desktop` electron-builder | `MetaCubeXD-<ver>-{mac,win,linux}` 安装包（未签名；README 有 Gatekeeper/SmartScreen 说明） |
| Homebrew | `Casks/metacubexd.rb` | `brew install --cask metacubexd` |
| 静态发布渠道 | `.github/workflows/release.yml` L63-69 `peaceiris/actions-gh-pages@v4`，`cname: d.metacubex.one` | `gh-pages` 分支 = 生产静态产物（我核对了 `gh-pages/index.html`：`appVersion:"1.273.1"`，资源为相对路径 `./_nuxt/...`） |
| Helm / K8s | 无 | `find` 无 Chart.yaml / k8s 清单；只有 `docs/docker-compose.yml` 与 `apps/server/compose.yaml` |

- **base path**：`nuxt.config.ts` 设 `app.baseURL = process.env.NUXT_APP_BASE_URL || '/'`，且注释明确"Use relative paths for assets to support both root and subdirectory deployments"；desktop 用 `NUXT_APP_BASE_URL=./`。`[上游源码]`
- **运行时注入**：`packages/ui/public/config.js` 定义 `window.__METACUBEXD_CONFIG__`（`defaultBackendURL` / `githubToken` / server 形态下还有 `controlToken`），在 `<head>` 同步阻塞加载，`onerror` 有兜底。`[实测: 构建产物 + apps/server/routes/config.js.ts]`
- **反向代理注入 controller 地址**：All-in-One 的替代做法是先填 `DEFAULT_BACKEND_URL`（server 动态 `config.js`；panel 镜像走 `NUXT_PUBLIC_DEFAULT_BACKEND_URL`）。`[上游源码]`
- **`external-ui`**：mihomo 侧支持 `external-ui: /path`、`external-ui-name`、`external-ui-url`（默认 zip 指向 metacubexd 的 gh-pages 归档）`[上游源码: MetaCubeX/mihomo docs/config.yaml L92-95]`。
- **官方镜像是否可拉取**：`[未验证]` —— 本机网络下 Docker Hub / ghcr.io 不可达（按上级 agent 环境更新），容器实验跳过。

### 3.5 License 与活跃度 `[实测]`

- **License：MIT**（`LICENSE`：`MIT License Copyright (c) 2023 MetaCubeX`）。→ 与我们 AGENTS.md 的"外部项目 License 需登记"一致，可安全依赖/搬运？**搬运仍需谨慎**：MIT 允许，但我们的原则是"不修改其核心代码、上游复用"，且 AGENTS.md 禁止未经评审复制外部源码。
- **活跃度**：GitHub API 元数据（本次会话早些时候成功获取）`stars 4340 / forks 520 / open_issues 11 / archived false / created 2023-07-11 / pushed_at 2026-09-10`。
- **最新版本**：`v1.273.1`（release published `2026-09-10`，非 prerelease）；`CHANGELOG.md` 顶部即 `1.273.1`（修复 desktop TUN helper），前几版节奏约每周~每月。
- **CI**：`.github/workflows/{release,e2e,unit-tests,stale}.yml`；release-please 管理版本。
- `[实测]` 本地跑通其单测：`packages/agent` **16 files / 246 tests 全通过**，`packages/config-editor` **11 tests 全通过**（`pnpm --filter ... test`）。测试覆盖面确实很广（supervisor/profiles/http/profile-editor/script/tun/webdav 均有 spec）。

### 3.6 其他值得注意的安全语义 `[上游源码]`

- **Control API 鉴权**：`Bearer <CONTROL_TOKEN>`，SSE 额外接受 `?token=`；`/health`、`/info` 公开；**未配置 token 时 fail closed（503）**（`apps/server/middleware/auth.ts`、`packages/agent/src/http.ts` L100-128）。
- **但 server 会把 `CONTROL_TOKEN` 注入浏览器**：`apps/server/routes/config.js.ts` 把 `controlToken` 写进 `window.__METACUBEXD_CONFIG__`，为的是同源页面无需手输。上游 README 明确警告："that token is **not** access control for the dashboard itself"、"operators must therefore treat dashboard access as token access"。→ **Dashboard 可达 ≈ 内核控制权可达**。
- **`external-controller: 0.0.0.0:<port>`**：server 形态为了发布 9090 端口，把 controller 绑到 0.0.0.0（`apps/server/lib/supervisor.ts` 注释）。这与我们 AGENTS.md 的"Mihomo controller 默认只绑 127.0.0.1 或 unix socket"**直接冲突**。
- **script profile = 执行用户提供的 JS**：`packages/agent/src/script.ts` 明确写"这**不是**硬安全沙箱"（worker_threads 无 require/fetch/fs + 5s 超时，但可触达 Node internals）。关闭方式：不添加 script 类型 profile。
- **Clash API 的 `secret` 基本等同于内核完全控制**：Dashboard 本身就能通过 `PUT /configs`（`useApi.ts: reloadConfigFileAPI`、`fetchRemoteConfigAPI`）热改运行中配置。`secret` 一旦给浏览器，等于把"改配置 + 重启内核"交给浏览器。

---

## 4. 通信与部署方式（含 CORS / secret / 反代）

### 4.1 三种上游运行形态与它们各自解决什么 `[上游文档: README「Deployment」]`

```text
Hosted panel        : 浏览器 → (静态 UI) → 远程 mihomo external-controller     [无 Agent]
Standalone container: 浏览器 → (panel 容器 :80) → 远程 mihomo                   [无 Agent]
Desktop app         : Electron → loopback control server → Agent → 自带内核      [有 Agent]
All-in-One server   : 浏览器 → Nitro(:8080, UI+/api/control) → Agent → 自带内核  [有 Agent]
                    浏览器 → mihomo Clash API(:9090) 直连（不走代理）
```

关键事实：**只有 Agent 形态才有 Profile/内核面板；纯 panel 形态下这些能力会被隐藏**（`pages/control.vue` 的 `hasAgent` 跳转逻辑 + `useControlInfo` 能力位）。

### 4.2 CORS / secret 的三种处理方式

| 方式 | CORS | secret 是否进浏览器 | controller 是否暴露 | 备注 |
| --- | --- | --- | --- | --- |
| 浏览器直连 controller（上游默认） | **需要** `external-controller-cors.allow-origins` 放行 dashboard origin；否则连不上 | **是**（localStorage + 每请求 Bearer） | 需要（至少对浏览器可达） | 需要用户改 mihomo 配置 |
| 我们做 same-origin Clash API 反代（推荐） | **不需要**（同源） | **否**（Agent 侧注入真实 secret） | 否（controller 可只绑 127.0.0.1 / unix socket） | 需要我们自己实现 HTTP + **WebSocket** 透传（nitro 的 proxy 做不到 WS，所以不能抄 nitro 方案） |
| mihomo `external-ui` 托管静态资源 | 同上，仍取决于浏览器如何访问 controller | 是 | 同上 | 只是"谁来 serve 静态文件"，不改变通信语义 |

### 4.3 反代要注意的实现点（来自上游踩坑记录）`[上游源码]`

- **必须支持 WebSocket upgrade**：`apps/server/nitro.config.ts` 的注释（nitro#2886）说明 nitro routeRules proxy 会在 WS 上形成"半个坏端点（HTTP 通、WS 死）"，而 traffic/connections/logs 三个页面全依赖 WS。我们的反代（axum/tower-http `ws` 或 hyper upgrade）必须做完整 upgrade 透传。
- **SSE 的鉴权只能用 query token**：`EventSource` 不能设 header。若我们复用 `/api/control` 形态，需要允许 `?token=`（或干脆不启用 `/api/control`）。
- **路径前缀**：UI 的所有 Clash API 调用走 `ky.create({ prefix: endpoint.url })`，即 endpoint URL 就是"API 根"。所以反代地址形如 `http://agent/clash-api`，UI 端填 `http://agent/clash-api`、secret 填任意占位（因为真实 header 会被 Agent 覆盖）。`[推测]`：需要在反代里**剥离/覆盖**浏览器带来的 `Authorization`，再注入真实 secret。
- **混合内容（HTTPS 页面 → HTTP controller）**：UI 会识别并提示 `mixed_content`（`useApi.ts: checkEndpointAPI`）。反代成 `https://agent/clash-api` 可同时解决。

---

## 5. 同类 Dashboard 对比（zashboard 等）

| 项目 | 定位 | 是否含 Agent / 内核管理 | 部署方式 | License | 活跃度 |
| --- | --- | --- | --- | --- | --- |
| **metacubexd**（MetaCubeX，官方） | 官方 Dashboard + **managed runtime** | **有**（见 §3.3） | 静态 / panel 容器 / all-in-one 容器 / Desktop | MIT | v1.273.1 (2026-09-10) `[实测]` |
| **zashboard**（Zephyruso） | 纯前端 Dashboard（`package.json description: "A Dashboard Using Clash API"`，Vite 构建） | **无**：无 agent / server / supervisor 代码；README 的"升级内核"按钮依赖 mihomo 自身的 `external-ui` 下载路径 | 静态 zip（`dist*.zip`）/ `gh-pages` 分支 / `ghcr.io/zephyruso/zashboard` 容器（仅 UI）/ 官方在线站 | MIT | v3.26.0 (2026-09-07) `[上游源码: CHANGELOG.md / package.json]` |
| Yacd / yacd-meta | 更早期的 Clash Dashboard | 无 | 静态 | MIT | 本次未核实 `[未验证]` |

选择理由（一句话）：**metacubexd 是 Mihomo 官方维护、仍在高频迭代，且它已经把"面板"这一层做得足够好（含多语言/PWA/主题/移动端）——我们没有任何理由重写 Dashboard；而它的 Agent 形态恰好落在我们要占的赛道上，因此我们必须"用它的 UI、绕开它的 Agent"。**

补充事实：zashboard 的 UI 侧也支持"内核升级"按钮与 TUN 开关（`README.md` URL 参数 `disableUpgradeCore` / `disableTunMode`），但那是**依赖 Mihomo 自身能力**（core UI download path、mihomo 的 tun 配置），不是自带 supervisor `[上游文档: zashboard README「Tips」5]`。

---

## 6. 集成方案对比与推荐

### 方案 A：Agent 内置静态资源 + 同源 Clash API 反代（**推荐**）

```text
Browser
  │  GET /ui/*            → Agent 内置 metacubexd 静态产物（hash 路由；资源相对路径）
  │  GET/POST/WS /clash-api/* → Agent ClashApiProxy → 127.0.0.1:9090 或 unix:///run/proxy-agent/mihomo.sock
  │  GET /api/v1/*        → 我们自己的 Web Admin API（生命周期/Config/订阅/Doctor/系统）
  ▼
proxy-agent (Rust)
```

- **安全边界**：真实 `secret` 只存在于 Agent；浏览器永远拿不到。Mihomo controller 不对外暴露，反代即鉴权边界（复用我们 Web 登录态）。
- **CORS**：同源，**完全不需要** `external-controller-cors`，也**不需要** `external-ui`。
- **版本升级**：替换我们打包进二进制的静态产物即可（构建期 pin 一个 metacubexd tag），与 mihomo 版本解耦。
- **离线/内网**：完全离线可用（无 CDN；但注意 `@nuxt/fonts` 会在构建期拉 Google Fonts —— 我本地构建时确实看到了 `fonts.google.com` 拉取失败重试，最终仍成功，产物已内联字体；若做离线可复现构建，建议固定/内置字体源）。
- **成本**：需要自己实现 WS upgrade 反代；需要确认 UI 的 endpoint 校验能接受我们填的占位 secret（`checkEndpointAPI` 会发 `Authorization: Bearer <占位>`，反代覆盖即可）。`[推测]`
- **风险**：上游 UI 若引入新的 `/api/control` 依赖，我们需要跟进或禁用相关入口（capability 探测在无 Agent 时本身就会隐藏）。

### 方案 B：独立 panel 容器 + 反代（不推荐为默认）

```text
Browser → Agent(反代 /ui → panel 容器:80) → 远程 mihomo
```

- **不推荐原因**：(1) 多一个容器依赖，与"单二进制 Agent / Debian+systemd / PVE LXC"的部署哲学冲突；(2) 仍要解决同一套 CORS/secret/WS 问题，但多了一层网络跳数；(3) 静态方案已能完全覆盖 panel 的能力。
- 唯一适用场景：我们希望 Dashboard 独立于 Agent 升级/重启（可用性隔离）。

### 方案 C：官方 All-in-One Server 容器 / iframe 嵌入（**明确不推荐**）

- **All-in-One 容器**：它自己要 `spawn` 并托管内核（`packages/agent/src/supervisor.ts`），而我们的 Agent 也要托管内核（systemd + 版本管理 + 回滚）。**双 supervisor = 状态分裂**，且外部无法用 systemd 管这个内核，违背 AGENTS.md 的进程管理模型。**架构冲突，直接排除。**
- **iframe 嵌入**：需要 `X-Frame-Options`/CSP 可控 + 跨源 cookie/鉴权，安全边界更糊；且我们仍要处理 CORS（父页面 origin ≠ controller）与 secret 暴露。收益为负。
- **`external-ui` 方式**：可作为方案 A 的"零成本替代"（把静态产物丢给 mihomo，由 controller 端口提供 UI），但代价是：UI 与 controller **同源同端口**、controller 需对浏览器可达、且无法叠加我们的 Web 登录态。**适合作为"用户自行部署"的可选配置项，不作为产品默认。**

### 推荐结论

**默认方案 A**，并把"方案 A 的静态产物"设计成一个独立可替换构件（例如 `frontend/metacubexd/` 中只保留我们剪裁过的 `config.js` + 构建产物 + 一个 `UPSTREAM_VERSION` 记录文件），满足 AGENTS.md "不修改其核心代码" 与 "upstream reuse > reimplementation"。

---

## 7. 我们自研 Web Admin 的边界（做什么 / 不做什么）

### 7.1 不做（直接复用 metacubexd）

| 页面 | 复用方式 |
| --- | --- |
| Overview / Proxies / Connections / Rules / Logs / Traffic / Config（Clash API 视图） | metacubexd 静态产物，跳转 `/ui` |
| 代理组选择、节点测速、实时流量图、规则搜索 | 同上 |

### 7.2 必须自研（metacubexd 缺失或与我们的模型冲突）

| 领域 | 为什么必须自研（证据） |
| --- | --- |
| **Mihomo 生命周期（systemd 语义）** | 上游只有 `spawn` 子进程模型，无 systemd unit 管理/`systemctl` 集成（`packages/agent/src/supervisor.ts`） |
| **内核版本管理（Linux 服务端）** | 上游 server 形态**没有** `kernelManager`（`apps/server/lib/supervisor.ts` 未注入），内核是镜像构建期烧入的 v1.19.27；desktop 有但面向桌面 |
| **Config 多版本 / diff / checksum / 回滚历史** | 上游只有单槽 `.bak`（`profiles.ts` L322/L405） |
| **Subscription 生命周期（不可破坏 active config 的更新流程）** | 上游有订阅刷新 + 调度，但"失败保留旧配置 + 校验 + 激活 + 健康检查 + 回滚"这条强不变式只在我们这边定义（AGENTS.md）。上游 `refresh-and-activate` 是 `refresh → safeActivate`，语义比我们要求的弱 |
| **Doctor（Linux/LXC 能力探测）** | 上游无任何 capability detection（无 systemd/TUN 探测，TUN 是 desktop helper + pkexec 模型） |
| **系统状态 / 日志聚合（journald、内核日志、doctor 报告）** | 上游只有 mihomo 子进程 stdout/stderr 的 SSE |
| **多实例 / 多节点** | 上游 = 单 store + 单 supervisor + 单 active.yaml；UI 层可以存多个 endpoint，但没有"一个 Agent 管多个 Mihomo 实例" |
| **nftables / TProxy / 路由 / PVE LXC 网络编排** | 上游无（desktop 的 TUN 是宿主 OS 语义） |
| **Web 鉴权与会话（远程可达场景）** | 上游把 `CONTROL_TOKEN` 直接注入页面，等价于"面板可达即可控内核" |
| **CLI / TUI** | 上游无 |

### 7.3 跳转/嵌入策略

- **跳转（推荐）**：`/admin` 为我们自研管理面（Overview/Mihomo/Subscriptions/Configs/System/Settings/Logs），其中"代理运行态"入口直接跳 `/ui`（metacubexd）。用同一套 Web 登录态做网关。
- **不做 iframe**：理由见 §6 方案 C。

---

## 8. 对 Agent 架构的影响

1. **Port 划分需要显式排除"上游已做"的部分**：`MihomoController` 的 `proxies/connections/traffic` 等观测类方法，如果我们只做反代就不需要 Rust 侧实现（可以直接透传）；真正需要 Rust 实现的是 **生命周期 + 配置版本 + 订阅 + Doctor** 这类"上游没有或语义更弱"的能力。
2. **`MihomoController` 的传输实现应同时支持 HTTP 与 unix socket**（上游 UI 不支持 unix socket，但我们的反代在后端做，因此 unix socket 仍然可用——这反而是我们相对"浏览器直连 controller"的安全优势）。
3. **配置不可破坏不变式需重新审视上游语义**：上游 `setActive` 是"先备份 .bak → 原子写"，`refresh-and-activate` 是"refresh → activate（含 `mihomo -t` 校验）→ 重启"。我们在 AGENTS.md 里要求的"校验→激活→reload→健康检查→失败回滚"比它强，**保持我们的更强不变式，并显式记录差异**（这本身就是不可替代价值的一部分）。
4. **进程管理边界**：如果我们最终在 systemd 下托管 mihomo，则 **绝不能同时启用 metacubexd 的 Agent 形态**（否则双 supervisor）。这是必须在 ADR 里写死的约束。
5. **反代是我们的新构件**：需要一个 `ClashApiProxy`（HTTP + WS upgrade）适配器，位于 interfaces/infrastructure；它是"复用上游 UI"的必要代价，应作为独立 Port 设计。
6. **前端目录策略**：AGENTS.md 的 `frontend/metacubexd/` 建议只放**构建产物 + 我们自己的 `config.js`**，加一个 `UPSTREAM_VERSION` 与构建脚本；不要 fork 其源码（MIT 允许，但会带来长期维护负担）。

---

## 9. 证据与来源

**本地实测（`[实测]`）**

- clone / tarball 工作副本：`/tmp/r07-metacubexd/src/metacubexd-main`（`main`，v1.273.1）；结构核对 `packages/*`、`apps/*`（见 §2.1）。
- `pnpm install --frozen-lockfile` 成功（55.7s）；`pnpm --filter @metacubexd/ui generate` 成功，产物 `packages/ui/.output/public` = **7.6 MB / 126 文件**。
- 用 `python3 -m http.server` 托管该产物：`GET /` → 200（5318 B），`GET /config.js` → 200（内容为 `window.__METACUBEXD_CONFIG__ = {defaultBackendURL:'',githubToken:''}`），深路由 → 404（hash 路由预期行为，SPA 回落由宿主服务器处理）。
- `pnpm --filter @metacubexd/agent test`：**16 files / 246 tests passed**；`pnpm --filter @metacubexd/config-editor test`：**11 tests passed**。
- GitHub API 元数据（会话早期成功获取，之后 api.github.com 限流）：`stars 4340 / forks 520 / license MIT / pushed_at 2026-09-10 / created 2023-07-11`；releases `v1.273.1 (2026-09-10)`。

**上游源码（`[上游源码]`，仓库内相对路径）**

- 结构/自述：`.github/copilot-instructions.md`、`CONTEXT.md`、`README.md`、`packages/ui/PRODUCT.md`、`packages/agent/MANUAL.md`、`pnpm-workspace.yaml`、`package.json`
- 包定义：`packages/ui/package.json`、`packages/agent/package.json`、`packages/config-editor/package.json`、`apps/server/package.json`、`apps/desktop/package.json`
- Agent 能力：`packages/agent/src/{index,http,supervisor,profiles,profile-editor,scheduler,refresh-apply,script,tun,webdav,types}.ts`、`packages/agent/src/kernel/{assets,fetch-kernel,geo}.ts`
- UI 能力：`packages/ui/pages/*.vue`、`packages/ui/components/{KernelControlPanel,KernelVersionPanel,KernelLogView,SystemProxyControlPanel}.vue`、`packages/ui/composables/{useApi,useControlApi,useConnect,useKernelVersions}.ts`、`packages/ui/stores/{endpoint,config}.ts`、`packages/ui/middleware/auth.global.ts`、`packages/ui/utils/index.ts`、`packages/ui/nuxt.config.ts`、`packages/ui/public/config.js`
- 部署：`packages/ui/Dockerfile`、`packages/ui/docker-entrypoint.sh`、`apps/server/Dockerfile`、`apps/server/docker-entrypoint.sh`、`apps/server/compose.yaml`、`apps/server/nitro.config.ts`、`apps/server/routes/{config.js.ts,api/control/[...].ts}`、`apps/server/middleware/auth.ts`、`apps/server/lib/supervisor.ts`、`.github/workflows/release.yml`、`.dockerignore`、`LICENSE`
- 桌面：`apps/desktop/src/main/{kernel-manager,supervisor 相关, tun-*, sysproxy*, helper/*}.ts`

**上游文档 / 外部链接（`[上游文档]`）**

- metacubexd README：https://github.com/MetaCubeX/metacubexd
- metacubexd gh-pages（静态产物）：https://github.com/MetaCubeX/metacubexd/tree/gh-pages ；主页 https://d.metacubex.one
- mihomo `config.yaml` 模板（`external-ui` / `external-controller-cors` / unix socket）：https://raw.githubusercontent.com/MetaCubeX/mihomo/Alpha/docs/config.yaml
- zashboard：https://github.com/Zephyruso/zashboard （README / package.json / CHANGELOG，经 `raw.githubusercontent.com` 获取）
- nitro WebSocket proxy 限制：nitrojs/nitro#2886（上游 `nitro.config.ts` 注释引用）

---

## 10. 未验证假设与开放问题

**未验证（本次环境限制或未做实验）**

1. `[未验证]` **容器实验全部跳过**：本机网络下 Docker Hub / ghcr.io 不可达（上级 agent 环境说明），因此 `ghcr.io/metacubex/metacubexd` 与 `ghcr.io/metacubex/metacubexd-server` 的**实际镜像内容、启动行为、健康检查**均未实测。
2. `[未验证]` **未做端到端联调**：没有启动真实 mihomo + 浏览器实际操作 Dashboard（无外部 mihomo 二进制、未跑 MANUAL.md 的 smoke 步骤）。所有"页面能做什么"来自源码与官方文档，非点击验证。
3. `[未验证]` **反代可行性未验证**：UI 是否能接受"占位 secret + 反代覆盖 Authorization"、`ky` 在 `prefix` 为 `/clash-api` 相对地址时的行为、WS upgrade 透传在 axum 侧的具体写法——全部为设计推断。
4. `[未验证]` **`NUXT_APP_BASE_URL=/ui` 的构建产物**：我验证了 `gh-pages` 分支的产物使用相对路径（`./_nuxt/...`），但未亲自用 `NUXT_APP_BASE_URL=/ui` 重新构建确认子路径挂载；`[推测]` 可行。
5. `[未验证]` **yacd / yacd-meta 等其它 Dashboard** 未对比（用户只要求核实 zashboard）。
6. `[未验证]` zashboard 的 stars/license 页面元数据：GitHub HTML 经 `ghfast.top` / `ghproxy.net` 均返回 **403**，api.github.com 限流；其 license(MIT)/版本(3.26.0)/构建方式(Vite) 均来自 `raw.githubusercontent.com` 上的 `LICENSE`、`package.json`、`CHANGELOG.md`。
7. `[未验证]` metacubexd 是否存在官方 `dist.zip`（我的 `releases/latest/download/dist.zip` 探测返回 404；zashboard 明确提供 dist.zip）。**结论：metacubexd 的静态分发渠道是 gh-pages 分支 + 官方容器**，`[推测]` gh-pages 归档 zip 可用于 `external-ui-url`。

**开放问题（建议进 `docs/research/open-questions.md`）**

1. 我们是否要把 metacubexd 静态产物**打进二进制**（离线可靠、版本可复现）还是**运行时下载/挂载**？二者对离线安装包大小与升级方式的影响不同。
2. Clash API 反代是否要做"只读透传 + 写操作需二次确认"的能力裁剪（`PUT /configs`、`DELETE /connections`）？——注意裁剪会影响 metacubexd 功能完整性。
3. 是否允许用户在"我们有 systemd 托管"的前提下，**自行选择**使用上游 All-in-One 容器（即不安装我们的 Agent）？如果允许，需要在文档里明确这是替代路径而非集成路径。
4. 我们的 Web 登录态与 Clash API 反代的授权模型：是否直接复用 session cookie？是否需要为反代单独定义 scope？
5. metacubexd 版本跟随策略：固定 pin（推荐）还是允许用户配置 tag？如何回归验证？
6. `external-ui` 作为"零依赖"备选路径，是否作为 `proxyctl doctor` 的一个建议项输出？

---

### 附：与 AGENTS.md / 设计文档的冲突点（需回写）

| 冲突 | 我们的规则 | metacubexd 现状 | 处理建议 |
| --- | --- | --- | --- |
| Mihomo controller 绑定 | 默认 `127.0.0.1` 或 unix socket，不暴露公网 | All-in-One server 绑 `0.0.0.0:<port>` 并把 secret 提供给浏览器 | **不采用其 Agent 形态**；采用方案 A 的反代，让 controller 保持 loopback/unix |
| 进程管理单一权威 | systemd/adapter 管理内核 | 上游 `spawn()` 自管内核 | 禁止同时启用上游 supervisor；ADR 写死 |
| 配置不可破坏 | 校验→激活→reload→健康检查→失败回滚 | 单槽 `.bak` 回滚 + `mihomo -t` 校验 | 保留我们更强的流程，并把它列为不可替代价值 |
| 设计文档 §29 描述 | "第一阶段：`http://agent/dashboard` 直接提供静态文件" | 静态托管可行（已验证产物），但**必须解决 Clash API 同源/WS 反代**，否则用户仍需自配 CORS | 更新 §29 为"静态托管 + 同源 Clash API 反代" |
| 设计文档 §47 前端目录 | `frontend/metacubexd/` 不修改其核心代码 | 上游为 pnpm monorepo，产出在 `packages/ui/.output/public` | 目录中放**构建产物 + config.js + UPSTREAM_VERSION**，不 vendor 源码 |
