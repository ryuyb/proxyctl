# 内嵌 metacubexd 仪表盘 — 设计方案

> 状态：**已实现**（2026-09-13） | 日期：2026-09-13
> 关联：ADR-006 D1/C4/C5、R07（`docs/research/07-metacubexd.md`）、R13 §3.4、Q016（已关闭）
> 前置结论：**Q016 已解决** —— 用户已取得 Highcharts 商业授权，因此可以内嵌上游构建产物。
>
> **实现已完成并在真实环境验证**（Debian 12 + 真实 mihomo v1.19.30 + 真实浏览器）。
> 本文档保留为设计记录；下方标 ✅ 的条目是实测确认的，标 ⚠️ 的是**实测推翻了初稿假设**的。
> 实现产出的回归测试见 `crates/interfaces/src/http/upgrade.rs` 与
> `crates/infrastructure/src/mihomo/clash_relay.rs`。

---

## 1. 本次实测确认的事实

以下不是推测，是在本机跑通上游构建后确认的：

| 事实 | 证据 |
|---|---|
| 上游可检出并构建 | `main` 分支 tarball → `pnpm install --frozen-lockfile` → `pnpm generate` 成功 |
| 产物规模 | `.output/public` = **9.3 MB / 154 文件**（含 `_nuxt/`、`_fonts/`、PWA 图标） |
| 产物是纯静态 | `ssr: false`，`HTML content not prerendered`，可丢给任意静态托管 |
| 资源是相对路径 | `NUXT_APP_BASE_URL=./` → `index.html` 里 `./_nuxt/...`，`app.baseURL: "./"` → **子路径托管可行** |
| 路由是 hash 模式 | `router.options.hashMode: true` → SPA 内部路由不需要服务器 fallback |
| Agent 形态是**探测**出来的 | `useControlInfo.ts` 探 `/api/control/info`，**`catch` 里即降级为普通 panel 模式**（`hasAgent = false`）→ **不需要改上游源码就能隐藏 Profile/内核控制页** |
| `/api/control` 的 base 取自 **origin**，不是相对路径 | `useControlApi.ts`: `` `${stripTrailingSlash(origin)}/api/control` `` → 我们只要在**根** `/api/control/info` 返回 404 即可，不必在 `/ui/` 下再挂一层 |
| 后端地址可从 `config.js` 注入 | `config.js` 定义 `window.__METACUBEXD_CONFIG__`，在 `<head>` **同步**加载；`useConnect.ts` 的优先级是 `runtimeConfig` > `config.js` > `FALLBACK_BACKEND_URL('http://127.0.0.1:9090')` |
| 存在 PWA service worker | `sw.js` precache 118 项 / 6.1 MB。内嵌场景下它会缓存 `/ui/*`，与「换版本即刷新」冲突 → 建议关闭 |
| 构建期访问 Google Fonts | `@nuxt/fonts` 会拉 `fonts.gstatic.com`，产物把字体内联进 `_fonts/`。**构建需要网络**，运行不需要 |

---

## 2. 关键设计决策

### D1. 产物如何进入仓库 — **不提交 `dist`，用可复现脚本拉取**

三个选项：

| 方案 | 评价 |
|---|---|
| A. 把 9.3 MB 产物提交进 git | ❌ 154 个带 hash 的文件进版本库，每次升级产生巨大 diff，且无法审计来源 |
| B. 构建时 `git clone` 上游并构建 | ❌ 需要 Nuxt/pnpm 工具链，把 Node 变成 Rust 构建的硬依赖；上游构建还要访问 Google Fonts，离线/隔离环境直接失败 |
| **C. 脚本拉取上游 `gh-pages` 发布产物 + 校验 + 解包到 `frontend/metacubexd/dist/`（推荐）** | ✅ 上游**已经**为静态发布维护 `gh-pages` 分支（R07 §3.4，`.github/workflows/release.yml` 用 `peaceiris/actions-gh-pages`）。我们消费官方发布物而不是自己构建，符合 AGENTS.md「upstream reuse > reimplementation」 |

选 C。理由与边界：

- **消费官方发布产物**，不自己构建 → 不需要 Node 工具链参与 Rust 构建，也不需要为了内嵌而 fork 上游。
- **构建产物不进 git**，与 `frontend/admin/dist` 的 `.gitignore` 一致。
- 脚本记录**上游版本号**到 `frontend/metacubexd/UPSTREAM_VERSION`，并校验 `_nuxt/` 里的 `appVersion` 与之一致 —— 否则「我内嵌的是哪个版本」无法回答。
- **离线兜底**：拉取失败不阻断构建，只警告并降级为「未部署」占位页（与 admin 的 placeholder 机制一致）。这是刻意的：一个没有网络的 `cargo build` 不应该失败。

⚠️ 需要确认的一点：`gh-pages` 分支的产物是 `nuxt generate` 的默认配置（`NUXT_APP_BASE_URL` 未设 → baseURL `/`），而我们要子路径托管。**二者不冲突**，因为：产物用的是相对路径（实测 `./_nuxt/...`），而 `app.baseURL` 只影响路由前缀 —— hash 模式下路由本就不进 URL path。**但这一条必须在实现时用真实产物验证**，不能只靠推断。

### D2. 访问路径 — `/ui` 还是根路径

- **`/ui/`（推荐）**：`/` 已经被我们的 admin 占用作为 fallback。`/ui` 前缀让「我们的界面」与「上游面板」边界清晰，也让 `Content-Security-Policy` 可以分开设置。
- 风险：绝对路径的 `config.js` 引用 —— 实测 `index.html` 里是 `<script src="config.js">`（相对），安全。
- **必须验证**：上游产物里是否存在任何以 `/` 开头的绝对资源引用。实现时用 `grep -o 'src="[^"]*"' | grep '^src="/'` 扫一遍。

### D3. 凭证 — **浏览器永远拿不到 mihomo secret**

按用户选择：复用我方登录态。

```text
浏览器 → /ui/*                静态产崧（我方提供，带 session 校验）
浏览器 → /clash-api/*         ClashApiProxy（我方反代 → mihomo controller）
浏览器 → /api/control/info    404（强制上游进「纯 panel」模式）
```

- 反代**剥离**浏览器带来的 `Authorization`，**注入**真实 mihomo secret。所以 UI 里填的 secret 是占位符，填什么都行。
- 反代要求 **ADMIN 角色**（按用户选择）。这比 admin 页面更严格是合理的：`/clash-api` 的 `PUT /configs` 能热改运行中配置，等于内核完全控制权（R07 §3.6）。**只读会话必须被拒绝**，否则等于把只读提升为内核管理员。
- mihomo controller 保持 `127.0.0.1` 或 unix socket，不对外暴露（R07 §4.2）。

### D4. WebSocket — **必须做真正的 upgrade 透传**（✅ 已 spike 验证）

这是上游踩过的坑（`apps/server/nitro.config.ts` 注释：nitro #2886，HTTP 通但 WS 死）。metacubexd 的 traffic / connections / logs 三个页面**全依赖 WS**。

**已实测通过**，不再是风险。spike 代码在 `/tmp/wsspike`（临时，结论记录于此）：

| 验证项 | 结果 |
|---|---|
| hyper 1.11 + `with_upgrades()` 双侧升级透传 | ✅ `SPIKE_OK: origin-saw:hello` |
| **unix socket 上游** + 升级透传 | ✅ `SPIKE_OK`（这正是我们默认的 mihomo 控制通道） |
| 帧是否被解析 | 否 —— 用 `copy_bidirectional` 原样搬运字节 |

**实现时必须改的第一件事**：`crates/interfaces/src/http/server.rs` 的两处
`serve_connection(...)` 都**没有**调用 `.with_upgrades()`，因此当前服务器**无法完成任何升级**。
不加它，反代会返回 101 但连接立刻断 —— 即上游踩过的「HTTP 通、WS 死」。

另外 `hyper::upgrade::on` 只接受 `Request`/`Response`（不是 `Parts`），所以转发时
必须保留整个 `Request`，不能先 `into_parts()`。这一点 spike 里踩到过。

需要新增的依赖：`hyper` 的 `client` feature + `hyper-util` 的 `client`/`client-legacy`
（当前 workspace 只开了 `server`/`http1`）。仍在 MIT/Apache-2.0 范围内。

### D5. CSRF — 反代是 cookie 认证，所以要过 origin 检查

`/clash-api` 复用 session cookie 认证，因此它是**写操作**路径（`PUT /configs`、`POST` 等），必须纳入现有的 CSRF 检查（`auth.rs` 的 `requires_csrf_check` + `is_same_origin`）。上游 UI 用 `ky` 发同源请求，会带 `Origin`，能通过。

但 **WebSocket upgrade 是 GET**，现有 `requires_csrf_check` 对 GET 返回 `false` —— 这符合预期（WS 握手不改变状态），但它携带 cookie，因此仍需验证浏览器会带 `Origin`/`Sec-Fetch-Site`，并在服务端检查。

### D6. CSP — 上游的 CSP 需求与我们不同

我们的 admin 用了严格 CSP（`script-src 'self'`）。metacubexd 是 Nuxt CSR，`index.html` 里有**内联脚本**（`window.__METACUBEXD_CONFIG__` 兜底、`window.__NUXT__` 数据）。因此 `/ui/*` 需要 `script-src 'self' 'unsafe-inline'`。

- 这是**必须接受**的降级，且只作用于 `/ui/*`，不影响 admin 的严格 CSP。
- 由于 `index.html` 是我们自己 serve 的（不是上游文件原样透传），**CSP 应该由我们在响应头里加**，而不是写在上游的 `index.html` meta 里 —— 这样上游升级不会覆盖它。

---

## 3. 组件设计

### 3.1 静态资源

复用现有 `assets` 机制的模式，但**独立一份**，因为两者的 fallback 语义不同：

| | admin | metacubexd |
|---|---|---|
| 路由模式 | history（需要 fallback 到 index） | **hash**（不需要 fallback） |
| CSP | 严格 | 需要 `unsafe-inline` |
| 缺失时 | placeholder 页 | 「未部署」提示页 |

`crates/interfaces/build.rs` 扩展为同时扫两个目录，产出两个 bundle 常量。若只扫到其中一个，另一个走占位。

### 3.2 Clash API 反代

新增 `crates/interfaces/src/http/routes/clash_api.rs`，挂在 `/clash-api/{*path}`：

1. `Caller` 提取器（已有）→ 校验 session/token，**要求 ADMIN**
2. 剥离 `Authorization`、`Cookie`、`Host` 等 hop-by-hop / 认证头
3. 注入 mihomo secret（`Authorization: Bearer <secret>`）
4. 对 mihomo controller 发起请求：**unix socket 优先**，其次 `host:port`
5. 响应原样回传（含 WS upgrade）

端口/套接字来源：`MihomoController` 的既有配置。需要一个 Port 或直接复用 infrastructure 里已有的 controller 客户端配置读取。

---

## 4. 落地顺序（每步可独立验证）

> **全部完成**（2026-09-13）。实际执行顺序与初稿略有不同：WS 升级（原 ①）留到最后，
> 因为 HTTP 路径能先独立验证，且升级一旦接上就依赖前面的反代结构。

| # | 步骤 | 验证方式 |
|---|---|---|
| ① ✅ | **WS 反代 spike** | 用 curl/websocat 对 `/clash-api/` 打通 `Upgrade`，确认帧双向流动。**这是 go/no-go** |
| ② ✅ | 拉取脚本 + 版本记录 | 断网跑一次确认降级；联网跑一次确认产物结构 |
| ③ ✅ | `build.rs` 双 bundle | `cargo build` 后检查生成的 bundle 常量含 154 个条目 |
| ④ ✅ | `/ui` 静态路由 + CSP | curl 验证 `index.html`、`_nuxt/*`、缺失资源 404 |
| ⑤ ✅ | `/api/control/info` → 404 | curl 验证，并在浏览器确认 Profile/控制页消失 |
| ⑥ ✅ | `/clash-api` HTTP 反代 + 按方法分流 | curl 用只读 token 确认 403；admin token 确认能拿到 `/version` |
| ⑦ ✅ | 浏览器端到端 | Playwright：填占位 secret 能连上，overview/traffic/logs 三页有数据 |
| ⑧ ✅ | 文档 + ADR | 更新 ADR-006、Q016 关闭、R13 补 Highcharts 授权结论 |

---

## 5. 实现记录：设计推错了什么

这一节比上面的设计更有价值——以下每一条都是**初稿的假设被实测推翻**，且都在真实环境中
付出了调试代价。写下来是为了不让下一个人重犯，也因为其中两条在原始代码注释里是**写反的**。

| # | 初稿假设 | 实测结果 | 代价 |
|---|---|---|---|
| ⚠️ 1 | 「转发浏览器的 `sec-websocket-key` 是可选优化——key 只要是合法 nonce 就行」 | **错。** 内核用收到的 key 派生 `accept`，浏览器拿**自己发的** key 去校验。换掉 key 必然失败于 `Incorrect 'Sec-WebSocket-Accept' header value` | 所有 WS 页面不可用。**注释里写对了原理、实现写反了** |
| ⚠️ 2 | 「`connection` 是 hop-by-hop，逐跳剥离」 | **只在请求方向成立。** `101` 响应必须以 `Connection: Upgrade` 才算合法，剥掉后浏览器报 `'Connection' header value must be 'Upgrade'` | 所有 WS 页面不可用 |
| ⚠️ 3 | 「`defaultBackendURL` 用相对路径 `/clash-api` 即可，会相对于 origin 解析」 | **错。** 上游对无 scheme 的值拼 `location.protocol`，`/clash-api` → `http:///clash-api`，它把 `clash-api` 当成了**主机名** | 仪表盘默认后端指向不存在的 host |
| ⚠️ 4 | 「响应头拿到后即可 abort 连接驱动」 | **错。** 响应**头**先到、body 后到，提前 abort 会关掉还没读完 body 的 socket | 所有请求失败于 `connection closed before message completed` |
| ⚠️ 5 | 「`{*path}` 通配符会匹配 `/ui/`」 | **错。** axum 的 wildcard 要求至少一个字符，`/ui/` 空尾不匹配，落到单页 fallback 后返回**admin 界面** | 正确的 URL 出现错误的界面 |
| ⚠️ 6 | 「可以不转发 `Host`，内核按 socket 寻址不需要它」 | **错。** HTTP/1.1 要求 `Host`；缺失报 `400 missing required Host header`，转发调用方的又会产生**两个 Host 头**得到无说明的 `400` | 所有请求 400 |

### 一个关于测试的教训

第 4 条的回归测试**第一版写错了、且不会失败**：测试服务器把响应头和 body 一次
`write_all` 发出，socket 缓冲区里已经没有「在途」数据可丢，于是有 bug 也通过。
改成分两次发送（中间停顿）后才真正复现。

第 1 条同样如此：只断言「返回了 101」的测试会通过，因为 bug 在**发给内核的字节**里。
现在断言的是内核实际收到的请求。

**原则**：这一层的测试必须断言**对端收到了什么**，而不是「我们没报错」——relay 的错误
全部发生在两个 socket 之间，任何只看自己返回值的测试都是无效的。

## 6. 验证记录（实测）

环境：Debian 12 (aarch64) + 真实 mihomo `v1.19.30` (linux/arm64) + Chromium（Playwright）。

```text
静态服务
  /ui            → 200  5359 B  <title>MetaCubeXD</title>
  /ui/           → 200  5359 B  （修复前返回 1547 B 的 admin 界面）
  /ui/_nuxt/*.js → 200  489426 B
  /ui/sw.js      → 200  350 B   （中性化，非上游的 6116 KiB）
  /ui/config.js  → 200  defaultBackendURL: location.origin + '/clash-api'

控制面
  /api/control/info → 404  {"hasAgent":false}  → 上游隐藏 Profile/内核控制页

反代（session → agent → unix socket → 内核）
  GET  /clash-api/version  → 200  {"meta":true,"version":"v1.19.30"}
  GET  /clash-api/configs  → 200  完整运行时配置
  GET  /clash-api/proxies  → 200  节点列表

WebSocket（真实浏览器，零错误）
  OPEN /clash-api/connections
  OPEN /clash-api/traffic
  OPEN /clash-api/memory
  OPEN /clash-api/logs

权限分流
  admin:     GET → 200, WS → 101, PUT/POST → 放行
  read-only: GET → 200, WS → 101（收到真实流量帧）, PUT/POST → 403
```

浏览器逐页确认数据来自内核：代理组 `GLOBAL | 2/2`、配置页显示内核版本 `v1.19.30`、
规则页 `Match → DIRECT`。

## 7. 顺带发现：内核的 unix 控制口用法

实测 `mihomo v1.19.30`：`external-controller: <路径>` **不生效**，内核把它当 `host:port`
解析并报 `missing port in address`，而且**只打日志、不退出**——得到一个「内核在跑、
控制口不存在」的静默故障。正确键是 **`external-controller-unix`**。

已核实我们的生成器写的是后者（`crates/domain/src/configuration/generation.rs`），
且有测试断言它**不**写 `external-controller:`。剩余缺口见 Q027。

## 8. 原设计待确认项 —— 均已有结论

- **产物来源**：采用「拉上游 `gh-pages` 发布物」，理由是零 Node 构建依赖、不 fork 上游。
- **`/clash-api` 权限粒度**：采用**按 HTTP 方法分流**，理由是上游全部写操作都是
  `PUT`/`POST` 且无 `GET` 改状态，实现后已在真实环境验证两种角色。
- **WS 升级退路**：无需退路——hyper 的 `with_upgrades()` 方案在真实内核上验证通过。

1. **产物来源**：接受「拉上游 gh-pages 发布物」而不是「我们自己构建」吗？
   这会让我们依赖上游的发布节奏，但换来零 Node 构建依赖、不 fork 上游。
   若你更希望**自己构建**（可控性更高、但 Rust 构建要依赖 Node 且构建期需要访问 Google Fonts），
   请指出，我改成构建脚本 + 本地 pin 版本号。

2. **`/clash-api` 的权限粒度**：三选一。

   **(a) 全路径要求 ADMIN** —— 最简单，只读会话完全看不到 Dashboard。
   **(b) 按 HTTP 方法分流（推荐）** —— `GET` + WS 放行给只读，`PUT`/`POST`/`PATCH`/`DELETE` 要求 ADMIN。

   我核对了上游 UI 的**全部** Clash API 调用（`packages/ui/composables/useApi.ts`），
   方法语义是干净的：

   ```text
   读（GET / WS）  version, configs, proxies, providers, rules, connections, traffic, logs, memory
   写（PUT）       proxies/:name（选组）、providers/proxies/:name（测速）、configs（热改配置）
   写（POST）      cache/fakeip/flush, cache/dns/flush, configs/geo, upgrade, upgrade/ui, restart
   ```

   **不存在「GET 却改变状态」的端点**，所以按方法分流在语义上是可靠的。
   代价：只读会话仍能看到连接列表里的**目标主机与规则**——
   但 mihomo 的 `/connections` 不回传进程信息（那是我们 admin API 才有的），
   所以泄露面比我们自己的 connections 页更小。

   **(c) 干脆不给只读会话** —— 与 (a) 同义，但明确写进文档。

   我倾向 **(b)**：它与我们 admin 界面的既定边界一致（只读能看状态、不能操作），
   且实现成本只是判断 `method` 是否在只读白名单里。
