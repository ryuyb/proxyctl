# R05 — Sub-Store 部署方案

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：`[实测]` + `[上游源码]` + `[上游文档]`
> 关键结论一句话：**MVP 不内嵌、不托管 Sub-Store：把官方 Sub-Store 当作"可选外部 Converter"，默认不安装；用户用 Docker（官方镜像 `xream/sub-store`，推荐）或 Node 直跑（官方 Release 资产 `sub-store.bundle.js`，零 npm 依赖）自行部署，Agent 通过可配置 `base_url` + 用户自选前置认证接入；未配置时订阅功能降级为 Native 解析，且绝不因 Sub-Store 不可用而影响 Mihomo 生命周期。**

---

## 1. 结论摘要（TL;DR）

1. `[上游文档]` **官方唯一"官方自建"路径是 Docker**：镜像名精确为 **`xream/sub-store`**（Docker Hub），官方安装文档给出的形态是"前端 `3001` + 后端 `3000`"双端口，数据卷挂载到容器内 **`/opt/app/data`**。Node/Bun 不属于官方"部署方式"（Node 只是构建与运行 backend 的底层运行时），Vercel 仅托管**前端**，Cloudflare Workers/Pages **不是** Sub-Store 后端的部署目标（backend 是 Express + Node 内建模块，无法跑在 Workers runtime 上）。
2. `[实测]` **Node 直跑可行且极轻**：官方 Release 资产 `sub-store.bundle.js`（2.39.6，**3.0 MiB** 自包含 bundle，`runtime-manifest.json` 声明 `"npm": []`）在**完全没有 `node_modules`** 的情况下正常启动并返回 `/api/utils/env`。冷启动到首次成功请求 **130 ms**，空闲内存 **60 MB**（macOS `footprint`）。
3. `[实测]` **Sub-Store 后端 API 默认无任何认证**：`GET /api/subs` 与 `POST /api/subs` 均未带任何凭据即返回 `200/201`，且写入已落盘到 `sub-store.json`。**必须只监听 `127.0.0.1` + 前置认证**。
4. `[实测]` **源码默认监听地址是 `::`（所有接口），与官方文档写的 `127.0.0.1` 不一致**。不显式设置 `SUB_STORE_BACKEND_API_HOST` 时日志实测为 `[BACKEND] listening on :::3032`。这是本项目 doctor 必须主动检查/纠正的一条。
5. `[实测]` **数据目录不存在则启动即崩溃**：`SUB_STORE_DATA_BASE_PATH` 指向的目录必须先 `mkdir -p`，否则 `initCache()` 抛 `ENOENT ... root.json` 且进程退出。挂载 volume / 建 systemd 单元时都要注意。
6. `[实测]` **持久化是"纯 JSON 文件，无数据库、无内置加密挂载"**：数据目录内生成 `root.json`（缓存）与 `sub-store.json`（订阅/组合/文件/Token/settings，含订阅 URL 凭据），默认跟随 `SUB_STORE_DATA_BASE_PATH`，**未设置时回退到当前工作目录 `.`**（实测确认）。没有 `db.json`/`store.json` 这种文件名。
7. `[实测]` **基础启动完全离线**：无 MMDB 配置时启动不发起任何外部下载（仅首次 `pnpm i` / `docker pull` 需要外网）。`shoutrrr`（推送）是可选外部二进制，不涉及主流程。
8. `[上游源码]` **版本/升级有明确机制**：上游每次改动 `backend/package.json` 即自动打 tag 并发 Release（资产含 `sub-store.bundle.js` + `runtime-manifest.json`）；`runtime-manifest.json` 内的 `"testedNode": "24.15.0"` 是官方认可的目标 Node 版本，官方 SubDock 正是**读取该字段下载对应 Node**。建议 Agent 只做**版本检测 + 提示**，不自动升级。

---

## 2. 官方支持的部署方式与要求

> 证据基准：官方文档仓库 `sub-store-org/doc`（`guide/installation.md`、`guide/update.md`、`reference/environment-variables.md`）+ 后端仓库 `sub-store-org/Sub-Store@master`（commit 对应版本 `2.39.6`，2026-09-11）。

### 2.1 官方安装方式总表 `[上游文档]`

官方 `guide/installation.md` 的"安装"表逐项如下（原文要点摘录）：

| 方式 | 官方描述 | 备注（原文） |
| --- | --- | --- |
| 软件内置 | Clash Party / Sparkle 等客户端内置 Sub-Store | 大多无法直接使用高级功能 |
| 代理 App 模块 | Surge / Shadowrocket / Loon / QX 等 | 可以变通使用部分高级功能 |
| Android 模块 | `Delusions6515/Sub-Store-Module` | **需要 root**，可直接使用高级功能 |
| Android APP | `sionnx/SubCase` | 无法直接使用高级功能 |
| Android Termux | 社区教程（Telegram） | — |
| **Docker 自建** | **官方镜像 `xream/sub-store`** | 若要使用高级功能，请参考 Docker Hub 页面说明 |
| 官方前端 | `https://sub-store.vercel.app` | **仅前端**，需搭配自定义后端使用 |

**关键否定结论**（对本项目很重要，避免把"前端托管平台"误当"后端部署方式"）：

- `[上游文档]` **Vercel 只是官方前端的托管地**，官方明确写 "仅前端，需搭配自定义后端使用"。Sub-Store 后端**不能**部署到 Vercel。
- `[上游文档]` 官方文档中**没有** Cloudflare Workers / Pages / Synology 的部署指引。`[上游源码]` 后端入口 `backend/src/main.js` 直接 `import serve from '@/restful'`，`backend/src/vendor/express.js` 用 Node `app.listen(port, host)` + `node:fs` / `node:http` 等服务端能力；`runtime-manifest.json` 声明 builtins 含 `fs`、`http`、`net`、`tls`、`child_process`、`worker_threads`。这些在 Workers runtime 上不可用，因此**Cloudflare Workers/Pages 部署后端不可行**（`[上游源码]` 推断，非官方明文否定）。
- `[上游文档]` **Bun 没有被官方列为部署方式**。build workflow 用 `actions/setup-node` + `node-version-file: .node-version` + `pnpm`（`[上游源码]` `.github/workflows/main.yml`），SubDock 也是下载官方 Node 二进制。Bun 兼容性 `[未验证]`，不建议列入 MVP 支持面。

### 2.2 Docker（官方推荐）`[上游文档]`

官方 `guide/installation.md` 的快速启动示例（逐字）：

```bash
docker run -it -d --restart=always \
 -e "SUB_STORE_FRONTEND_BACKEND_PATH=/2cXaAxRGfddmGz2yx1wA" \
 -e "SUB_STORE_CORS_ALLOWED_ORIGINS=https://sub-store-frontend.a.com" \
 -p 127.0.0.1:3001:3001 \
 -v /root/sub-store-data:/opt/app/data \
 --name sub-store \
 xream/sub-store
```

官方原文要点：

- 前端默认监听 `3001`，**后端 API 默认 `3000`**，并特别注明"**尽量只监听本机**"。
- `SUB_STORE_BACKEND_MERGE=true` 可合并前后端端口，"仅暴露一个端口"。
- **数据目录挂载到 `/opt/app/data`，升级容器不会丢数据。**
- 需要测活等脚本时，可选用带 **`http-meta` tag** 的镜像（同一 `xream/sub-store` 仓库的不同 tag）。
- 更新方式：`docker pull xream/sub-store` 后重建容器，或用 Watchtower 自动更新。
- 启动后访问 `http://127.0.0.1:3001`；后端地址形如 `http://127.0.0.1:3001/2cXaAxRGfddmGz2yx1wA`（前缀由 `SUB_STORE_FRONTEND_BACKEND_PATH` 决定）。

`[实测]` 本机 Docker（OrbStack，Server 29.4.0）daemon 可用，但 **Docker Hub 与各镜像源均不可达**，因此 `docker pull xream/sub-store` 无法执行，**镜像体积、容器内默认监听、容器内存占用均未能实测**（见 §11）。

`[上游文档]` http-meta tag 的命名形态：官方文档只写"带 `http-meta` tag"，未列出完整 tag 规则；`[上游文档]` 第三方的 Docker Hub 镜像页（如 `manlongdan/xream_sub-store`）可见 `2.24.9-http-meta`、`2.24.15-http-meta` 形式，`[推测]` 官方规则为 `<版本号>-http-meta`，落地前需在 Docker Hub 页面确认。

### 2.3 Node 直跑 `[实测]` + `[上游源码]`

官方 README 的 "Build" 段落只写构建产物：

```
pnpm bundle:esbuild
```

`[上游源码]` `backend/bundle-esbuild.js` 的产物清单（本机实测复现）：

| 产物 | 用途 | 本机实测大小 |
| --- | --- | --- |
| `backend/sub-store.min.js` | 浏览器 IIFE（代理 App 模块用） | 1,372,438 B |
| `backend/dist/sub-store.bundle.js` | **Node CJS bundle（服务器运行入口）** | 3,096,828 B |
| `backend/dist/runtime-manifest.json` | 运行时能力清单（builtins / npm / testedNode） | 712 B |
| `backend/dist/sub-store-0.min.js` / `sub-store-1.min.js` | 代理 App 模块 | 1.33 / 1.31 MB |
| `backend/dist/sub-store-parser.loon.min.js` | Loon 资源解析器 | 1,269,150 B |
| `backend/dist/cron-sync-artifacts.min.js` | 定时任务 | 1,276,423 B |
| `backend/dist/proxy-utils.esm.mjs` | ESM 库 | 1,266,979 B |

运行方式（`[实测]` 已验证，等价于官方 Release 资产用法）：

```bash
SUB_STORE_DATA_BASE_PATH=/var/lib/sub-store \
SUB_STORE_BACKEND_API_PORT=3000 \
SUB_STORE_BACKEND_API_HOST=127.0.0.1 \
node dist/sub-store.bundle.js
```

官方 Release 直接提供该 bundle，**无需构建、无需 `node_modules`**：`https://github.com/sub-store-org/Sub-Store/releases/latest/download/sub-store.bundle.js`（`[实测]` 经镜像站返回 `content-length: 3096828`，与本地构建产物同量级）。官方 SubDock 的 `tool/prepare_backend.sh` 也正是下载这两个资产并安装到自身目录。

开发模式（`[上游文档]` README "Development"）：`pnpm i` 后 `SUB_STORE_BACKEND_API_PORT=3000 pnpm esbuild:dev`。

### 2.4 官方 Node 版本要求 `[上游源码]`

| 证据 | 内容 |
| --- | --- |
| 仓库根 `.node-version` | `24.15.0` |
| `runtime-manifest.json` → `testedNode`（本机构建产物与官方 Release 资产一致） | `24.15.0` |
| CI `.github/workflows/main.yml` | `actions/setup-node` + `node-version-file: ".node-version"` |
| SubDock `tool/prepare_runtime.sh` | 无显式版本时从 `runtime-manifest.json` 读 `testedNode`，再下 `node-v<该版本>-linux-x64.tar.xz` |
| `backend/package.json` | **没有 `engines` 字段**（`[实测]` grep 无结果），即安装期不做版本强制 |

结论：**官方认可的目标 Node 是 `24.x`（精确 `24.15.0`）；实际上游只在 24.15.0 上测过。** 本机用 Node `v25.2.1` 实测通过。最低版本边界：`[上游源码]` `backend/src/main.js` 对 `Promise.withResolvers` 做了 polyfill 兜底，并对 `worker_threads.markAsUncloneable` 做空实现兜底（注释提到 "Node < 22"），`[推测]` 实际下限大致为 Node 20/22，但**未验证**。Agent 的 doctor 应要求 ≥ 22 并推荐 24 LTS。

### 2.5 环境变量（官方文档 × 上游源码交叉核对）

`[上游文档]` `reference/environment-variables.md` 与 `[上游源码]` grep 一致的核心变量（**注意：数据目录变量名是 `SUB_STORE_DATA_BASE_PATH`，不是 `SUB_STORE_DATA_BASE`**）：

| 变量 | 文档默认值 | 源码实际默认（`[上游源码]`） | 说明 |
| --- | --- | --- | --- |
| `SUB_STORE_BACKEND_API_PORT` | `3000` | `3000`（`restful/index.js:55`） | 后端 API 端口 |
| `SUB_STORE_BACKEND_API_HOST` | 文档写 `127.0.0.1` | **`'::'`（`restful/index.js:56`）** | ⚠️ 源码默认绑定所有接口 |
| `SUB_STORE_DATA_BASE_PATH` | （文档环境变量章节未单列） | **`'.'`（`vendor/open-api.js:120,193`）** | ⚠️ 未设置则写当前工作目录 |
| `SUB_STORE_FRONTEND_PORT` | `3001` | —（前端/Docker 侧） | 前端端口 |
| `SUB_STORE_FRONTEND_HOST` | -（按需开放） | — | 前端监听地址 |
| `SUB_STORE_BACKEND_MERGE` | -（true 时合并端口） | 布尔 env | 合并前后端端口 |
| `SUB_STORE_FRONTEND_BACKEND_PATH` | `/2cXaAxRGfddmGz2yx1wA` | 同名 env | 后端路径前缀（防扫） |
| `SUB_STORE_BACKEND_PREFIX` | - | 同名 env | 后端也加该前缀 |
| `SUB_STORE_FRONTEND_PATH` | -（Docker 自带） | 同名 env | 前端静态目录 |
| `SUB_STORE_CORS_ALLOWED_ORIGINS` | `https://sub-store.vercel.app,http://substore.stash,https://substore.stash` | 实测日志确认同一默认值 | CORS allowlist |
| `SUB_STORE_BODY_JSON_LIMIT` | `1mb` | 实测日志 `[BACKEND] body JSON limit: 1mb` | JSON body 上限 |
| `SUB_STORE_MAX_HEADER_SIZE` | `32768` | undici header 上限 | 订阅响应头过大时报 `Headers Overflow Error` 可调大 |
| `SUB_STORE_BACKEND_DEFAULT_PROXY` | - | 例 `socks5://a:b@host:7890` | 脚本请求默认代理 |
| `SUB_STORE_BACKEND_SYNC_CRON` | - | 定时同步到私有 Gist | 旧 `SUB_STORE_BACKEND_CRON` 自 2.14.376 弃用；Docker 旧 `SUB_STORE_CRON` 不再支持 |
| `SUB_STORE_BACKEND_UPLOAD_CRON` / `SUB_STORE_BACKEND_DOWNLOAD_CRON` | - | 定时备份 / 恢复 | 对应"我的 → 备份/上传" |
| `SUB_STORE_PRODUCE_CRON` | - | `cron,类型,名称`，`sub`/`col` | 脚本缓存预热 |
| `SUB_STORE_PUSH_SERVICE` | - | shoutrrr URL（Telegram/Bark/PushPlus） | 依赖外部 `shoutrrr` 二进制 |
| `SUB_STORE_DATA_URL` / `SUB_STORE_DATA_URL_POST` | - | 启动时拉取并恢复数据 | 可配 Gist Raw 链接 |
| `SUB_STORE_MMDB_COUNTRY_PATH` / `SUB_STORE_MMDB_ASN_PATH` / `SUB_STORE_MMDB_*_URL` / `SUB_STORE_MMDB_CRON` | - | 本地 MaxMind GeoLite2 | 用于落地/入口检测脚本 |
| `SUB_STORE_BACKEND_CUSTOM_NAME` / `SUB_STORE_BACKEND_CUSTOM_ICON` / `SUB_STORE_X_POWERED_BY` | - | 自定义显示/响应头 | 低风险 |
| `SUB_STORE_PLATFORM_BANNER` / `SUB_STORE_BANNER*` / `SUB_STORE_LOG_PREFIX_RE` | - | 显示/日志前缀类 | 低风险 |
| `HOST` / `PORT` | `127.0.0.1:9876` | http-meta 专用 | ⚠️ 默认 `9876` 可能与其他服务（如 ddns-go）冲突 |

`[实测]` `/api/utils/env` 会把所有 `SUB_STORE_*` 环境变量的值原样回显在 `meta.node.env` 中（本机实测回显了 `SUB_STORE_DATA_BASE_PATH`、`SUB_STORE_BACKEND_API_PORT`、`SUB_STORE_BACKEND_API_HOST`）。**这既是有用的健康检查，也是信息泄漏面**：Agent 不应把该响应整体透传给前端/日志。

---

## 3. 实测记录（体积/内存/启动/坑）

> 环境：macOS (Darwin arm64)，Node `v25.2.1`，pnpm `11.0.9`，Docker OrbStack Server `29.4.0`（daemon 可用但**镜像仓库不可达**）。工作副本与数据目录均在 `/tmp/r05-substore/`，测试数据为自造的假订阅，**任务结束已删除临时目录并结束所有测试进程**。

### 3.1 Docker 路径 `[未验证]`

`docker info` 确认 daemon 正常（`Server Version: 29.4.0`，Storage Driver `overlay2`），但 `docker pull` 无法从 Docker Hub 或任何镜像源拉取 `xream/sub-store`。因此以下数据**未能实测**，`[推测]` 值仅供规划参考，必须在有镜像源的机器上复测：

| 指标 | 状态 | `[推测]` 参考 |
| --- | --- | --- |
| 镜像体积（`docker images` SIZE） | `[未验证]` | Node 24 + 已构建 bundle 的镜像量级约 **150–300 MB**（取决于基础镜像是否为 alpine） |
| 镜像 tag 规则 | `[上游文档]` 存在 `http-meta` 变体 | `[推测]` `<version>`、`latest`、`<version>-http-meta` |
| 容器内存（`docker stats --no-stream`） | `[未验证]` | 与 Node 直跑同量级，**60–120 MB** |
| 容器内默认监听 | `[上游文档]` 前端 3001 / 后端 3000 | — |

**未执行但建议的验证步骤**（写入部署文档/doctor 测试用例）：

```bash
docker pull xream/sub-store:latest
docker images xream/sub-store --format '{{.Repository}}:{{.Tag}} {{.Size}}'
mkdir -p /tmp/ssdata
docker run -d --name ss --restart=always \
  -e SUB_STORE_FRONTEND_BACKEND_PATH=/r05probe \
  -e SUB_STORE_BACKEND_API_HOST=127.0.0.1 \
  -p 127.0.0.1:3001:3001 -v /tmp/ssdata:/opt/app/data xream/sub-store
docker stats --no-stream ss
curl -s http://127.0.0.1:3001/r05probe/api/utils/env
docker rm -f ss
```

### 3.2 Node 路径 `[实测]`

| 指标 | 实测值 | 方法/证据 |
| --- | --- | --- |
| 依赖安装耗时 | **12.0 s**（含下载，`real 12.02`） | `pnpm install --frozen-lockfile`，Node 25.2.1 / pnpm 11.0.9；无需 `--no-frozen-lockfile` |
| `node_modules` 体积（含 devDependencies） | **139 MB**（`.pnpm` 138 MB，602 个包目录） | `du -sh node_modules` |
| production bundle 体积 | **3.0 MiB**（3,096,828 B），自包含 | `dist/sub-store.bundle.js`，与官方 Release 资产字节数一致 |
| bundle 是否需 `node_modules` | **完全不需要** | 把 `node_modules` 移走后仍 `HTTP 200` 返回 `version 2.39.6` |
| 构建耗时 | **5.5 s** | `pnpm bundle:esbuild`（`real 5.46`） |
| 冷启动到首个成功请求 | **130 ms** | 起进程后轮询 `/api/utils/env` 至首个 200 |
| 空闲内存 | **60 MB** | macOS `footprint <pid>` → `Footprint: 60 MB`；两次独立实例一致 |
| API 响应延迟 | **0.5–3.5 ms**（10 次，空闲） | `curl -w %{time_total}` → `/api/utils/env` |
| `runtime-manifest.json` 声明的 npm 依赖 | **`[]`（无）** | 本地产物与官方 Release 资产一致；`workerThreads: true`、`childProcess: true`、`externalBinary: ["shoutrrr"]` |

自造数据启动后数据目录内容（`[实测]`）：

```
<data>/root.json        128 B   缓存类：sub-store-cached-resource / -headers-resource / -script-resource
<data>/sub-store.json   181 B   业务数据：subs / collections / artifacts / rules / files / tokens /
                                schemaVersion "2.0" / settings / archives / modules
```

### 3.3 实测踩到的坑（对 doctor 与部署文档最有价值）`[实测]`

1. **数据目录必须先存在**：`SUB_STORE_DATA_BASE_PATH=/tmp/r05-substore/offline/data`（该目录不存在）时进程立即崩溃退出：
   `Error: ENOENT: no such file or directory, open '/tmp/.../data/root.json'`，栈顶为 `initCache()`。
   → Agent 生成 systemd 单元 / docker run 时必须先确保数据目录存在且归属正确；doctor 应检查该路径可写。
2. **默认监听 `::`（所有接口），不是文档写的 `127.0.0.1`**：不设 `SUB_STORE_BACKEND_API_HOST` 时日志为 `[BACKEND] listening on :::3032`。
   → 任何"只监听本机"的安全假设都必须由我们显式设置该变量来保证。
3. **后端 API 无认证**：无凭据 `GET /api/subs` → `HTTP 200 {"status":"success","data":[]}`；无凭据 `POST /api/subs` → `HTTP 201`，**且已写入 `sub-store.json` 落盘**。
   → 与坑 2 叠加就是"任意同网段写入 + 读取订阅凭据"。这是本调研最重要的安全结论。
4. **`/api/subs` 等业务端点在没有对应资源时返回 `RESOURCE_NOT_FOUND`**：`GET /download/sub?...` 在未创建同名订阅时返回 `404 {"code":"RESOURCE_NOT_FOUND"}`。即"订阅必须先在前端/API 建好对象，再由 `/download/sub?name=...` 取用"；只有 `content=` 参数的内联转换路径不是这样用（`[未验证]`：内联 `content` 的正确参数组合未逐一验证）。
5. **`GET /` 返回 `200`**：官方前端未部署时，后端根路径仍返回内容（非 404），因此**不能用"根路径 200"判定前端可用**；健康检查应使用 `/<prefix>/api/utils/env`。
6. **时区/日志**：日志时间戳为本地时区非 ISO8601（实测形如 `9/12/2026, 12:42:10 PM`），解析时需注意，不建议 Agent 依赖其做严格时间对齐。
7. **运行时环境细节**：`/api/utils/env` 会回显运行节点的 `process.version`、`argv`、`filename`、`dirname` 以及全部 `SUB_STORE_*` 环境变量值（见 §2.5），`dirname`/`argv` 会泄漏部署路径。
8. **pnpm 安装的环境依赖**：`backend/package.json` 的 `preinstall` 是 `npx only-allow pnpm`（`[上游源码]`），用 npm 安装会被拦截；本地沙箱下 pnpm 需要可写的 `HOME`/`PNPM_HOME`（本测试通过重定向 `HOME`、`PNPM_HOME`、`XDG_*` 到临时目录解决）。这与 LXC 部署关系不大，但影响"源码构建"路径的自动化。
9. **`SUB_STORE_FRONTEND_BACKEND_PATH` 影响脚本能力**：`[上游源码]` `core/proxy-utils/index.js` 明确警告——Node 环境下"脚本操作、脚本过滤和修改响应必须设置 `SUB_STORE_FRONTEND_BACKEND_PATH` 才能生效"，否则功能静默不生效。这是一个"配了却没效果"的隐蔽坑。

---

## 4. 持久化与数据备份

### 4.1 存储模型 `[实测]` + `[上游源码]`

- **纯 JSON 文件，无数据库**。`[上游源码]` `vendor/open-api.js` 的 `initCache()` / `persistCache()`：每个 store 一个 `<name>.json`，另有全局 `root.json`。实测目录内容见 §3.2。
- **路径由 `SUB_STORE_DATA_BASE_PATH` 决定，默认 `.`（当前工作目录）**（`open-api.js:120,193`）。这是 systemd 部署的经典陷阱：不设该变量时数据落在 `WorkingDirectory`，`systemctl restart` 或换目录就会"丢数据"。
- **损坏自愈**：`initCache()` 解析失败时会先把原文件复制为 `<name>_<timestamp>.json` 再重建空对象；`root.json` 同理（`root_<timestamp>.json`）。`[实测]` 未人为制造损坏场景，该行为来自源码。
- **写入方式**：`persistCache()` 用 `fs.writeFileSync` **直接覆盖**，没有临时文件 + 原子 rename。→ 进程在写入中途被杀可能留下半截 JSON（但下次启动有自愈备份），**不适合把它的数据目录当成高可靠存储**。
- `[上游源码]` 明文存储：订阅 URL（常含机场 token）、`settings`（含 `gistToken` 等）都在 `sub-store.json` 里。**没有内置的静态加密**；备份格式支持 Base64 编码（官方文档注明"默认使用 Base64 编码"），但**Base64 不是加密**。

### 4.2 备份 / 迁移建议 `[上游文档]` + `[实测]`

| 方式 | 说明 | 评价 |
| --- | --- | --- |
| **文件级归档**（推荐给 Agent） | 停写窗口内归档 `<data>/*.json`（至少 `sub-store.json`）+ 可选 MMDB 文件 | 最简单、无外部依赖、可离线。Agent 侧只需把它纳入"订阅后端数据备份"，与我们在 `docs/architecture.md` 的 `subscriptions/` 备份策略并列 |
| 官方 Gist 备份 | `[上游文档]` 接口 `/api/utils/backup?action=upload` / `?action=download`（可加 `keep=settings.gistToken`）；`settings.gistToken` 是 GitHub Token | 需要外网 + GitHub Token；**不推荐 MVP 依赖**（隐私面：订阅凭据进第三方 Gist） |
| 定时备份 | `SUB_STORE_BACKEND_UPLOAD_CRON` / `SUB_STORE_BACKEND_DOWNLOAD_CRON` | 同上，属于用户自选能力，不由 Agent 强制 |
| 启动时自动恢复 | `SUB_STORE_DATA_URL`（可从 Gist Raw 拉取）+ `SUB_STORE_DATA_URL_POST` | 用户自选 |
| MMDB | `[上游文档]` `SUB_STORE_MMDB_COUNTRY_PATH` / `_ASN_PATH` 指向本地文件 | 与数据目录分开备份 |

**升级/迁移前必做**：`[上游文档]` 官方"更新前备份"章节明确建议先备份数据（见"我的 → 备份/恢复"）或用 `SUB_STORE_BACKEND_UPLOAD_CRON` 定时备份。Agent 在提供受控升级时应在升级前自动做一次文件级快照。

---

## 5. 升级与回滚策略

### 5.1 官方机制 `[上游源码]` + `[上游文档]`

| 维度 | 事实 |
| --- | --- |
| 版本号来源 | `backend/package.json` 的 `version`（实测 `2.39.6`）；Release tag 即该版本号（`main.yml`：`substore_release=require('./package.json').version`） |
| 发布触发 | push 到 `master` 且改动路径为 `backend/package.json`（`[上游源码]` `main.yml` 的 `paths`）；即**每次版本提交都会立刻发 Release** |
| 发布频率（实测） | 极高：`2.39.6`→`2.39.5`→`2.39.4` 等**同日多次**发布（2026-09-09 ~ 09-11 至少 10 个 tag） |
| Release 资产 | `sub-store.bundle.js`、`runtime-manifest.json`、各代理 App 模块产物（`main.yml` `files:`） |
| Docker tag 策略 | `[上游文档]` 未给出版本固定指引，只给 `xream/sub-store` 与"带 `http-meta` tag"；`[未验证]` 是否有 `latest`/语义化 tag 的完整列表（Docker Hub 不可达） |
| 官方"自动更新" | `[上游文档]` 推荐 **Watchtower**（`--interval 3600`）自动拉新镜像 |
| 版本可见性 | `[上游文档]` + `[实测]` `GET /<prefix>/api/utils/env` → `data.version`；官方文档明确把它当健康检查与版本核对接口 |
| 数据 schema | `sub-store.json` 内含 `"schemaVersion": "2.0"`（实测），启动时 `migrate()` 会跑迁移并打印 `Start migrating... / Migration complete!` |
| 自更新能力 | **无**（没有内置 self-update 端点） |

### 5.2 建议：不自动升级，只做版本检测与提示（+ 可选的受控升级）`[推测]`（本项目决策建议）

理由：

1. **上游发布节奏极快**（同日多版），自动跟随意味着不可预测的重启与 schema 迁移频率，违背 `AGENTS.md` 的 "rollback > destructive update"。
2. **Sub-Store 不在我们的关键路径上**（见 §1 与 §8）：它挂掉只应导致"订阅转换失败 → 保留旧配置"，没有理由为它引入高风险自动更新。
3. **它没有官方的版本回退机制**：JSON schema 迁移是单向的，一旦新版本改写了数据文件，回退镜像可能导致数据不可读。因此"自动升级"实际上**不可回滚**。
4. AGPL-3.0 外部组件，由用户自行掌控其版本更符合"可替换外部组件"的定位。

具体建议：

```text
Agent 行为：
  - 只读检测：GET <base_url>/api/utils/env → data.version（+ 记录 testedNode）
  - 发现版本变化：仅记录事件 + Doctor 报告 + Web UI 提示
  - 不自动 pull / 不自动重启 Sub-Store

可选"受控升级"（默认关闭，需用户显式开启）：
  1. 前置：数据目录文件级快照（含 sub-store.json）
  2. 执行：由用户选择的部署方式驱动
       Docker：docker pull <镜像:tag> && 重建容器（保留同一 -v 挂载）
       Node  ：下载 Releases 指定 tag 的 sub-store.bundle.js + runtime-manifest.json
               → 校验 runtime-manifest.json 的 testedNode 与本地 Node 兼容
               → 原子替换 bundle 文件 → 重启进程
  3. 后置：轮询 /api/utils/env 校验版本；失败则回滚 bundle 文件 + 恢复数据快照
  4. 全程：串行化（同一时刻只允许一个升级任务），并把结果作为 JobFinished 事件暴露
```

**数据备份/迁移**（与 §4.2 一致）：升级前快照数据目录；跨机迁移只需复制数据目录（JSON 文件）并在目标机设置相同的 `SUB_STORE_DATA_BASE_PATH`，无需额外导出。

---

## 6. 网络暴露面与安全建议

### 6.1 暴露面事实 `[实测]` + `[上游源码]`

| 事实 | 证据 | 风险 |
| --- | --- | --- |
| 后端 API **无认证** | `[实测]` 无凭据 `GET /api/subs` 200、`POST /api/subs` 201 且落盘 | 高：可读订阅凭据、可写入/篡改订阅 |
| 前端地址前缀 `SUB_STORE_FRONTEND_BACKEND_PATH` **不是认证** | `[上游文档]` 描述为"防扫"用途 | 中：只是隐蔽路径，非安全边界 |
| 源码默认 `SUB_STORE_BACKEND_API_HOST='::'` | `[上游源码]` + `[实测]` 日志 `listening on :::3032` | 高：默认绑定所有接口 |
| 环境变量回显 | `[实测]` `/api/utils/env` 的 `meta.node.env` | 中：泄漏路径与配置 |
| CORS allowlist 默认仅官方前端域名 | `[实测]` 启动日志 / `[上游文档]` | 低：这是浏览器跨域读限制，**不阻止非浏览器直接请求** |
| 订阅输出链接用 `tokens` 机制 | `[上游源码]` `restful/token.js` 的 `/api/token`、`/api/tokens` | 说明：这些 token 是**分享订阅链接**的 token，**不是 API 访问认证** |

`[实测]` 结论一句话：**Sub-Store 后端默认既无认证也不默认只绑本机；把它暴露到网络等同于把订阅凭据公开。**

### 6.2 建议的部署基线

```text
1. 绑定：SUB_STORE_BACKEND_API_HOST=127.0.0.1（强制，由 Agent 写入部署模板）
   前端 SUB_STORE_FRONTEND_HOST 默认也不对外；需要 Web 管理时通过我们的 Web UI 反代，
   或显式绑定到受信内网地址。
2. 端口：只用 127.0.0.1 上的 loopback 端口，绝不映射到 0.0.0.0/公网。
3. 反代/认证：若必须远程访问，放在我们自己的认证反代之后（或 SSH 隧道 / WireGuard），
   并同时设置 SUB_STORE_FRONTEND_BACKEND_PATH 前缀 + 严格 CORS allowlist。
   不要把 Sub-Store 直接挂到公网域名根路径。
4. CORS：显式设置为实际使用的前端 origin（不要用 `*`）。
5. 日志：Agent 不记录订阅 URL / token / /api/utils/env 的完整 env 回显。
```

### 6.3 与 R04（订阅 API 安全）的衔接

本节的每条结论都应在 R04 的"认证/暴露面"结论下复核：**R04 若得出"Sub-Store 无认证能力"的结论，本调研从部署侧给出同一结论的独立证据（无凭据写入成功）**，两者合并即支持"Sub-Store 必须置于 loopback + 我们的认证边界之后"的架构决定。Sub-Store 自身**不提供**可供 Agent 复用的认证机制。

---

## 7. PVE LXC 适配要点

`[实测]`（macOS Node 路径）+ `[上游文档]`/`[上游源码]` 推广结论：

| 维度 | 结论 |
| --- | --- |
| 运行形态 | **Node 长驻进程**（Express HTTP 服务）。CPU 需求极低（空闲几乎为 0，请求期毫秒级），`[推测]` 单核足够 |
| 内存下限 | `[实测]` 空闲 **60 MB**；考虑转换大订阅时的峰值与 Node GC，`[推测]` 建议 **≥ 256 MB** 可用内存，512 MB 更稳 |
| 磁盘 | Node 路径：bundle **3.0 MB** + 数据目录（初始 309 B，随订阅数增长）。Docker 路径：镜像 `[未验证]` `[推测]` 150–300 MB + 数据目录 |
| Node 常驻依赖 | **Node 运行时必须常驻**（无纯二进制形态）。Node 路径需要系统 Node ≥ 22（推荐 24 LTS，官方 `testedNode=24.15.0`）；**不需要 `node_modules`**（实测移走后正常运行） |
| unprivileged LXC | **可以跑**：纯用户态 HTTP 服务，**不需要** `CAP_NET_ADMIN`、`/dev/net/tun`、nftables、内核模块或特权。这是它与 Mihomo 数据面最大的区别 |
| 外网依赖 | **首次安装需要外网**（`docker pull` 或下载 Node + Release bundle）；`[实测]` 无 MMDB 配置时**启动完全离线**，运行期仅在你启用订阅拉取/脚本/Gist 备份时需要外网 |
| 时区/Locale | `[实测]` 日志用本地时区；无需特殊 locale（`[上游源码]` 依赖 `fastestsmallesttextencoderdecoder`，不依赖系统 ICU） |
| 文件权限 | 数据目录必须由运行用户可写且**必须预先存在**（坑 §3.3-1）；systemd 场景注意 `WorkingDirectory` 不得作为隐式数据目录 |
| 进程监督 | `[推测]` 建议独立 systemd unit（`Restart=on-failure`），与 Agent 单元解耦：**Sub-Store 挂掉不能让 Agent 或 Mihomo 跟着失败**（与设计文档 §44 一致） |
| 与 Mihomo 同机 | 无冲突：Sub-Store 是纯 HTTP 服务，不碰 TUN/路由/防火墙。唯一要注意的是端口与代理设置（`SUB_STORE_BACKEND_DEFAULT_PROXY` 可指向本地 Mihomo 的 `7890` 类端口）。⚠️ 启用 http-meta tag 时其默认端口 **9876** 可能与应用冲突 |

**端口规划建议（同机 PVE LXC，Sub-Store 可选）**：

```text
127.0.0.1:9090     Mihomo external-controller（既有约定，仅 loopback）
127.0.0.1:7890     Mihomo mixed proxy（按用户配置）
127.0.0.1:3000     Sub-Store 后端 API（仅 loopback；或与前端合并后不再单独暴露）
127.0.0.1:3001     Sub-Store 前端（仅 loopback；由我们的 Web UI 反代时可不映射）
127.0.0.1:9876     http-meta（仅在使用该镜像变体时；注意冲突）
```

`[推测]` 建议 Agent 默认采用 **`SUB_STORE_BACKEND_MERGE=true` + 单端口**（如 `127.0.0.1:3001/<prefix>`），以减少暴露面与端口占用；此形态对 Adapter 只需一个 `base_url` 即可覆盖前后端。

---

## 8. MVP 默认部署方案与降级路径

### 8.1 默认方案（明确结论）

> **Sub-Store 是可选的外部 Converter / Subscription Backend：MVP 默认不安装、不内嵌、不由 Agent 托管其生命周期。**

```text
MVP 默认：
  - Agent 不安装、不升级、不托管 Sub-Store 进程
  - 默认不部署 Sub-Store；订阅功能以 Native 解析直连订阅（降级态）为默认可用路径
  - 若用户已自行部署（Docker 官方镜像 xream/sub-store，或 Node 直跑官方 Release bundle），
    则 Agent 通过可配置 base_url 接入
  - Agent 不依赖 Sub-Store 内部实现，仅使用公开的 HTTP 接口
    （GET /download/sub?target=...&url=...&ua=... 等，见设计文档 §12/§43）
  - Sub-Store 不可用 = 该次订阅更新失败 = 保留旧配置（设计文档 §44）
```

- **首选推荐部署方式（写进部署文档，由用户执行）**：Docker 官方镜像 `xream/sub-store`，`-p 127.0.0.1:3001:3001`，`-v <host-data>:/opt/app/data`。
- **无 Docker 时的推荐方式**：Node ≥ 22（推荐 24）+ 官方 Release `sub-store.bundle.js`（零依赖），`SUB_STORE_BACKEND_API_HOST=127.0.0.1` + `SUB_STORE_DATA_BASE_PATH=<预先创建的可写目录>`。
- **不使用**：Vercel 部署后端（不支持）、Cloudflare Workers/Pages（不兼容）、Bun（未验证）。官方前端 `sub-store.vercel.app` 仅作为"用户自选 UI"，不作为我们的依赖（我们有自己的 Web admin；且官方前端要求能访问 Vercel）。

### 8.2 接入判定与降级路径

```text
启动 / 配置变更时：
  Sub-Store endpoint configured?
      ├─ NO  → Converter = NativeConverter（降级态）
      │        订阅功能 = 仅原生解析直连订阅（不做高级处理/脚本）
      │        Doctor 输出：SubStore = Unavailable/NotConfigured（非错误，是合法降级）
      └─ YES → Health Check: GET <base_url>/api/utils/env
                 ├─ 2xx 且 data.version 可解析
                 │     → Converter = SubStoreConverter
                 │       记录 version（版本检测/提示用）
                 └─ 失败/超时/版本不可解析
                       → Converter 标记 Misconfigured/Unavailable
                         若曾成功过：继续用缓存的转换结果或 Native，绝不中断 Mihomo
                         绝不因 Sub-Store 不可用而 stop/disable Mihomo（设计文档 §44）

换算失败时（运行时）：
  Sub-Store timeout / 5xx / 格式错误
      → Subscription Update Failed
      → old config stays active（回滚流程见 AGENTS.md「Failure behavior」）
```

**与 R14 部署模型的衔接**：本方案对应 **"Agent 单机部署 + 外部可选服务（Model B：Agent 与 Mihomo 同机，Sub-Store 为可选外部组件）"**。它**不**要求 Model 中引入"Sub-Store 由 Agent 托管"的变体；若 R14 中已有 "sidecar/bundle 安装 Sub-Store" 的 Model，本调研建议**不在 MVP 落地**（理由：AGPL-3.0 边界 + 上游极快发布节奏带来的升级维护负担 + 无认证需额外反代）。`[推测]`：R14 若需要单一 Model 承载"可选外部组件"，应把它表达为"**外部依赖 + 健康检查 + 降级**"，而不是"内置服务"。

---

## 9. 对 Agent 架构的影响（Adapter/Doctor/配置项）

### 9.1 Adapter（`SubscriptionConverter` 的 SubStore 实现）

- 沿用设计文档 §43：`SubStoreConverter { base_url: Url, client: reqwest::Client }`；**只依赖公开 HTTP 行为**，不依赖 JSON 数据文件结构（`sub-store.json` 是内部实现细节，不进 Domain/Application）。
- `base_url` 应支持两种形态：**双端口**（后端 `127.0.0.1:3000`）与**单端口合并前缀**（`127.0.0.1:3001/<prefix>`）。Adapter 内部统一按"公网可路由的 base url"处理。
- 健康检查固定用 `GET <base_url>/api/utils/env`（`[上游文档]` 明确推荐为健康检查接口），解析 `data.version`。
- 认证：Sub-Store 自身无认证 → Adapter 需要支持**可选的自定义请求头**（如用户在其前置反代上配置的 `Authorization`/自定义头），并支持 mTLS/自定义 CA `[推测]`（MVP 可只做自定义 header）。
- 超时与失败：Adapter 必须**永不 panic**、永不把失败升级为"Mihomo 不可用"；重试仅限幂等 GET。

### 9.2 Doctor 检查项（建议）

```text
SubStore:
  - Configured?                 （未配置 → NotConfigured，合法降级，不算失败）
  - Endpoint reachable?         （TCP + /api/utils/env 2xx）
  - Version                    （data.version，用于版本提示）
  - Node compatibility          （若为 Node 部署：要求 ≥ 22，推荐 24；对照 runtime-manifest.json 的 testedNode）
  - Bind safety                 （若 Agent 能读到其配置/环境：SUB_STORE_BACKEND_API_HOST 是否为 127.0.0.1；
                                 默认 '::' 判定为 Misconfigured 并告警）
  - Data dir writable & exists  （SUB_STORE_DATA_BASE_PATH 存在且可写；缺失会导致对端启动崩溃）
  - CORS config                 （是否被设为 `*`）
  - Exposure warning            （若检测到监听在非 loopback，明确提示凭据泄漏风险）
```

能力状态使用 AGENTS.md 规定的枚举：`Supported` / `Unsupported` / `Unavailable` / `Misconfigured` / `Unknown`。

### 9.3 配置项（建议 schema，`/etc/proxy-agent/config.toml`）

```toml
[subscription.backends.substore]
enabled  = false                      # MVP 默认 false：可选外部组件
base_url = "http://127.0.0.1:3001/2cXaAxRGfddmGz2yx1wA"
# 认证由用户前置反代提供；这里是 Agent 侧附加的静态头
auth_header_name  = "Authorization"   # 可选
auth_header_value = "Bearer <redacted>"
health_path       = "/api/utils/env"
timeout_ms        = 10000
verify_tls        = true
version_check     = true              # 只检测与提示，不自动升级
managed           = "external"        # external | docker | node  （MVP 只允许 external）
data_path_hint    = "/var/lib/sub-store"   # 仅用于 Doctor 提示，不由 Agent 创建
```

要点：**`enabled=false` 是默认值**；开关只影响订阅转换路径，不影响其他任何功能。

---

## 10. 证据与来源

### 10.1 上游官方文档

- 官方文档仓库 `sub-store-org/doc`（tarball 解包，`main` 分支）：
  - `guide/installation.md` — 安装方式总表、Docker 快速启动示例、端口/数据卷说明、http-meta tag、官方前端
  - `guide/update.md` — Docker 手动更新（`docker pull` + 重建）、Watchtower 自动更新、更新前备份、版本验证接口
  - `reference/environment-variables.md` — 环境变量完整表（端口/监听、CORS、定时任务、备份恢复、MMDB、自定义显示）
  - `guide/getting-started.md` — 前端/后端职责划分，"Docker / 服务器 / 云平台上"运行后端
  - `guide/troubleshooting.md` — `docker logs` / `docker ps` 端口观察方法
- 文档站点：<https://github.com/sub-store-org/doc>
- 官方 Wiki：<https://github.com/sub-store-org/Sub-Store/wiki>
- Sub-Store 主仓库：<https://github.com/sub-store-org/Sub-Store>
- Docker 镜像：<https://hub.docker.com/r/xream/sub-store>
- 官方前端：<https://sub-store.vercel.app>

### 10.2 上游源码（`sub-store-org/Sub-Store@master`，版本 `2.39.6`）

- `backend/src/restful/index.js:55-56` — `SUB_STORE_BACKEND_API_PORT || 3000`、`SUB_STORE_BACKEND_API_HOST || '::'`
- `backend/src/vendor/open-api.js:120,193` — `SUB_STORE_DATA_BASE_PATH || '.'`、`initCache()` / `persistCache()` 的 `root.json` + `<name>.json` 与损坏自愈
- `backend/src/vendor/express.js:108` — `app.listen(port, host)`
- `backend/src/main.js` — Node 分支的 polyfill 兜底（`Promise.withResolvers`、`worker_threads.markAsUncloneable`）与 `migrate(); serve();`
- `backend/src/core/proxy-utils/index.js:282-286` — `SUB_STORE_FRONTEND_BACKEND_PATH` 不设则脚本类功能静默失效
- `backend/src/restful/token.js` — `tokens` 是分享链接 token，非 API 认证
- `backend/src/restful/miscs.js` — `/api/utils/backup`、Gist 备份需要 GitHub Token
- `backend/bundle-esbuild.js` — 产物清单与 `runtime-manifest.json` 生成逻辑（`testedNode`、`externalBinary: ["shoutrrr"]`）
- `backend/package.json` — 版本、`preinstall: npx only-allow pnpm`、无 `engines` 字段
- `.node-version` — `24.15.0`
- `.github/workflows/main.yml` — 触发路径 `backend/package.json`、Release 资产清单、`release` 分支同步
- `config/README.md` — "服务器/云平台/Docker/Android 版"说明与 CORS 默认值

### 10.3 同组织官方项目（佐证 Node 打包与版本策略）

- `sub-store-org/SubDock`（"Native cross-platform runtime manager for Sub-Store"）：
  - `tool/prepare_backend.sh` — 下载 Release 的 `sub-store.bundle.js` + `runtime-manifest.json`
  - `tool/prepare_runtime.sh` — 从 manifest 读 `testedNode` 并下载对应官方 Node 二进制（含 `SHASUMS256.txt` 校验）
  - `tool/versions.sh` — `SUBDOCK_NODE_VERSION` 可 pin，默认从 manifest 取
  - README — "The Linux build is offline after preparation and always runs with its packaged Node.js runtime selected by the Backend manifest"

### 10.4 本次实测证据（`/tmp/r05-substore/`，已清理）

- `pnpm install --frozen-lockfile`：`real 12.02`，`node_modules` 139 MB
- `pnpm bundle:esbuild`：`real 5.46`，产出 `dist/sub-store.bundle.js` 3,096,828 B
- 启动日志：`[BACKEND] listening on 127.0.0.1:3030`（显式设置 host 时）与 `listening on :::3032`（默认时）
- `/api/utils/env` → `{"backend":"Node","version":"2.39.6",...}`；`meta.node.env` 回显全部 `SUB_STORE_*`
- 无凭据 `GET /api/subs` → `200`；无凭据 `POST /api/subs` → `201` 并写入 `sub-store.json`
- 数据目录缺失时 `ENOENT ... root.json` 崩溃
- 移走 `node_modules` 后仍 `200`
- 冷启动 130 ms；`footprint` 空闲 60 MB；`/api/utils/env` 延迟 0.5–3.5 ms

---

## 11. 未验证假设与开放问题

| 编号 | 未验证项 | 影响 | 建议的验证方式 |
| --- | --- | --- | --- |
| Q1 | **Docker 镜像的体积 / 内存 / 默认监听 / tag 列表** | 中（部署文档的容量规划） | 在有可用镜像源的机器上执行 §3.1 的验证脚本；补 `docker images`、`docker stats --no-stream`、`docker logs` 首行 |
| Q2 | 官方 Docker 镜像的确切 tag 规则（`latest` 是否指向最新版？`<version>-http-meta` 是否稳定？） | 中（升级策略与固定版本能力） | 在可达环境列出 `xream/sub-store` tags；确认能否按 `<version>` 固定 |
| Q3 | 官方 Docker 镜像的**基础镜像与 Node 版本**（是否等于 `testedNode`） | 中（与 Node 直跑的等价性） | `docker inspect` / `docker run --entrypoint node -v` |
| Q4 | 源码/Release 声明的 `node:sqlite` builtin 实际用途（`grep` 未在 `backend/src` 找到直接引用） | 低（可能抬高 Node 下限） | grep bundle 中 `sqlite` 的使用点；确认是否影响最低 Node 版本 |
| Q5 | 最低可用 Node 版本（`< 22` 是否可行） | 低（因为我们会要求 ≥ 22） | 用 Node 20 / 22 各起一次，验证启动 + 一次转换 |
| Q6 | Bun 兼容性 | 低（不在 MVP 支持面） | 若未来要支持再测；本调研建议不承诺 |
| Q7 | `/download/sub` 内联 `content=` 参数的完整正确用法（本机用 `content=` 返回 `RESOURCE_NOT_FOUND`，测试参数可能不完整） | 中（R03/R04 的接口契约） | 以 R03 的 `/download/sub` 参数契约调研为准；在本调研基础上补一次带完整参数（`target` + `content` + 必要的 `ua`）的实测 |
| Q8 | Sub-Store 在高负载/大订阅下的峰值内存与 CPU | 中（LXC 资源规划） | 构造 10k+ 节点的假订阅做一次转换，采样 `footprint`/`docker stats` |
| Q9 | `SUB_STORE_FRONTEND_PORT`/`HOST` 在纯 Node 直跑（非官方镜像）下的行为（这两个是前端/Docker 侧变量，Node 直跑只有后端） | 低 | 明确写进部署文档："Node 直跑 = 只有后端 API，无内置前端" |
| Q10 | 数据文件在升级跨 schemaVersion 时的向前/向后兼容性 | 高（"不自动升级"策略的依据之一） | 用两个相邻版本（如 2.38.x → 2.39.6）跑同一数据目录，观察 `migrate()` 行为与回退可行性 |
| Q11 | R04（订阅 API 安全）的最终结论 | 中（影响 §6.3 的合并结论） | 由 R04 负责；本调研已提供"无认证写入成功"的独立证据 |
| Q12 | R14 部署模型的确切 Model 编号与命名 | 中（§8.2 的衔接需要准确 Model 名） | 由 R14 输出后回填本节 |

---

## 附：一句话交付

**MVP 默认 = Sub-Store 为可选外部组件，默认不安装、不托管、不自动升级；用户以 Docker（`xream/sub-store`，`-p 127.0.0.1:3001:3001`，`-v <data>:/opt/app/data`）或 Node ≥ 22 + 官方 Release `sub-store.bundle.js`（零 npm 依赖，实测冷启 130 ms / 空闲 60 MB）自行部署；Agent 仅通过可配置 `base_url` 调用公开 HTTP 接口并做健康检查与版本提示；未配置或不可用时降级为 Native 解析直连订阅，且始终保留上一份可用配置。**
