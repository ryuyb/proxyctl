# R01 — Mihomo 能力与 Controller API

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：实测 + 上游源码 + 上游文档
> 关键结论一句话：Mihomo Controller 已提供完整的 runtime 读/写/流式能力（约 45 个 route），Agent **不应重复实现**代理内核与 Dashboard 逻辑，只需把其中**稳定子集**封装成 `MihomoController` Port；但 `external-controller-unix` **不校验 secret 且 socket 默认 0666**，是必须在部署层解决的安全红线。

---

## 1. 结论摘要（TL;DR）

1. **实测版本**：`Mihomo Meta v1.19.30 darwin arm64 with go1.26.6`（`with_gvisor`），来自 GitHub release tag [`v1.19.30`](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30)。`/version` 返回 `{"meta":true,"version":"v1.19.30"}`，`meta=true` 可用于 agent 做 **Meta 内核识别**。[实测]

2. **route 清单已冻结且与源码一致**：实测 route 覆盖与 tag-pinned 源码 `hub/route/server.go` 的 `r.Mount(...)` 列表**逐条吻合**。任务清单里点名的 `/script` 与 `/profile` 在 v1.19.30 中**并不存在**（实测 `404`），属于过时/误传接口。[实测][上游源码]

3. **不存在"部分可用"的鉴权**：一旦设置了 `secret`，TCP controller 上**所有** route 都要求 `Authorization: Bearer <secret>`，包括 `/version`（无 secret 时 `401 {"message":"Unauthorized"}`）。[实测]

4. **Unix socket 是"无鉴权 + 权限即安全边界"**：`external-controller-unix` 在设置了 `secret` 的情况下**依然完全跳过鉴权**（无 header / 正确 Bearer / 错误 Bearer 三种情况均返回 `200`），且 socket 文件权限为 `srw-rw-rw-`（**0666**）。官方文档明确承认这一点。**这意味着凡是能访问该 socket 文件的本机进程都拥有完整内核控制权（含 `/upgrade`、`/restart`）**。[实测][上游文档]

5. **流式接口是 NDJSON / WebSocket 双模**：`/traffic`、`/memory`、`/connections`、`/logs` 在不带 `Upgrade: websocket` 时以 **HTTP chunked + 每秒一行 JSON** 推送（实测 `/traffic` 3 秒收 2 行）；带 WS header 时返回 `101 Switching Protocols` 并推 WS 帧。[实测]

6. **reload 是"先解析、后应用"的原子语义**：`PUT /configs` 先 `ParseWithBytes/ParseWithPath`，解析失败返回 `400` 且**不影响当前运行实例**；实测发送非法 YAML 后实例仍 `200` 存活且 `mode`/`mixed-port` 不变。但**空 body 会 `400 Body invalid`**，必须发 `{}` 或带 `payload`/`path` 的 JSON（官方示例含 `{}` 形式，与此一致）。[实测][上游源码]

7. **存在路径白名单**：`PUT /configs` 的 `path` 必须是绝对路径且在 home 或 `SAFE_PATHS` 内，否则 `400`，报错会回显允许路径（实测 `allowed paths: [/tmp/r01-mihomo]`）。[实测]

8. **`/restart` 是真正的进程自替换**，不是 reload：源码用 `syscall.Exec(execPath, os.Args, os.Environ())`（Linux/macOS 原地替换进程映像，PID 不变）；实测返回 `200 {"status":"ok"}` 后实例完成 `executor.Shutdown()` 并重建全部 listener。返回值与官方文档写的 `204` **不一致**（文档有误）。[实测][上游源码]

9. **`/upgrade` 由上游保管升级逻辑，但 Agent 不应依赖它**：`POST /upgrade?force=false` 实测返回 `500 {"message":"update error: already using latest version v1.19.30"}`，binary 哈希未变。它依赖 GitHub 可达，且在受限网络下 `force=true` 存在把自身替换成半截二进制的风险。**Agent 应走"自行下载 + 校验 + 原子替换"路径**。[实测][上游源码]

10. **`/configs/geo` 与 `/upgrade/geo` 是 fire-and-forget**：实测 `POST /configs/geo` 立即返回 `204`，但日志显示下载随后仍在进行中（`[GEO] Updating GEO database`），且**无 geodata 文件落盘、无失败回传**。调用方无法从这个 204 判断成功。[实测][上游源码]

---

## 2. 实测环境与版本

### 2.1 宿主与工具

| 项 | 值 | 证据 |
|---|---|---|
| 宿主 | macOS Darwin 25.6.0 arm64（Apple Silicon） | [实测] |
| 内核版本 | Darwin Kernel Version 25.6.0 | [实测] |
| Docker | daemon **不可用**（OrbStack socket 缺失）→ **容器实验全部跳过** | [实测] |
| 网络 | `github.com` 直连超时；release asset 经 `https://ghfast.top/` 代理获取；`raw.githubusercontent.com` 与 `wiki.metacubex.one` 直连可用 | [实测] |

> ⚠️ **平台差异声明**：本次实测在 **darwin/arm64** 上完成。目标平台是 **Linux + PVE LXC**。route 层与 controller 语义是平台无关的 Go 代码，可跨平台信任；但 **TUN / nftables / auto-redirect / `external-controller-routing-mark` 等 Linux 专属能力本次完全未验证**，标注为 `[未验证]`（详见 §8）。

### 2.2 二进制

```text
$ ./mihomo -v
Mihomo Meta v1.19.30 darwin arm64 with go1.26.6 Sun Aug 16 10:01:05 UTC 2026
Use tags: with_gvisor
```

- Release tag：[`v1.19.30`](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30)（published 2026-08-16T10:11:34Z）
- 资产名：`mihomo-darwin-arm64-v1.19.30.gz`
- 源码基线：`raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.30/hub/route/*.go`（tag-pinned，与 `Alpha` 分支本次抽样一致）

### 2.3 CLI 参数（`mihomo -h`）[实测]

```text
-age-secret-key string   specify age secret key to decrypt configuration
-config string           specify base64-encoded configuration string
-d string                set configuration directory
-ext-ctl string          override external controller address
-ext-ctl-pipe string     override external controller pipe address
-ext-ctl-routing-mark int  override external controller routing mark
-ext-ctl-tls string      override external controller tls address
-ext-ctl-unix string     override external controller unix address
-ext-ui string           override external ui directory
-f string                specify configuration file
-m                       set geodata mode
-post-down string        set post-down script
-post-up string          set post-up script
-secret string           override secret for RESTful API
-t                       test configuration and exit
-v                       show current version of mihomo
```

> 对 Agent 的意义：`-t` 提供**离线配置校验**能力，`-d` 分离工作目录，`-ext-ctl-unix` / `-ext-ctl` / `-secret` 允许 Agent 在**不改写用户 YAML** 的前提下覆盖 controller 监听，是实现"配置版本化 + 原子激活"的重要配合点。[实测]

### 2.4 实验配置（无 TUN、无 proxy-provider、仅回环）

```yaml
mixed-port: 28890
allow-lan: false
bind-address: 127.0.0.1
mode: rule
log-level: info
ipv6: false
external-controller: 127.0.0.1:29190
external-controller-unix: /tmp/r01-mihomo/r01.sock
secret: "r01-test-secret"
dns:
  enable: true
  listen: 127.0.0.1:25353
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  nameserver: [1.1.1.1, 8.8.8.8]
proxies: []
proxy-groups: []
rules:
  - MATCH,DIRECT
```

- `mihomo -t` 校验通过：`configuration file ... test is successful`。[实测]
- 端口选择刻意避开宿主已有代理软件；实验期间发现 **19090/17890 已被宿主 Clash Party 占用**，因此改用 29190/28890/25353。此过程也验证了 mihomo **会记录 `bind: address already in use` 并继续启动**（部分 listener 失败不致命）。[实测]

---

## 3. Controller API 完整清单（实测）

### 3.1 鉴权行为矩阵

| 访问方式 | 无 secret header | 正确 Bearer | 错误 Bearer | 证据 |
|---|---|---|---|---|
| TCP `127.0.0.1:29190` | `401 {"message":"Unauthorized"}` | `200` | `401` | [实测] |
| Unix socket `r01.sock` | **`200`** | `200` | **`200`** | [实测] |
| Unix socket 文件权限 | `srw-rw-rw-`（0666） | — | — | [实测] |

路由级例外（源码）：WebSocket 请求允许用 `?token=<secret>` 代替 header（"Browser websocket not support custom header"）。TCP 下 `Authorization` 的 scheme 必须**精确**为 `Bearer`（区分大小写），实测 `bearer ...` / 裸 secret 均 `401`。[实测][上游源码 `hub/route/server.go`]

### 3.2 完整 route inventory（实测状态码 + 源码方法）

`A` = 需要鉴权（TCP 下）。所有路径均可经 Unix socket 无鉴权访问。

| # | Method | Path | 实测 status | 顶层响应字段 / 语义 | A | 证据 |
|---|---|---|---|---|---|---|
| 1 | GET | `/` | 200 | `{hello:"mihomo"}` | ✔ | [实测] |
| 2 | GET | `/version` | 200 | `meta`, `version` | ✔ | [实测] |
| 3 | GET | `/configs` | 200 | 33 个字段，见 §3.3 | ✔ | [实测] |
| 4 | PUT | `/configs` | 204 / 400 | reload；body `{}` 或 `{path,payload}`；`?force=true` 可选 | ✔ | [实测] |
| 5 | PATCH | `/configs` | 204 | 热改运行参数（实测 `log-level`、`mode` 生效） | ✔ | [实测] |
| 6 | POST | `/configs/geo` | 204 | fire-and-forget，无错误回传 | ✔ | [实测] |
| 7 | GET | `/configs/geo` | **405** | 仅 POST | ✔ | [实测] |
| 8 | GET | `/proxies` | 200 | `proxies`（map: name → proxy） | ✔ | [实测] |
| 9 | GET | `/proxies/:name` | 200 | 见 §3.4 字段表 | ✔ | [实测] |
| 10 | PUT | `/proxies/:name` | 204 | body `{"name":"DIRECT"}` 选择节点 | ✔ | [实测] |
| 11 | DELETE | `/proxies/:name` | 400（空 body） | 清除 fixed 选择；需 body | ✔ | [实测] |
| 12 | GET | `/proxies/:name/delay` | 200 / 504 | `?url=&timeout=`；超时返回 `504 {"message":"Timeout"}` | ✔ | [实测] |
| 13 | GET | `/group` | 200 | `proxies` | ✔ | [实测] |
| 14 | GET | `/group/:name` | 200 | 同单个 group 对象 | ✔ | [实测] |
| 15 | GET | `/group/:name/delay` | 200 / 504 | 超时返回 `504 {"message":"get delay: all proxies timeout"}` | ✔ | [实测] |
| 16 | GET | `/connections` | 200 | `downloadTotal`, `uploadTotal`, `connections`, `memory`；`?interval=` 控制推送间隔 | ✔ | [实测] |
| 17 | GET/WS | `/connections` | 200 / **101** | 无 Upgrade 时 NDJSON 流 | ✔ | [实测] |
| 18 | DELETE | `/connections` | 204 | 关闭全部连接 | ✔ | [实测] |
| 19 | DELETE | `/connections/:id` | 204 | **不存在的 id 也返回 204**（无幂等反馈） | ✔ | [实测] |
| 20 | GET | `/traffic` | 200 / **101** | `{up,down,upTotal,downTotal}`，1Hz | ✔ | [实测] |
| 21 | GET | `/memory` | 200 / **101** | `{inuse,oslimit}`，1Hz（`oslimit` 恒为 0） | ✔ | [实测] |
| 22 | GET | `/logs` | 200 / **101** | `?level=`、`?format=structured`、`?token=`（WS） | ✔ | [实测] |
| 23 | GET | `/rules` | 200 | `rules[]`：`index,type,payload,proxy,size,extra{disabled,hitCount,hitAt,missCount,missAt}` | ✔ | [实测] |
| 24 | PATCH | `/rules/disable` | 204 / 400 | body 为 **int→bool 映射** `{"0":true}`；用字符串 key 会 `400 Body invalid` | ✔ | [实测] |
| 25 | GET | `/providers/proxies` | 200 | `providers` | ✔ | [实测] |
| 26 | GET | `/providers/proxies/:name` | 200 | provider 对象 + `proxies[]` | ✔ | [实测] |
| 27 | PUT | `/providers/proxies/:name` | — | 触发 provider 更新 | ✔ | [上游源码] |
| 28 | GET | `/providers/proxies/:name/healthcheck` | 204 | 触发健康检查 | ✔ | [实测] |
| 29 | GET | `/providers/proxies/:name/:proxy` | — | 单个 provider 内 proxy | ✔ | [实测 404 于不存在路径] |
| 30 | GET | `/providers/proxies/:name/:proxy/healthcheck` | 200 / 504 | 延迟测试 | ✔ | [上游源码] |
| 31 | GET | `/providers/rules` | 200 | `providers` | ✔ | [实测] |
| 32 | PUT | `/providers/rules/:name` | — | 更新 rule provider | ✔ | [上游源码] |
| 33 | POST | `/cache/fakeip/flush` | 204 | 清 fake-ip 池 | ✔ | [实测] |
| 34 | POST | `/cache/dns/flush` | 204 | 清 DNS 缓存 | ✔ | [实测] |
| 35 | GET | `/dns/query` | 200 | `?name=&type=`；返回 DNS wire 风格 `{AD,Answer[{TTL,data,name,type}]}` | ✔ | [实测] |
| 36 | GET | `/storage/:key` | 200 | 值或 `null` | ✔ | [实测] |
| 37 | PUT | `/storage/:key` | 204 | 写 KV | ✔ | [实测] |
| 38 | DELETE | `/storage/:key` | 204 | 删 KV | ✔ | [实测] |
| 39 | POST | `/restart` | **200** `{"status":"ok"}` | `syscall.Exec` 自替换；文档写 204（**文档有误**） | ✔ | [实测] |
| 40 | POST | `/upgrade` | 200 / 500 | `?channel=&force=`；需 GitHub 可达 | ✔ | [实测] |
| 41 | POST | `/upgrade/ui` | 200 / 500 | 下载 external-ui | ✔ | [实测 超时] |
| 42 | POST | `/upgrade/geo` | 204 | fire-and-forget | ✔ | [实测] |
| 43 | GET | `/ui`, `/ui/*` | — | 仅当 `external-ui` 已配置 | ✔ | [上游源码] |
| 44 | GET | `/debug/pprof/*`, `PUT /debug/gc` | — | **仅 `-d`/debug 模式** | ✖/✔ | [上游源码] |
| 45 | ANY | `external-doh-server` 路径 | — | 仅当配置；**不校验 secret** | ✖ | [上游文档] |
| — | GET | `/script` | **404** | **不存在** | — | [实测] |
| — | GET | `/profile` | **404** | **不存在** | — | [实测] |
| — | GET | `/definitely-not-a-route` | 404 | 路由不存在 | — | [实测] |

### 3.3 `GET /configs` 实测返回字段（33 个）

```text
port, socks-port, redir-port, tproxy-port, mixed-port,
tun, tuic-server, ss-config, vmess-config,
authentication, skip-auth-prefixes, lan-allowed-ips, lan-disallowed-ips,
allow-lan, bind-address, inbound-tfo, inbound-mptcp,
mode, unified-delay, log-level, ipv6,
interface-name, routing-mark,
geox-url, geo-auto-update, geo-update-interval, geodata-mode,
geodata-loader, geosite-matcher,
tcp-concurrent, find-process-mode, sniffing,
global-ua, etag-support,
keep-alive-idle, keep-alive-interval, disable-keep-alive
```

对照源码，该响应等于 `config.General` 结构体。**关键缺口：`/configs` 不返回 `dns`、`proxies`、`proxy-groups`、`rules`、`proxy-providers`、`rule-providers`、`tunnels`、`listeners`、`hosts`、`ntp`、`experimental`、`profile`、`sniffer`、`tls`。** 这些必须从配置文件或各自专用接口获取。[实测][上游源码 `config/config.go`]

实测 `tun` 子对象为 `{"enable":false,...}`，包含 `device/stack/dns-hijack/auto-route/auto-detect-interface/mtu/gso/...` 及大量 Linux 专属字段（`iproute2-table-index`、`auto-redirect`、`include-uid`、`route-exclude-address`、`file-descriptor` 等）。`/configs` 的 `tun.enable` 可作为 **TUN 是否实际生效**的读点，但**它只反映配置意图，不代表内核能力可用**。[实测][上游源码 `hub/route/configs.go`]

### 3.3.1 Linux 复核与线格式细节（2026-09-12，linux arm64）

在 Debian（mihomo v1.19.30 linux arm64）上复核，**R01 的 macOS 结论全部成立**，并补充
了几条实现适配器时必须知道的**线格式细节**（`[实测]`）：

| 端点 | 方法/参数 | 实测结果 |
|---|---|---|
| `/configs` | `PUT` + 空 body | **400**（确认不能发空 body） |
| `/configs` | `PUT` + `{}` | **204** |
| `/configs` | `PUT` + `{"payload": "<yaml>"}` | **204**，payload 字段名确认 |
| `/configs` | `PUT` + `{"payload": "<非法 yaml>"}` | **400** + 明确 message；**实例仍 200 存活** |
| `/configs` | `?force=true` | **204**（上游接受该参数 → Agent 必须自己不带） |
| `/version` | `GET` | `{"meta":true,"version":"v1.19.30"}` |
| `/version` | 无 secret | **401** |

**线格式细节（手写解析器必须知道）**：

```text
GET /proxies  →  {"proxies": { "<name>": {...}, ... }}     ← 嵌套在 "proxies" 键下，非裸数组
GET /rules    →  {"rules":   [ {"index":0,"type":"Match","payload":"","proxy":"DIRECT",...} ]}
```

`/proxies` 是**以名字为键的对象**，不是数组；`/rules` 是数组但**包在 `rules` 键下**。
两者都不是裸数组 —— 按裸数组解析会静默得到空结果。

`/configs` 的端口字段在未配置时为 **`0` 而非 null**（`"port":0`），因此"端口是否存在"
必须判 `!= 0`，不能判 `is_some`。`tun` 是子对象，`tun.enable` 只反映**配置意图**，
不代表内核能力可用（与 3.3 结论一致）。

### 3.3.2 观测面（`/logs` `/traffic` `/memory` `/connections`）线格式实测（2026-09-12，linux arm64）

在 Debian（mihomo v1.19.30 linux arm64）上逐一实测四个观测端点，**证据来自本次运行**。
这些结论直接决定 `MihomoObserver` 适配器的形态，故单独成节。

**全部四个端点都是 chunked 流，永不自行结束**（`[实测]`）：

| 端点 | 响应头 | 结束行为 |
|---|---|---|
| `/traffic` | `200` + `Transfer-Encoding: chunked` | 8s 内不结束，需外部超时 |
| `/memory` | `200` + `Transfer-Encoding: chunked` | 同上 |
| `/connections` | `200` + `Transfer-Encoding: chunked` | 同上 |
| `/logs` | **连接后不立即返回响应头** | 直到第一条日志出现才刷头，随后持续推送 |

两条对解析器有实际影响的结论：

1. **`Transfer-Encoding: chunked`，且现有 `send` 不处理分块编码。**
   Agent 的 `Transport::send` 现在用 `read_to_end` 再解析裸 body；对 chunked 流它既
   等不到结尾，也拿不到正确字节。**必须新增一个流式入口**，而不是复用 `send`。
2. **`/logs` 不立即刷响应头。** 因此「connect 成功 ⇒ 可以读头」这个假设**不成立**：
   空闲实例上 `/logs` 会长时间停在「已连接但无任何字节」。适配器必须把
   「连接建立」与「首条数据到达」当作两个独立事件，否则 `proxyctl logs` 在无流量的
   实例上会表现成「卡住」。

**`/logs` 两种格式的实测对比**（同一次订阅，49 行）：

```text
默认格式      {"type":"debug","payload":"[DNS] resolve example.com A from ..."}
structured    {"time":"21:21:55","level":"debug","message":"[DNS] resolve ...","fields":[]}
```

- 默认格式：级别在 **`type`**，消息在 **`payload`**。
- `format=structured`：级别在 **`level`**，消息在 **`message`**，另有 **`fields`** 数组。
- **`structured` 的 `time` 只有 `HH:MM:SS`，没有日期。** 因此时间戳**不能**从该字段
  直接构造绝对时间；`LogEntry.at` 要么留 `None`，要么由适配器补当天日期（并承担跨午夜
  的歧义）。这一点推翻了「structured 字段名更稳定所以无代价」的假设——它稳定，但时间信息
  反而更少。

**级别过滤行为（`[实测]`，此前 R01 仅验证 `level=info`）**：

| `level=` | 实测收到的级别 | 结论 |
|---|---|---|
| `debug` | `debug`, `info` | 含本级别及以下 |
| `info` | `info` | 含本级别，不含 debug |
| `warning` | （无数据触发） | 无法判断，见下 |
| `error` | （无数据触发） | 同上 |
| `silent` | （无数据触发） | 同上 |

`warning`/`error`/`silent` **仍未验证**：本次环境无法稳定产出 warning/error 级日志
（需要构造 listen 冲突或上游失败才能触发）。已在这一侧保留为未验证项。

**`/traffic` 与 `/memory` 的实测字段**：

```text
/traffic  {"up":75,"down":870,"upTotal":2790,"downTotal":19155}     ← 逐秒增量 + 累计
/memory   {"inuse":42098688,"oslimit":0}                            ← inuse 首帧可能为 0
```

`/memory` 的**第一帧是 `{"inuse":0,"oslimit":0}`**（尚未采样），随后才是真实值。适配器
若把首帧直接上报，会得到一个假的「内存占用 0」。`/traffic` 同理：空闲时 `up`/`down`
为 `0` 是真实增量，不是缺失。

**`/connections` 的隐私字段确认存在**（印证 ADR-003 D1 的分拆理由）：

```text
"metadata":{"sourceIP":"127.0.0.1","uid":0,"process":"","processPath":"",
            "sourceIPASN":"","destinationIPASN":"",...}
```

`uid`/`process`/`processPath` 确实随连接记录返回，且外层是 `{"downloadTotal":...,"connections":[...]}`，
**不是裸数组**。

### 3.4 `GET /proxies/:name` 实测字段

- **通用字段**：`name, type, alive, udp, uot, xudp, tfo, mptcp, smux, history[], extra, interface, routing-mark, provider-name, dialer-proxy, hidden, icon, testUrl, emptyFallback`
- **策略组额外**：`now, all[]`
- 实测 `GLOBAL` 组：`{"alive":true,"all":["DIRECT","REJECT"],"now":"DIRECT","emptyFallback":"COMPATIBLE",...}`

### 3.5 `GET /connections` 实测字段（真实流量下抓取）

```
conn:     id, metadata, upload, download, start, chains, providerChains, rule, rulePayload
metadata: network, type, sourceIP, destinationIP, sourceGeoIP, destinationGeoIP,
          sourceIPASN, destinationIPASN, sourcePort, destinationPort,
          inboundIP, inboundPort, inboundName, inboundUser, rematchName,
          host, dnsMode, uid, process, processPath,
          specialProxy, specialRules, remoteDestination, dscp, sniffHost
```

> `metadata` 含 `uid` / `process` / `processPath` / `specialProxy` —— **这是隐私面较大的数据**（能暴露本机哪个进程访问了什么）。Agent 若透传给 Web UI 必须做权限与脱敏设计。[实测]

### 3.6 文档未覆盖的部分（明确标注）

| 项 | 状态 |
|---|---|
| `/rules/disable` 的 payload 是 **int index 映射**而非规则文本 | [实测]；官方文档示例 `{"0":false}` 一致 |
| `/rules/disable` **重启后重置**（临时操作） | [上游文档]（未做重启后复测） |
| `/connections/:id` 对不存在 id 返回 204 | [实测] |
| `/upgrade` 无可用更新时返回 **500 + "already using latest version"** | [实测] |
| `/restart` 返回 200 而非文档所述 204 | [实测] |
| `/configs/geo` 204 不代表更新成功（无落盘、无错误） | [实测] |
| PATCH `/configs` 支持哪些字段 | 源码 `configSchema` 定义约 25 个可写字段（port/.../tun/tuic-server/mode/log-level 等）[上游源码] |

---

## 4. 能力矩阵

`Mihomo API` 列给出**实测端点**；`稳定性` 列见 §5。

| 能力 | Mihomo API | 端点 | Agent 应该做什么 |
|---|---|---|---|
| 内核版本识别 | `/version`（`meta` 标识 Meta 内核） | `GET /version` | **直接封装** → `MihomoController::version()`。`meta=false` 必须拒绝启动（非 Meta 内核语义不同） |
| 运行配置读取 | `/configs`（33 字段，不含 dns/rules/proxies） | `GET /configs` | **直接封装**，但只取 `mode/log-level/ports/tun/allow-lan` 等运行态字段。**禁止**把 `/configs` 当作"完整配置真相"，完整真相 = Agent 自己的 ConfigVersion 文件 |
| 运行参数热改 | PATCH `/configs` | `PATCH /configs` | **薄封装**，仅暴露白名单字段（`mode`、`log-level`）。不要暴露 `tun`/端口等危险字段 |
| 配置重载/激活 | `PUT /configs`（先解析后应用，失败不改现状） | `PUT /configs` | **直接封装 + 自研外层**：Agent 负责版本化写入、校验（`-t`）、原子 rename、健康检查、失败回滚。**不要自己实现 YAML→runtime 的应用逻辑** |
| 代理/策略组枚举 | `/proxies`、`/group` | `GET /proxies`,`GET /group` | **直接封装** → `proxies()` / `groups()` |
| 策略组选点 | `PUT /proxies/:name` | `PUT /proxies/:name` | **直接封装** → `select_proxy(group, name)`。注意 `Selector` 与 `URLTest/Fallback` 语义不同 |
| 清除 fixed 选择 | `DELETE /proxies/:name` | `DELETE /proxies/:name` | **薄封装**，需带 body；对 Selector 无效（按文档语义） |
| 节点延迟测试 | `/proxies/:name/delay`、`/group/:name/delay` | 同左 | **直接封装** → `test_delay()`。必须透传 `url`/`timeout`/`expected`，并把 `504` 归一化为"全部超时"业务结果而非基础设施错误 |
| Provider 管理 | `/providers/proxies*`、`/providers/rules*` | 同左 | **薄封装**（`PUT` 触发更新 + `GET healthcheck`）。provider 订阅的**内容生成**属于 Agent/Sub-Store，不属于这里 |
| 连接观测 | `/connections` | `GET/WS /connections` | **薄封装**，只做数据搬运与权限控制。**不要把 connections 建模成 Domain 实体**（见 §6） |
| 关闭连接 | `DELETE /connections`、`/connections/:id` | 同左 | **不重复实现**（无内核状态可自维护）；作为显式运维 Use Case 暴露 |
| 实时流量 | `/traffic` | `GET/WS /traffic` | **薄封装** → 转为 Agent 事件（`TrafficUpdated`）。**不要自研流量统计** |
| 内存观测 | `/memory` | `GET/WS /memory` | **薄封装**，仅用于 Doctor/监控展示 |
| 实时日志 | `/logs` | `GET/WS /logs` | **薄封装** → 事件流。**不要自研日志采集**；必须做敏感信息脱敏（见 §6） |
| 规则枚举 | `/rules` | `GET /rules` | **直接封装**，只读展示 |
| 规则临时禁用 | `/rules/disable` | `PATCH /rules/disable` | **薄封装**；注意是**临时**操作、重启丢失，UI 必须明示 |
| DNS 查询诊断 | `/dns/query` | `GET /dns/query` | **直接封装** → Doctor 的 DNS 诊断项 |
| fake-ip / DNS 缓存清理 | `/cache/fakeip/flush`、`/cache/dns/flush` | 同左 | **直接封装**，作为显式运维 Use Case |
| KV 存储 | `/storage/:key` | `GET/PUT/DELETE /storage/:key` | **不重复实现，也基本不用**；Agent 自己的状态应进 SQLite/文件，不要占用内核 KV |
| 内核自重启 | `/restart`（`syscall.Exec`） | `POST /restart` | **慎用**。Agent 的生命周期权威应是 systemd/ProcessManager；`/restart` 会绕过 Agent 的状态机（PID 不变）→ 如需重启，走 Agent 自己 stop/start |
| 内核自升级 | `/upgrade` | `POST /upgrade` | **不重复实现，也不应依赖**。Agent 必须自研"下载 + 校验 + 原子替换"，理由见 §5 |
| UI 自升级 | `/upgrade/ui` | `POST /upgrade/ui` | **不重复实现**（metacubexd 由 Agent 管理部署更可控） |
| GEO 数据库更新 | `/configs/geo`、`/upgrade/geo` | `POST` | **不依赖**（204 不保证成功）；Agent 若需更新 GEO，应自管下载与校验 |
| Dashboard（含 WS） | `/ui/*` + 全部 WS 端点 | — | **不重复实现**（项目范围明确排除完整 Dashboard） |
| 配置语法校验 | CLI `-t` | 非 API | **必须自研封装**（离线校验，先于 `/configs` PUT） |
| 内核二进制管理 | 无 API | 非 API | **必须自研**（版本、下载、校验、systemd 集成） |
| 订阅抓取/转换 | 无 API | 非 API | **必须自研**（Sub-Store / sub-store-convert / Native） |
| 配置版本化/回滚 | 无 API | 非 API | **必须自研**（Agent 核心价值） |
| 系统能力检测（TUN/CAP/等） | 无 API | 非 API | **必须自研**（Doctor，见 R09） |
| 进程生命周期 | 无 API | 非 API | **必须自研**（systemd / ProcessManager Port） |

---

## 5. 稳定性与风险（哪些 API 不稳定/实验性）

### 5.1 可直接视为稳定契约（建议封装）

依据：有官方文档条目、本次实测通过、且语义自洽。

`GET /version`、`GET /configs`、`PATCH /configs`（白名单字段）、`PUT /configs`、`GET /proxies`、`GET /proxies/:name`、`PUT /proxies/:name`、`GET /proxies/:name/delay`、`GET /group`、`GET /group/:name`、`GET /group/:name/delay`、`GET /rules`、`GET /providers/proxies*`、`GET /providers/rules*`、`POST /cache/*/flush`、`GET /dns/query`、`GET /connections`、`/traffic`、`/memory`、`/logs`。

### 5.2 需要"薄封装 + 防御性处理"

| 端点 | 风险 | 依据 |
|---|---|---|
| `GET /configs` | **字段集随版本漂移**（实测 33 字段，且源码里 `General` 在演进）。不可作为完整配置真相 | [实测][上游源码] |
| `DELETE /connections/:id` | 对不存在 id 也 `204` → **无法区分"已关闭"与"不存在"** | [实测] |
| `GET */delay` | `504` 同时表示"节点超时"和别的超时；需按 message 区分 | [实测] |
| `POST /configs/geo`、`/upgrade/geo` | `204` 与真实结果**解耦**（fire-and-forget，失败只在日志） | [实测] |
| `PATCH /configs` | 可改 `tun` 等危险字段；无细粒度权限 | [上游源码 `configSchema`] |
| `PATCH /rules/disable` | 重启即丢失；key 为 index，**重载后 index 会变** | [实测][上游文档] |
| `/logs` 各 level | 实测 `level=info` 可命中；`silent`/`warning` 边界未逐一验证 | [实测部分] |

### 5.3 不稳定 / 不建议依赖

| 端点 | 原因 | 依据 |
|---|---|---|
| `POST /upgrade` | ① 依赖 GitHub 可达（本次实测校验更新需要访问 release API）；② 无可用更新时返回 **500**（把"已是最新"当错误）；③ `force=true` 会**直接替换运行中的二进制**，无签名校验、无 A/B 回滚。Agent 若依赖它，等于把"内核更新"这一核心 Use Case 的失败语义交给内核自己 | [实测][上游源码] |
| `POST /restart` | 文档说 204 实测 200；用 `syscall.Exec` 原地替换，**绕过 Agent 状态机**（Agent 会看到"进程还在、但状态全丢"）；`executor.Shutdown()` 期间的窗口不可观测 | [实测][上游源码] |
| `POST /upgrade/ui` | 实测 30s 超时无响应（网络受限时直接挂起，无超时保护返回） | [实测] |
| `/storage/:key` | 内核 KV 语义与生命周期未被正式文档化，且属于"内核内部存储"，不应作为 Agent 持久化 | [推测] |
| `/ui/*` | 取决于 `external-ui` 配置与目录内容，非稳定 API | [上游源码] |
| `/debug/pprof/*`、`/debug/gc` | **仅 debug 模式**存在；属诊断内部接口 | [上游源码] |
| `external-doh-server` | 官方明示**不校验 secret** | [上游文档] |
| `external-controller-pipe`（Windows） | 官方明示**不校验 secret**；本项目非目标平台 | [上游文档] |

### 5.4 安全红线（实测发现，必须进 ADR/部署设计）

1. **Unix socket 完全绕过 secret**：实测无 header/错误 header 均 `200`。官方文档原文："Accessing API endpoints via Unix socket does not verify secrets. If enabled, please ensure security measures are in place."
2. **socket 文件权限 0666**：实测 `srw-rw-rw-`。**同机任何用户都能获得完整内核控制权（含 `/restart`、`/upgrade`）**。
3. **`/configs` 会回显允许路径**：路径校验失败时返回 `allowed paths: [...]`，泄露目录结构（低危，但不应直接透传给远端 UI）。
4. **`/connections` metadata 含 `uid/process/processPath`**：属敏感运行信息，透传前必须做权限校验与脱敏。
5. **`/upgrade` 无签名校验**：内核自升级不验证产物真实性，Agent 不应把它当作可信更新通道。

> → 对 AGENTS.md 中"Treat socket filesystem permissions as part of the security model"的实证支持：**必须 `chmod 0600`（或 0660 + 专用组）+ 专用 runtime 目录（如 `/run/proxy-agent/`）**，且不能让 socket 落在世界可写的 `/tmp` 下。

---

## 6. 对 Agent 架构的影响

### 6.1 `MihomoController` Port 建议（方法 / 参数 / 返回语义）

按能力拆分，**避免上帝接口**（AGENTS.md 明确反对）。建议将 R01 涉及的面拆成 3 个聚焦 Port：

```rust
/// 核心：运行态只读 + 生命周期相关控制
#[async_trait]
pub trait MihomoController: Send + Sync {
    /// GET /version → 解析 meta/version；meta=false 视为不兼容内核
    async fn version(&self) -> Result<MihomoVersion>;

    /// GET /configs → 仅提取白名单运行态字段
    /// 返回领域值对象 MihomoRuntimeConfig，而非原始 JSON
    async fn runtime_config(&self) -> Result<MihomoRuntimeConfig>;

    /// GET /proxies + GET /group → 统一为 ProxyEntry 列表
    /// 注意：策略组与普通节点用同一个 API 命名空间，需按 `type` 分派
    async fn proxies(&self) -> Result<Vec<ProxyEntry>>;

    /// PUT /proxies/:name  body {"name": ...}
    /// group 必须是 Selector 语义；对 URLTest/Fallback 调用应返回领域错误
    async fn select_proxy(&self, group: &ProxyGroupName, target: &ProxyName) -> Result<()>;

    /// GET /proxies/:name/delay | /group/:name/delay
    /// 504 → Ok(DelayOutcome::Timeout)，而非 Err(InfrastructureError)
    async fn test_delay(&self, target: &ProxyTarget, req: DelayRequest)
        -> Result<DelayOutcome>;

    /// GET /rules → 只读规则快照
    async fn rules(&self) -> Result<Vec<RuleView>>;

    /// PUT /configs  body {"path": <abs>} ；?force 由 Agent 策略决定
    /// 语义：把已在磁盘上、已校验的配置交给内核应用。不负责写文件/回滚。
    async fn reload(&self, config: &ConfigPath) -> Result<ReloadOutcome>;

    /// PATCH /configs 白名单字段（mode / log-level）
    async fn patch_runtime(&self, patch: RuntimeConfigPatch) -> Result<()>;

    /// 探活：GET /version；失败即视为不可控
    async fn health_check(&self) -> Result<HealthStatus>;
}

/// 观测流（与上者分离，便于用 broadcast/事件适配）
#[async_trait]
pub trait MihomoObserver: Send + Sync {
    /// GET /traffic (NDJSON 或 WS) → 归一化事件
    async fn subscribe_traffic(&self) -> Result<TrafficStream>;
    /// GET /logs → 已脱敏的日志流
    async fn subscribe_logs(&self, level: LogLevel) -> Result<LogStream>;
    /// GET /memory
    async fn memory(&self) -> Result<MemoryStat>;
}

/// 连接运维（刻意独立：高频、隐私敏感、非 Domain 实体）
#[async_trait]
pub trait MihomoConnectionOps: Send + Sync {
    /// GET /connections → 直接映射为 DTO，不进 Domain
    async fn list_connections(&self) -> Result<ConnectionSnapshotDto>;
    /// DELETE /connections
    async fn close_all_connections(&self) -> Result<()>;
    /// DELETE /connections/:id
    /// 注意：内核对该端点无幂等反馈，Port 应返回 ()，不要伪造 NotFound 语义
    async fn close_connection(&self, id: &ConnectionId) -> Result<()>;
}
```

**设计要点（均有实测/源码依据）**

- `reload()` **只负责"把磁盘上已校验的配置交给内核"**；文件写入、版本化、校验（CLI `-t`）、健康检查、回滚属于 Application 编排。原因：`PUT /configs` 虽已具备"解析失败不改现状"的原子性 [实测]，但**它不管理文件版本**，而版本化是 Agent 的核心价值。
- `test_delay()` 必须把 `504` 映射为**业务结果** `DelayOutcome::Timeout`，不能当作基础设施故障——否则单节点超时会污染整个用例的错误处理 [实测 `504`]。
- `reload()` 必须支持 `path` 与 `payload` 两种模式，但 **`path` 模式需处理 `SAFE_PATHS` 白名单**（实测：路径必须在工作目录或 `SAFE_PATHS` 内，否则 400）。Agent 的配置目录必须落在 mihomo 工作目录内，或显式设置 `SAFE_PATHS`。
- `PUT` 空 body 会 `400 Body invalid` [实测]，Adapter 必须始终发送 `{}` 或完整 JSON。**这是实测发现的、极易踩的兼容性陷阱。**
- 鉴权：TCP 必须精确 `Authorization: Bearer <secret>`；若走 Unix socket，**Adapter 不应假设 secret 生效**，安全性由文件权限承担 [实测]。

### 6.2 属于 Domain 的状态

```text
MihomoVersion            // meta + version，来源 /version
MihomoRuntimeConfig      // mode / log-level / ports / tun.enable / allow-lan（白名单值对象）
MihomoMode               // rule | global | direct（枚举）
ProxyEntry / ProxyGroup  // name/type/alive/UDP 能力/延迟历史（只读快照）
ProxyGroup::selection    // 当前选中项（now）
DelayOutcome             // Ok(Duration) | Timeout  ← 明确区分业务超时与故障
RuleView                 // 只读规则快照（含 disabled/hitCount）
MihomoStatus / MihomoRuntimeStatus  // 生命周期状态机（Agent 自持）
```

**判定标准**：能从 controller 读到、且能表达"业务不变量"的东西才进 Domain。纯展示性、量大、隐私敏感的数据不进。

### 6.3 明确不进 Domain（关键设计决策）

| 数据 | 为什么不进 Domain | 依据 |
|---|---|---|
| **connections 明细** | 高频、无界、每次请求都不同，且含 `uid/process/processPath/sourceIP/destinationIP` 隐私字段。把它建模成实体会导致：Domain 被 IO 形态污染、内存无界、隐私面进入核心层。**正确定位：Adapter 层 DTO，直接序列化给 Interface 层** | [实测] |
| **traffic 逐秒采样** | 纯遥测流，无业务不变量。仅作为 `TrafficUpdated` 事件广播 | [实测] |
| **logs 原文** | 无结构、含潜在敏感信息（订阅 URL、节点地址）。必须先在 Adapter 层脱敏，再作为事件 | [实测] |
| **memory 数值** | 监控指标，非领域概念 | [实测] |
| **`/storage/:key` KV** | 内核内部存储，语义未文档化；Agent 状态属于 SQLite/文件 | [推测] |
| **provider 原始 JSON** | 订阅内容生成属 Agent/Sub-Store 边界，内核只提供"已加载结果" | [上游源码] |

### 6.4 Infrastructure Adapter 需要处理的"真实世界脏活"（全部来自实测）

1. **双通道**：TCP（需 secret）与 Unix socket（不需 secret）统一为一个 Adapter 配置，但鉴权策略不同。
2. **WebSocket vs NDJSON 双模**：`/traffic`、`/memory`、`/connections`、`/logs` 在无 `Upgrade` 时是 chunked NDJSON。Adapter 应优先用 NDJSON（实现简单、无需 WS 库）；WS 仅在需要 `?token=` 的浏览器场景需要。
3. **超时与错误归一化**：`504`（延迟测试）、`500`（upgrade 无更新）、`400 Body invalid`（缺 body）、`405`（GET 到 POST-only 端点）都要有明确映射。
4. **`PATCH /configs` 白名单**：只允许 `mode` / `log-level`，其余字段拒绝，避免 Agent 变成"任意内核字段注入器"。
5. **`/configs` 字段漂移防御**：反序列化用 `#[serde(default)]` + 容忍未知字段，只读白名单字段。
6. **敏感信息脱敏**：`/logs` 与 `/connections.metadata` 必须过滤。
7. **不得使用 `/upgrade`、`/restart`**：Agent 自持生命周期（systemd / ProcessManager Port）。

### 6.5 明确不做（Do-NOT）

```text
✗ 不重写代理内核 / 不解析、应用代理规则
✗ 不重写 Dashboard / 不自研流量、连接、日志采集
✗ 不实现 /upgrade 与 /restart 的替代语义 —— 而是根本不依赖它们
✗ 不把 /configs 当完整配置真相；完整真相是 Agent 的 ConfigVersion
✗ 不把 connections / traffic / logs 原文建模成 Domain 实体
✗ 不把内核 KV（/storage）当作 Agent 持久化
✗ 不绑定 0.0.0.0；不把 Unix socket 放在世界可写目录且不 chmod
```

---

## 7. 证据与来源

### 7.1 本次实测（可复现步骤）

| 步骤 | 命令 / 动作 | 结果 |
|---|---|---|
| 下载 | `https://ghfast.top/https://github.com/MetaCubeX/mihomo/releases/download/v1.19.30/mihomo-darwin-arm64-v1.19.30.gz` | 200，16,805,556 B |
| 版本 | `./mihomo -v` | `Mihomo Meta v1.19.30 darwin arm64 with go1.26.6`，`with_gvisor` |
| CLI | `./mihomo -h` | 15 个参数，见 §2.3 |
| 校验 | `./mihomo -d . -f config.yaml -t` | `test is successful` |
| 启动 | `./mihomo -d /tmp/r01-mihomo -f config.yaml` | TCP 29190 / Unix r01.sock / mixed 28890 / DNS 25353 全部监听 |
| 鉴权 | curl matrix × TCP / Unix | 见 §3.1 |
| 端点 | ~45 条 route 逐一 curl | 见 §3.2 |
| 字段 | JSON 顶层 key 提取 | 见 §3.3–3.5 |
| 流式 | `curl -N -m 3 /traffic`、`/memory` | NDJSON，1Hz |
| WS | 手工 `Sec-WebSocket-Key` handshake | `101 Switching Protocols` + 帧 |
| reload | `PUT /configs` 合法/非法/相对路径/白名单外 | 204 / 400 且实例存活 / 400 / 400 |
| restart | `POST /restart` | 200 `{"status":"ok"}`，进程自替换 |
| upgrade | `POST /upgrade?force=false` + sha256 前后比对 | 500 "already using latest version"，**binary 未变** |
| geo | `POST /configs/geo` | 204，但日志显示下载仍在进行、无文件落盘 |
| 清理 | kill mihomo；端口已释放；无残留进程 | 已验证 |

**环境限制（诚实声明）**：
- Docker daemon 不可用 → **无 Linux 容器实测**；所有 Linux 专属行为（TUN、nftables、`external-controller-routing-mark`、`auto-redirect`）**均未验证**。
- `github.com` 直连超时 → 部分依赖 GitHub 的端点（`/upgrade`、`/upgrade/ui`）只能观察到"失败路径"，**成功路径未验证**。
- 网络出口受限 → 延迟测试（`/proxies/:name/delay`）实测均 `504`（目标 `gstatic.com/generate_204` 不可达）；**`200 + {"delay":N}` 的成功路径未实测**，字段名依据官方文档。

### 7.2 上游源码

均取自 tag-pinned `v1.19.30`（与 `Alpha` 分支本次抽样一致）：

| 文件 | 用途 | 链接 |
|---|---|---|
| `hub/route/server.go` | 顶层 route 挂载 + `authentication()` 鉴权中间件 | [server.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/server.go) |
| `hub/route/configs.go` | `/configs` GET/PUT/PATCH、`configSchema` 可写字段、`/configs/geo` | [configs.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/configs.go) |
| `hub/route/proxies.go` | `/proxies*` | [proxies.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/proxies.go) |
| `hub/route/groups.go` | `/group*` | [groups.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/groups.go) |
| `hub/route/connections.go` | `/connections*` + `interval` 处理 | [connections.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/connections.go) |
| `hub/route/rules.go` | `/rules`、`/rules/disable`（int index 映射） | [rules.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/rules.go) |
| `hub/route/restart.go` | `/restart` = `syscall.Exec` | [restart.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/restart.go) |
| `hub/route/upgrade.go` | `/upgrade`、`/upgrade/ui`、`/upgrade/geo` | [upgrade.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/upgrade.go) |
| `hub/route/cache.go`、`dns.go`、`provider.go`、`storage.go` | 对应子路由 | [hub/route](https://github.com/MetaCubeX/mihomo/tree/v1.19.30/hub/route) |
| `config/config.go` | `General` / `Inbound`（`/configs` 响应结构） | [config.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/config/config.go) |
| `hub/executor/executor.go` | `GetGeneral()` / `ApplyConfig()` | [executor.go@v1.19.30](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/executor/executor.go) |

### 7.3 官方文档

| 页面 | 用途 | 链接 |
|---|---|---|
| API 参考 | 全部端点、请求方法、响应字段 | [wiki.metacubex.one/en/api/](https://wiki.metacubex.one/en/api/) |
| General configuration | `external-controller*`、`secret`、`SAFE_PATHS`、Unix socket **不校验 secret** 的官方声明 | [wiki.metacubex.one/en/config/general/](https://wiki.metacubex.one/en/config/general/) |
| Release v1.19.30 | tag / 资产 | [releases/tag/v1.19.30](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30) |

### 7.4 文档 vs 实测 差异清单

| 项 | 官方文档 | 实测/源码 | 结论 |
|---|---|---|---|
| `POST /restart` 响应 | `204` | `200 {"status":"ok"}` | **文档有误**，以实测为准 |
| `POST /configs/geo` | `204`（暗示成功） | `204` 但异步、失败仅日志 | 文档**语义不完整** |
| `/proxies/:name` DELETE | `204` | 空 body 时 `400 Body invalid` | 文档未提 body 要求 |
| `/rules/disable` | `{"0":false}` | 一致（int key） | 一致 |
| `PUT /configs` 合法 body | 示例含 `{}` | 空 body `400`；`{}` 成功 | 需**显式**发送 `{}` |
| Unix socket 鉴权 | 明确说明不校验 | 实测确认（且 0666） | 一致，但**权限风险文档未强调** |

---

## 8. 未验证假设与开放问题

### 8.1 `[未验证]` 项（本次环境无法覆盖）

1. **Linux 行为差异**：TUN、nftables/iptables、`auto-redirect`、`external-controller-routing-mark`、`route-address`、`include-uid` 等 Linux-only 字段**全部未验证**（无 Docker/Linux 环境）。→ 需在 Debian + PVE LXC 上补测（关联 R09）。
2. **`/proxies/:name/delay` 成功路径**：实测均 `504`（出口受限）。`{"delay":N}` 的成功响应结构与单位未实测。
3. **provider 相关端点**：本次配置 `proxies: []`、无 proxy-provider / rule-provider，`/providers/proxies/:name/*` 与 `/providers/rules/:name` 的**成功路径未验证**（仅验证了 `/providers/proxies/default`，它是 mihomo 自动生成的 compatible provider）。
4. **`/upgrade` 成功路径**：需 GitHub 可达，未验证下载-替换-restart 全流程。
5. **`/upgrade/ui` 成功/失败语义**：实测 30s 无响应超时，未取得明确状态码。
6. **`?force=true` 的真实差异**：实测 `{}` 与 `{}&force=true` 都返回 `204`，**未能观察到行为差异**；`force` 对 listener 重绑的具体影响需专项验证（关联 R02）。
7. **`/logs` 各 level 与 `silent`**：**部分已收口**（见 §3.3.2）：`debug`（含 info）与 `info` 已实测；`warning`/`error`/`silent` 因本次无法稳定产出对应级别日志**仍未验证**。
8. **`/rules/disable` 重启后是否重置**：文档说会重置，本次**未做重启后复测**。
9. **`/connections?interval=` 的实际推送频率**：源码显示支持（默认 1000ms），但实测计时受并发干扰，未严格验证自定义间隔。
10. **Unix socket 在 Linux 上的默认权限**：实测 macOS 为 `0666`（受 umask 影响）。**Linux + systemd 下的实际权限与 umask 交互未验证** —— 这是安全关键项，必须在目标平台复测。
11. **`/configs` 字段在 Linux 上是否更多**：`tun` 子对象可能包含平台相关字段，未在 Linux 上抓取对比。

### 8.2 开放问题（建议进 `open-questions.md` / ADR）

- **Q-A**：Agent 默认用 TCP controller 还是 Unix socket？
  - TCP：有 secret 鉴权，但需管理端口与 secret 注入。
  - Unix socket：无需 secret，安全完全依赖文件权限（实测 0666 是危险的默认值）；须强制 `chmod 0600` + 专用 `/run/proxy-agent/`。
  - **倾向**：Unix socket + 严格的目录/权限管理（`RuntimeDirectory=` + `RuntimeDirectoryMode=0700`），因为不需要在配置里落 secret。**需 ADR。**
- **Q-B**：`/restart` 与 `/upgrade` 是否应**完全禁止** Agent 调用？
  - 实测两者都绕过 Agent 状态机。**倾向完全禁止**，由 systemd/ProcessManager 承担。**需 ADR。**
- **Q-C**：`PATCH /configs` 的白名单边界是什么？
  - 建议仅 `mode` + `log-level`（最安全）。是否允许更多（如端口热改）需产品决策。**需 ADR。**
- **Q-D**：`SAFE_PATHS` 策略 —— 让配置目录落在 mihomo 工作目录内，还是显式设置 `SAFE_PATHS`？
  - 实测支持两种；影响部署布局与 systemd unit 设计（关联 R13 部署）。
- **Q-E**：`/connections` 的 `metadata.uid/process/processPath` 是否应在 Web UI 暴露？暴露给谁？
  - 隐私面较大，需与 Web 鉴权模型一起决策（关联 R11 安全）。
- **Q-F**：`/rules/disable` 的 index 在 reload 后会变化 —— UI 是否要暴露这个"临时"功能，还是干脆不做？
- **Q-G**：Agent 是否需要在 mihomo 之外**自行**维护流量/连接的历史统计（用于图表）？
  - 内核只给实时值，无历史。若产品要图表，需 Agent 侧采样存储 → 这属于 Agent 自研范围，但要注意 `/traffic` 是逐秒全量、长期存储成本与隐私问题。
- **Q-H**：mihomo 启动时 listener 绑定失败（实测 `bind: address already in use`）**不致命且仅记 error 日志** —— Agent 健康检查如何检测这种"半启动"状态？
  - 这是实测发现的重要边界：`/version` 仍会返回 200，但代理端口可能没起来。**健康检查必须同时验证 controller 可达 + 代理端口可连 + `/configs` 反映预期端口**。建议进 R03/R09。

---

> **R01 完成度**：Controller API 能力清单、稳定性分级、能力矩阵、Port 建议均已基于实测 + tag-pinned 源码 + 官方文档完成。Linux/PVE LXC 平台相关能力为本次环境不可达项，已逐条标注 `[未验证]`，需在 R09（Linux runtime）与 R02（Mihomo config）中补齐。
