# R04 — Sub-Store 公开 API inventory

> 状态：已完成（由父 agent 亲自补做，原调研子任务在写盘前因上下文耗尽失败，本文件基于其遗留证据 + 本次补充实测）
> 调研日期：2026-09-12
> 证据等级：`[实测]`（本机实跑 Sub-Store 2.39.6 + 上游源码逐行核对）+ `[上游源码]`
> 关键结论一句话：**Sub-Store 的订阅输出不是无状态转换接口，而是"先入库、再按名字下载"的两段式模型**——`GET /download/:name` 才是真实入口，任务清单里的 `GET /download/sub?url=...` **不是官方文档接口**（无 wiki 记录、无源码特例，`sub` 只是被当作订阅名，故 404）；而 `target=clash` **是非法值**（合法值为 `Clash`/`mihomo`/`ClashMeta`），会返回 500。

---

## 1. 结论摘要（TL;DR）

1. **`/download/sub` 是误传接口** `[实测]`。请求 `/download/sub?target=mihomo&url=...` 返回 **404** `RESOURCE_NOT_FOUND: Subscription sub does not exist!`——`sub` 被解析为**订阅名**，而库中没有该订阅。设计文档 §12/§43 与 Phase 0 清单中"官方 Wiki 公开记录了 `/download/sub` 链接参数"的说法**与当前 v2.39.6 不符**，必须修正。
2. **真实入口是 `/download/:name[/:target]`** `[上游源码]`：`src/restful/download.js:74,82`。另有 `/download/collection/:name`、`/share/sub/:name`、`/share/col/:name`。
3. **两段式模型** `[实测]`：先 `POST /api/subs`（**无任何认证**）把订阅写入数据库，再 `GET /download/:name?target=ClashMeta` 获取转换结果。**这意味着"订阅数据存在 Sub-Store 里"，Agent 无法只把 Sub-Store 当无状态转换器**。
4. **`url=` 覆盖在命名路由上有效** `[实测]`：`GET /download/agenttmp?target=mihomo&url=<另一个地址>` 返回 200，可绕过数据库中的 `sub.url`。这是 Adapter 实现"外部订阅源由 Agent 掌管"的关键机制——但**订阅名仍必须先在库中存在**。
5. **输出不是完整 Mihomo 配置，只是 `proxies:` 片段** `[实测]`：响应体 433 B，仅含 `proxies:` + 两个节点，**没有** `mixed-port` / `external-controller` / `secret` / `dns` / `rules` / `proxy-groups`。Agent 必须自己补全为可运行配置（与设计文档 §16 的"生成配置"职责一致，但它**必须由 Agent 承担**）。
6. **target 枚举已从源码冻结** `[上游源码]`：`qx/QX/QuantumultX`、`surge/Surge`、`SurgeMac`、`Loon`、`Clash`、`meta/clashmeta/clash.meta/Clash.Meta/ClashMeta/mihomo/Mihomo`、`uri/URI`、`v2/v2ray/V2Ray`、`json/JSON`、`stash/Stash`、`shadowrocket/Shadowrocket/ShadowRocket`、`surfboard/Surfboard`、`singbox/sing-box`、`egern/Egern`。**小写 `clash`、`ALL`、`meta`（大写形式未全列）等不存在或会报错**：实测 `target=clash` → **500** `Target platform: clash is not supported!`，`target=ALL` → 500。
7. **响应无订阅元数据头** `[实测]`：成功响应只有 `Content-Type: text/plain; charset=utf-8`，**没有** `subscription-userinfo` / `profile-update-interval` / `content-disposition` / `Cache-Control`。流量与到期信息只在处理 `profile-web-page-url` 时存在（源码 `download.js:361,409,752`），不是稳定契约。
8. **后端 API 完全无认证** `[实测]`：`POST /api/subs` 无凭据返回 **201 且已落盘**（与 R05 独立结论一致）。`tokens` 是分享链接用的 token，**不是 API 鉴权**。
9. **有 `BACKEND PREFIX` 机制** `[实测]`：设置后监听日志显示 `[BACKEND PREFIX] 127.0.0.1:13010/secretpath`，可作为弱访问控制（防扫描），但**不构成认证**。
10. **结论**：Adapter 必须同时实现"写入订阅 + 按名下载"两条路径，并把 Sub-Store 视为**有状态外部服务**；同时必须对 `target` 值做白名单映射（不能透传用户输入）。

---

## 2. 实测环境

| 项 | 值 |
|---|---|
| Sub-Store 版本 | **2.39.6**（`/api/utils/env` 返回 `backend: Node`） |
| 运行方式 | 官方 Release bundle，`node` 直跑（Node v25.2.1），零 npm 依赖 |
| 监听 | `127.0.0.1:13001`（`[BACKEND] listening on 127.0.0.1:13001`） |
| 测试订阅源 | 本机伪造 HTTP 服务 `127.0.0.1:13002/sub`，内容为 base64 节点列表（2 个节点：1×ss + 1×vmess） |
| 数据目录 | 临时 `/tmp/r04-substore/data*`，纯 JSON（`root.json` / `sub-store.json`），无数据库 |
| 证据文件 | `/tmp/r04-substore/r04-final-{mihomo.txt,headers.txt,create.json}`、`server*.log`、上游 `upstream/backend/src/restful/download.js` |

> 注：本次调研环境（macOS）无法拉取 Docker Hub 镜像，容器路径未实测；Node 路径为主线，与 R05 的结论一致。

---

## 3. `/download/sub` 参数清单（源码确认）

### 3.1 源码中的真实查询参数（`src/restful/download.js:106-119`）

```js
let {
    url, ua, content, mergeSources, ignoreFailedRemoteSub,
    produceType, includeUnsupportedProxy, proxy, noCache,
    _fakeNode, fakeSub: _fakeSub,
} = req.query;
const prettyYaml = req.query.prettyYaml ?? req.query['pretty-yaml'];
```

| 参数 | 类型 | 语义 | 证据 |
|---|---|---|---|
| `url` | string | 覆盖该订阅的远端地址（可带 `#insecure` 等后缀，见 `download.js:632-667`） | 源码 + `[实测]` 覆盖成功 |
| `ua` | string | 拉取远端订阅时使用的 User-Agent | 源码 |
| `content` | string | 直接内联订阅内容（绕过远端拉取）。**实测：仅接受 raw 文本，base64 / `data:` 不解码；且不能替代入库——订阅名必须已存在** | 源码 + 实测（Q023） |
| `mergeSources` | string | 合并来源（**在 `/share/*` 路由上被显式禁止**，返回 400 `UNSUPPORTED_SHARE_SUB_MERGE_SOURCES`） | 源码 `download.js:165-183` |
| `ignoreFailedRemoteSub` | flag | 远端拉取失败时的降级策略（有独立模块 `ignore-failed-remote-sub`） | 源码 |
| `produceType` | string | 产出类型（`internal` 等） | 源码 |
| `includeUnsupportedProxy` | flag | 是否包含不支持的节点 | 源码 |
| `proxy` | string | 拉取远端订阅时使用的代理 | 源码 |
| `noCache` | flag | 绕过缓存 | 源码 |
| `_fakeNode` / `fakeSub` | flag | 假节点/假订阅（**分享路由禁止**） | 源码 |
| `prettyYaml` / `pretty-yaml` | flag | YAML 美化输出 | 源码 |
| `platform` 或 `target` | string | 输出目标格式；`platform` 优先于 `target` | 源码 `download.js:97-101` |
| `$options` | JSON/querystring | 注入 `$options` 上下文（支持 `#encodeURIComponent(JSON)` 形式） | 源码 `download.js:135-157` |

### 3.2 `target` / `platform` 的合法值（源码冻结）

`src/core/proxy-utils/producers/index.js` 注册表：

```text
qx | QX | QuantumultX
surge | Surge
SurgeMac
Loon
Clash
meta | clashmeta | clash.meta | Clash.Meta | ClashMeta | mihomo | Mihomo
uri | URI
v2 | v2ray | V2Ray
json | JSON
stash | Stash
shadowrocket | Shadowrocket | ShadowRocket
surfboard | Surfboard
singbox | sing-box
egern | Egern
```

**实测反例（重要）**：

| 请求 | 结果 |
|---|---|
| `?target=mihomo` | **200**，433 B，返回 `proxies:` 片段 |
| `?target=Clash` | **200**，433 B |
| `?target=ClashMeta` | **200**，433 B |
| `?target=clash`（小写） | **500** `Target platform: clash is not supported!` |
| `?target=ALL` | **500** `Target platform: ALL is not supported!` |

> 设计文档 §12 示例写的是 `target=ClashMeta`（正确），但 Phase 0 清单与常见教程里的 `target=clash` **在本版本会 500**。Adapter 必须做**显式白名单映射**，不得透传。

### 3.3 参数传递位置

- **全部通过 query string**（`req.query`），不是 header 也不是 POST body。
- 无 body；`/download/*` 只注册了 **GET**（源码 `$app.get(...)`）。
- 超长订阅通过 `url` 传递时受 URL 长度限制，源码中通过 `#insecure` 等后缀拼接（`download.js:632-667`）说明其设计预期是"短 URL + 服务端存储"，而不是把大段内容塞进 query。

---

## 4. 实测行为

### 4.1 错误响应格式（统一 JSON）

```json
{"status":"failed","error":{"code":"RESOURCE_NOT_FOUND","type":"ResourceNotFoundError","message":"Subscription sub does not exist!"}}
```

| 场景 | HTTP | `error.code` |
|---|---|---|
| 订阅名不存在 | **404** | `RESOURCE_NOT_FOUND` |
| 非法 target | **500** | `INTERNAL_SERVER_ERROR`（`details: "Reason: Target platform: clash is not supported!"`） |
| 分享路由使用 `url`/`content`/`mergeSources` | **400** | `UNSUPPORTED_SHARE_SUB_SOURCE_OVERRIDE` / `UNSUPPORTED_SHARE_SUB_MERGE_SOURCES` |
| 分享路由使用 `fakeSub` | **400** | `UNSUPPORTED_SHARE_FAKE_SUB` |

**错误响应也是 `text/plain`**，但 body 是 JSON——Adapter 解析时不能只看 `Content-Type`。

### 4.2 成功响应形态

```http
HTTP/1.1 200 OK
Content-Type: text/plain; charset=utf-8

proxies:
  - {"type":"ss","skip-cert-verify":false,"udp":false,"server":"1.2.3.4","port":8388,"cipher":"aes-128-gcm","password":"password","name":"🇭🇰HK-01"}
  - {"name":"US-01","type":"vmess","server":"5.6.7.8","port":443,"cipher":"auto","uuid":"1111...","alterId":0,"tls":true,"network":"ws","ws-opts":{"path":"/ws","headers":{"Host":"cdn.example.com"}},"udp":true,"servername":"sni.example.com"}
```

- **不是 base64**，不是完整配置，就是 YAML 的 `proxies:` 列表段。
- 无缓存相关 header、无订阅信息 header。
- 节点名会被 Sub-Store 的 emoji/rename 处理器改写（本例保留了源节点名）。

### 4.3 写入路径（无认证）

```http
POST /api/subs
Content-Type: application/json

{"name":"agenttmp","url":"http://127.0.0.1:13002/sub"}

→ 201 {"status":"success","data":{"name":"agenttmp","url":"http://127.0.0.1:13002/sub"}}
```

**无任何凭据即可创建并落盘。** 这与 R05 的独立实测完全一致。

---

## 5. 公开可依赖 vs 内部实现（边界表）

| 对象 | 判定 | 依据 |
|---|---|---|
| `GET /download/:name[/:target]` | **可依赖（公开）** | 源码路由 + 实测 200 |
| `GET /download/collection/:name` | 可依赖（公开） | 源码 |
| `GET /share/sub/:name`、`/share/col/:name` | 可依赖（有明确业务语义与限制） | 源码 400 分支 |
| `GET /download/sub?url=...` | **不存在（误传）** | 实测 404；wiki 无记录 |
| `target` / `platform` 合法值 | **可依赖但必须白名单映射** | 源码注册表 + 实测 500 |
| `POST /api/subs`（写入订阅） | **存在但属"内部/前端 API"** —— 无版本承诺、无认证、字段可随前端演进 | 源码 `src/restful/subscriptions.js` |
| `/api/preview/sub`、`/api/preview/file` | 内部（POST + 复杂 body，面向前端预览） | 源码 `preview.js` |
| `POST /api/token`、`tokens` 字段 | 内部（分享 token，非认证） | 源码 `token.js` |
| `sub-store.json` 数据结构、`root.json` | **内部实现，绝对禁止依赖** | 实测数据文件 |
| `X-Powered-By: Sub-Store` | 偶然可见，不可依赖 | 实测 header |
| `subscription-userinfo` 等订阅头 | **不存在于稳定响应** | 实测无该 header |

---

## 6. 输出是否为完整 Mihomo Config（关键结论）

**否。** 实测输出只有 `proxies:` 段（433 B），不含：

```text
mixed-port / port / socks-port / redir-port / tproxy-port
external-controller / external-controller-unix / secret
dns: / tun: / rules: / rule-providers: / proxy-groups: / proxy-providers:
```

**架构含义**（必须回写设计文档）：

```text
Sub-Store            Agent 必须自己做
─────────────────    ──────────────────────────────────────────
proxies 列表    →    补全运行参数（端口/controller/secret/CORS 白名单）
                    注入 dns / tun（按能力检测结果决定是否启用）
                    生成 proxy-groups（Sub-Store 不产生 groups！）
                    生成 rules / rule-providers
                    版本化 + checksum + 激活 + reload + 健康检查
```

特别注意：**Sub-Store 的 Clash/Mihomo producer 只产出 proxies**，因此在我们的流水线里"订阅转换"的输出是**中间产物**，而不是最终配置。设计文档 §16 的 `Generated Config` 步骤**必须由 Agent 实现**，不能假设 Sub-Store 给出完整配置。

---

## 7. 建议的 Converter Port 契约与 Adapter 边界

### 7.1 Port（保持与设计文档 §10 一致）

```rust
#[async_trait]
pub trait SubscriptionConverter: Send + Sync {
    /// 把订阅源转换为 Mihomo 可用的节点列表（中间产物）
    /// 注：最终命名已收紧为 `ConvertedProxies`（见 ADR-002 D4 / docs/design/domain-ports-bootstrap.md §3.4），
    /// 因为上游只返回 proxies 段，不是完整可运行订阅。
    async fn convert(&self, request: ConvertRequest) -> Result<ConvertedProxies>;

    /// 能力探测：报告该实现支持的 target 与限制
    async fn capabilities(&self) -> Result<ConverterCapabilities>;
}
```

`ConvertRequest` 只表达业务意图（订阅源、UA、代理、目标格式枚举 `TargetFormat::Mihomo`），**不含任何 Sub-Store 参数名**。

### 7.2 Adapter 内部（仅 `SubStoreConverter` 知道）

```text
1) 幂等写入：POST <base>/api/subs  (name = 由订阅 ID 派生的稳定名，如 "proxy-agent-<sub_id>")
2) 下载转换：GET  <base>/download/<name>?target=<白名单映射>&url=<源>&ua=<UA>&noCache=1
3) 解析：以 YAML 解析 proxies 段；非 2xx 或 body 为 JSON error → 映射为错误
4) 可选前缀：base_url 可含 BACKEND PREFIX（如 http://127.0.0.1:3001/secretpath）
```

**Adapter 独占的知识**（禁止泄漏到 Application）：端点为 `/download/:name`、`target` 白名单、`url/ua/content/noCache` 参数名、错误 JSON 形状、`/api/subs` 写入细节、`x-powered-by` 等。

### 7.3 错误映射

| Sub-Store 现象 | 映射 |
|---|---|
| 连接失败 / 超时 | `ConverterError::Unreachable`（可重试，**绝不**影响当前配置） |
| 404 `RESOURCE_NOT_FOUND` | `ConverterError::SubscriptionNotFound`（Adapter 应先尝试写入，再下载） |
| 500 `Target platform ... not supported` | `ConverterError::UnsupportedTarget`（**编程错误，应告警而不是重试**） |
| 400 `UNSUPPORTED_*` | `ConverterError::InvalidRequest` |
| 200 但 `proxies` 为空/解析失败 | `ConverterError::EmptyOrInvalidOutput`（**等同于失败，必须保留旧配置**） |
| 200 但 YAML 非法 | `ConverterError::InvalidOutput` |

### 7.4 必须强制的不变量

```text
REQ-SUB-001  application 中不得出现 "download/"、"api/subs"、"target="、"mihomo" 等 Sub-Store 专属字符串
REQ-SUB-003  以上任何错误都不得替换当前激活配置
REQ-SUB-006  切换 Converter 实现（SubStoreConverter → NativeConverter）不改 application
```

---

## 8. 安全与暴露面

1. **后端 API 无认证** `[实测]`：`POST /api/subs` 无凭据成功 → **绝不能把 Sub-Store 后端暴露到非 loopback**。R05 实测其源码默认绑定 `::`（所有接口），必须显式设为 `127.0.0.1`。
2. **`BACKEND PREFIX` 不是认证** `[实测]`：它只把路由挂到随机路径下（防扫描），拿到前缀即可完全访问，包括写入订阅。
3. **SSRF 面**：订阅拉取由 **Sub-Store 进程**发起（`url=` 参数），Agent 无法控制其出站目标 → 若需要 SSRF 防护，必须由 Agent 侧校验 URL，或要求 Sub-Store 与本机网络策略配合（关联 open-questions Q012）。
4. **凭据泄漏面**：订阅 URL 常含 token。`/api/utils/env` 会回显全部 `SUB_STORE_*` 环境变量与部署路径（R05 实测），**不得整体记录日志**。
5. **依赖最小化**：`GET /download/:name` 的响应头带 `X-Powered-By: Sub-Store`，可作为版本指纹，但不应写进日志的敏感字段。
6. **进程隔离**：Sub-Store 为 AGPL-3.0，必须保持独立进程/容器，仅通过 HTTP 调用（见 R13、ADR-002）。

---

## 9. 证据与来源

### 9.1 实测命令与结果（本机，2026-09-12）

```bash
# 1) 误传接口 → 404
curl "http://127.0.0.1:13001/download/sub?target=mihomo&url=http://127.0.0.1:13002/sub"
# → 404 {"status":"failed","error":{"code":"RESOURCE_NOT_FOUND",...,"message":"Subscription sub does not exist!"}}

# 2) 写入订阅（无认证）
curl -X POST http://127.0.0.1:13001/api/subs -H 'Content-Type: application/json' \
     -d '{"name":"agenttmp","url":"http://127.0.0.1:13002/sub"}'
# → 201 {"status":"success",...}

# 3) 命名路由下载 → 200，仅 proxies
curl -D - "http://127.0.0.1:13001/download/agenttmp?target=mihomo"
# → 200, Content-Type: text/plain; charset=utf-8, 433 B, 仅 "proxies:"

# 4) url 覆盖有效
curl "http://127.0.0.1:13001/download/agenttmp?target=mihomo&url=http://127.0.0.1:13002/sub"
# → 200, 433 B

# 5) 非法 target
curl "http://127.0.0.1:13001/download/agenttmp?target=clash"
# → 500 {"status":"failed","error":{"code":"INTERNAL_SERVER_ERROR",...,"details":"Reason: Target platform: clash is not supported!"}}
```

### 9.2 上游源码

- `sub-store-org/Sub-Store`，`backend/src/restful/download.js`（路由注册 L45-83；参数解构 L106-119；share 路由限制 L160-190；url 处理 L632-667）
- `backend/src/core/proxy-utils/producers/index.js`（target 注册表）
- `backend/src/restful/preview.js`（`/api/preview/*` 属内部 API）
- `backend/src/restful/subscriptions.js`（`/api/sub/*`）
- 抓取方式：`raw.githubusercontent.com` 直连（`github.com` 直连超时）；本地 `upstream/backend` 为源码工作副本

### 9.3 遗留证据文件

- `/tmp/r04-substore/r04-final-mihomo.txt`、`r04-final-headers.txt`、`r04-final-create.json`
- `/tmp/r04-substore/server.log`、`server2.log`、`server3.log`、`server4.log`
- `/tmp/r04-substore/out_*.txt`（原调研子任务对 `target=clash/ClashMeta/mihomo/...` 的早期探测，全部因把 `sub` 当订阅名而 404 —— 该批结果正是"`/download/sub` 不是真接口"的旁证）

---

## 10. 未验证假设与开放问题

| # | 问题 | 影响 | 验证方法 |
|---|---|---|---|
| O1 | `/download/:name` 是否在 Sub-Store 官方 wiki 有兼容性承诺？ | 决定 Adapter 是否需要版本探测 | 抓 wiki（本次 `wiki.tar.gz` 中未检索到 `download/` 相关文档，疑为前端文档站） |
| O2 | `POST /api/subs` 的字段契约稳定性 | 写入路径可能随前端演进破坏 | 跟踪上游 release note；考虑改为"用户自行在 Sub-Store 建订阅，Agent 只读"的保守模式 |
| O3 | 大订阅（数千节点）的输出大小与耗时 | 影响超时与内存策略 | 需在真实订阅上实测 |
| O4 | 缓存行为（`noCache` 是否真绕过、缓存 TTL） | 影响"订阅更新是否即时生效" | 连续请求观察结果是否变化 |
| O5 | ~~`content=` 内联内容的编码契约（base64? raw?）~~ **✅ 已答（2026-09-12 实测）** | ~~决定是否能完全避开先入库~~ → **结论：不能避开；`content=` 只覆盖远端抓取。编码契约为 raw，base64 不解码** | 已实测，见 Q023 |
| O6 | BACKEND PREFIX 与 `/download/:name` 的路径拼接规则 | 影响 Adapter URL 构造 | 已在实测中观察到 `[BACKEND PREFIX] 127.0.0.1:13010/secretpath`，但未验证 `/download` 是否也在前缀下 |
| O7 | Docker 路径下的行为差异 | 部署模板 | Docker Hub 不可达，未实测（R05 同样受限） |
| O8 | producer 输出的 `proxy-groups` 是否真的从不生成 | 若某些 target 会生成 groups，则需按 target 分别处理 | 遍历全部 target 实测（本次仅验证 Clash/ClashMeta/mihomo） |

> **对设计文档的回写建议**：§12 与 §43 中 `GET /download/sub?target=ClashMeta&url=...` 的示例必须改为两段式（`POST /api/subs` + `GET /download/:name?target=ClashMeta`），并明确"Sub-Store 只产出 proxies 段，完整配置由 Agent 生成"。
