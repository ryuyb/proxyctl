# R12 — 安全模型（Threat Model / Trust Boundary / Permission Model）

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：主要由 `[实测]`（mihomo v1.19.30 本机原生运行 + 容器内运行 + 源码）与 `[上游源码]`/`[上游文档]` 支撑；容器实验受 Docker Hub 不可达限制，仅使用了本地已有镜像（见 §12.3）。
> 关键结论一句话：**mihomo 的 `secret` 为空（默认值）即等于完全无鉴权，`external-controller-cors` 默认 `allow-origins: ['*']` + `allow-private-network: true`，且 `external-controller: ":9090"` 这种写法会绑定 `0.0.0.0/[::]`——因此 Agent 必须用「Unix socket + 文件权限（0660 root:proxy-admin）+ 显式非空 secret + 绝不监听非 loopback 地址」三重加固，并把「远程可达 ⇒ 强制认证」做成启动期硬校验而不是部署约定。**

---

## 1. 结论摘要（TL;DR）

1. **Mihomo controller 默认是「匿名可写」的。** 源码中只有 `if secret != "" { r.Use(authentication(secret)) }`（`hub/route/server.go:119-122`，v1.19.30 同）。`secret` 缺省为空字符串 ⇒ 鉴权中间件根本不挂载。`[实测]`：不配 `secret` 时，`GET /version`、`/configs`、`/proxies`、`/rules` 全部 `200`，无任何认证头。
2. **Unix socket 是「无鉴权 + 0666」的组合。** `external-controller-unix` 启动时显式 `os.Chmod(addr, 0o666)`，且传给 router 的 secret 被硬编码为 `""`（`hub/route/server.go:286,290`；v1.19.30 完全一致）。任何本地非特权用户都能 connect，并拿到全部控制面能力。官方文档中文原文：**「从 Unix socket 访问 api 接口不会验证 secret，如果开启请自行保证安全问题」**。`[实测]` + `[上游文档]`。
3. **`secret` 的「空」有两种写法，行为一致且都危险**：省略字段、`secret: ""`、`secret: "   "` 在语义上都是「无鉴权」；`[实测]` 省略与显式空串均返回 `200`。
4. **CORS 默认允许任意来源，并允许公网页面访问私有网络。** `DefaultRawConfig()` 中 `ExternalControllerCors: RawCors{AllowOrigins: []string{"*"}, AllowPrivateNetwork: true}`（`config/config.go:595-598`）。`[实测]` 即使把 `allow-origins` 显式写成 `[]`，响应仍是 `Access-Control-Allow-Origin: *` + `Access-Control-Allow-Private-Network: true`。这意味着「用户浏览器里的任意网站」可以读写本机 mihomo API——在个人 PC 场景是 CSRF/DNS rebinding 级风险。
5. **`external-controller: ":9090"` 会全网卡监听。** `[实测]`：配置为 `":19097"` 时实际监听 `[::]:19097`（`*:19097`）；而 `127.0.0.1:19097` 只监听 loopback。因此 Agent 生成配置时必须**显式校验 host 非空且属于 loopback**。
6. **已知漏洞：CVE-2025-56499（mihomo ≤1.19.11 任意文件读取）。** `/configs` 注入 `rule-providers.type=file` + 任意 `path`，解析错误把文件内容写进日志，再经 `/logs` 读回。`[实测]` v1.19.30 已修复绝对路径穿越（`/etc/passwd` 返回明确 400），但**工作目录内/`SAFE_PATHS` 内的文件仍会被读取并可能进入日志**——这是「修复后的残余风险」，直接影响 Agent 的 `SAFE_PATHS` 设计。
7. **Sub-Store 不能作为可信组件。** 默认监听 `::`（全地址）、Node.js、**没有任何内置认证**；其 `/api/preview/sub` 的 Script Operator 历史上存在未授权 RCE（官方 issue #634，v2.11.4–2.37.1，v2.38.2 起修复）。Agent 必须把它当**本机不可信外部组件**（默认只允许 loopback/Unix socket，且置于 Agent 反代+认证之后）。
8. **推荐的 Agent 认证方案（MVP）**：本地走 Unix socket + 文件权限（`0660 root:proxy-admin`）+ `SO_PEERCRED` 校验 uid/gid；远程走 **Bearer token（哈希存储）** + 严格 CORS 白名单 + 「非 loopback 监听 ⇒ 启动期强制要求 token，否则拒绝启动/拒绝绑定」。会话 cookie+CSRF 与 mTLS 列入 Phase 2。
9. **MVP 安全基线共 32 条 checklist**（§10），每条可直接映射为集成/单元测试（其中 9 条已在本轮 `[实测]` 验证过 mihomo 侧行为）。

---

## 2. 资产清单与信任边界图（文本图）

### 2.1 资产清单

| ID | 资产 | 位置/形态 | 泄露影响 | 篡改影响 |
|----|------|-----------|----------|----------|
| A1 | Mihomo 内核进程 | `/usr/lib/proxy-agent/mihomo/mihomo-*`（设计文档 §38） | 低 | **极高**：替换即获得数据面全部流量（含 TLS 之外的明文元数据） |
| A2 | Mihomo 运行时配置（含节点凭据） | `/var/lib/proxy-agent/configs/vNNN.yaml`、`/etc/mihomo/config.yaml` | **极高**：含节点 UUID/password、订阅 token、`secret` | **极高**：改写成恶意节点/规则 = 流量劫持 |
| A3 | 订阅源 URL（含凭据 query） | SQLite `subscriptions.url` + 也可来自 `?api=` 形式 | **高**：订阅 token 可被他人盗用并窥探全部节点 | 中：切换订阅到攻击者节点 |
| A4 | Mihomo `secret`（controller 访问密钥） | 生成的 YAML 内 + 可能落到 Agent 的内存/DB | **高**：等价于控制面读写权 | 高 |
| A5 | Agent Unix socket | `/run/proxy-agent/agent.sock` | — | **极高**：可伪造任意操作请求（start/stop/改配置/更新二进制） |
| A6 | Agent Web API / Admin UI | TCP 监听（默认 loopback） | — | **极高**（跨边界后等价 root 级控制面） |
| A7 | Web 认证 token / 密码哈希 | `/etc/proxy-agent/config.toml`、SQLite `settings` | **高** | 高：可持久化后门 |
| A8 | systemd root 权限与 capability | `proxy-agent.service` | — | **极高**：`CAP_NET_ADMIN`/`CAP_SYS_ADMIN` 级能力被滥用 |
| A9 | nftables/iptables 防火墙与策略路由规则 | 内核 netfilter 表 | — | **高**：可造成断网、流量旁路或隐蔽隧道 |
| A10 | 审计日志 | SQLite `audit_logs` + journald | 中：暴露行为模式/目标 config id | **高**：抹除痕迹 |
| A11 | 二进制更新通道 | GitHub Releases（`MetaCubeX/mihomo`） | — | **极高**：供应链替换（见 T5） |
| A12 | Sub-Store 运行时与其数据 | `xream/sub-store` 容器/进程，数据卷 `/opt/app/data` | **极高**：含全部订阅与转换脚本 | **极高**：其 Script Operator 可导致 RCE |
| A13 | Agent 内部 SQLite 元数据 | `/var/lib/proxy-agent/database.sqlite` | 中（含订阅 URL、审计） | 中：篡改状态/配置版本链 |

### 2.2 信任边界图

```text
┌───────────────────────────────────────────────────────────────────────────────┐
│ TB-0  不可信：Internet / 公共网络                                             │
│   - 任意站点（浏览器内 JS，可能读取本机 127.0.0.1，见 T2）                     │
│   - 远程 Web 用户（未认证）/ 被入侵的订阅服务端                                │
└───────────────┬───────────────────────────────────────────────────────────────┘
                │  (边界跨越必须：TLS 终止 + 认证 + 严格 CORS)
                ▼
┌───────────────────────────────────────────────────────────────────────────────┐
│ TB-1  半可信：Reverse Proxy（nginx/caddy，独立进程，非本项目资产）             │
│   - 只做 TLS、限流、认证前置；不得把未认证流量透传到 Agent                     │
└───────────────┬───────────────────────────────────────────────────────────────┘
                │  (只允许 loopback / 私有管理网)
                ▼
╔═══════════════════════════════════════════════════════════════════════════════╗
║ TB-2  Agent 本体边界（proxy-agent 进程，二选一或同时）                        ║
║                                                                               ║
║   (a) Web API  ── TCP 127.0.0.1:PORT（远程可达时必须 Bearer token）           ║
║   (b) Local API ─ /run/proxy-agent/agent.sock  0750 root:proxy-admin          ║
║                   + SO_PEERCRED(uid/gid) 校验（Defense in Depth）              ║
║                                                                               ║
║   进程内：Application UseCase → Port（Auth/Audit/Process/Firewall/…）          ║
╚═══════════╤═══════════════════════════════════╤═══════════════════════════════╝
            │                                   │
   (仅经显式 UseCase + Port)          (仅经显式 UseCase + Port)
            ▼                                   ▼
┌───────────────────────────────┐   ┌───────────────────────────────────────────┐
│ TB-3  Mihomo 控制面            │   │ TB-4  系统特权边界                          │
│  mihomo.sock (0660)            │   │  systemd (root) / CAP_NET_ADMIN             │
│  或 127.0.0.1:9090 + secret   │   │  nftables / TUN / 策略路由 / 二进制安装      │
│  ⚠ 若配错则退化为 0666+无鉴权  │   │  ⚠ 此边界内不得存在「任意命令执行」            │
└───────────────┬───────────────┘   └───────────────────────────────────────────┘
                │
                ▼
┌───────────────────────────────────────────────────────────────────────────────┐
│ TB-5  外部可替换组件（不可信，默认只允许本机访问）                              │
│  Sub-Store（Node.js，默认 :: 监听、无内置鉴权、含 Script Operator）            │
│  metacubexd 静态资源 / external-ui                                             │
└───────────────────────────────────────────────────────────────────────────────┘

┌───────────────────────────────────────────────────────────────────────────────┐
│ TB-6  本机非特权用户（同主机、无 root）                                        │
│  可：connect 任意 0666 socket、读 world-readable 文件、发起任意出站请求         │
│  不可（若加固正确）：读写 /run/proxy-agent/agent.sock、读 configs/*.yaml      │
│  ⚠ 这是本安全模型最容易被忽视的边界：mihomo 默认给它开了 0666 的 socket        │
└───────────────────────────────────────────────────────────────────────────────┘
```

**跨边界规则（架构硬约束）：**

```text
TB-0/TB-1 → TB-2 : 必须认证（token）→ 必须输入校验 → 只能命中已知 UseCase
TB-2      → TB-3 : 只走 MihomoController Port；配置里的 secret 非空；地址 loopback 或 unix
TB-2      → TB-4 : 只走显式 UseCase + Port；禁止任何形式的 shell 拼接/透传
TB-2      → TB-5 : 视为不可信远端 HTTP 服务；超时/限重定向/不转发用户可控 URL 到其内网接口
TB-6      → TB-2 : 仅 socket 文件权限 + peer uid 白名单（不用口令）
```

---

## 3. Threat Model（威胁 → 缓解 → 残余风险）

采用 STRIDE 分类。信任边界编号对应 §2.2，证据列写 `[实测]`/`[上游源码]`/`[上游文档]`/`[推测]`/`[未验证]`。

### 3.1 Spoofing（身份伪造）

| ID | 威胁 | 缓解 | 残余风险 | 证据 |
|----|------|------|----------|------|
| T-S1 | 本地非特权用户伪造为合法客户端，经 `agent.sock` 调用特权 UseCase | socket `0660 root:proxy-admin`；目录 `0750 root:proxy-admin`；启动时显式 `chmod`（bind 后、umask 前存在窗口）；`SO_PEERCRED` 校验 uid/gid 白名单 | 同组用户互相冒充（`SO_PEERCRED` 可区分 uid 但不能区分同 uid 进程）；抽象命名空间 socket 无文件权限可用 | `[上游文档]` unix(7)/systemd.exec；`[上游文档]` docs.rs/tokio UnixStream::peer_cred |
| T-S2 | 远程攻击者以匿名身份访问 Web API | 「非 loopback 监听 ⇒ 必须有 token，否则拒绝绑定」启动期硬校验；Bearer token（哈希存储）；反向代理只允许已认证流量 | token 泄露（浏览器 localStorage / 截图 / 日志）后完全失守；无速率限制时仍可暴力枚举（须 128-bit 以上随机） | `[推测]` 设计约束，§6.2 给方案 |
| T-S3 | 恶意网页（浏览器）冒充合法前端调用 mihomo/Agent API | CORS 白名单（**不使用 `*`**）；不信任 `Origin` 缺失；对写操作要求自定义头（`X-Requested-With` 语义）或 CSRF token；Agent 侧不依赖 CORS 做鉴权 | CORS 只管浏览器；非浏览器客户端不受影响，必须靠认证；DNS rebinding 可绕过 IP 判断 `[推测]` | `[实测]` mihomo 默认 `ACAO: *` |

### 3.2 Tampering（篡改）

| ID | 威胁 | 缓解 | 残余风险 | 证据 |
|----|------|------|----------|------|
| T-T1 | 二进制替换（本地或供应链）→ 长期流量劫持 | 只从官方 Release 下载；SHA256 校验（设计文档 §38 已有流程）；安装目录 `0755 root:root`、二进制 `0755 root:root`；记录 SHA256 到审计；Phase 2 考虑 minisign/cosign 或 GPG | 上游 Release 本身被投毒时 SHA256 无效（需签名）；下载过程仍依赖 HTTPS 与 CDN 信任 | `[上游文档]` 设计文档 §38 + `AGENTS.md` |
| T-T2 | 配置注入：订阅内容/API 请求夹带恶意 Mihomo 配置（恶意 `rule-providers`、`proxy-providers` URL、`tun`、`dns`、任意 `path`） | 配置必须过 `ValidateConfig` UseCase（schema + 白名单 key + 路径必须落在 config 目录内且为 Agent 生成的相对名）；版本化 + 激活前 diff + 健康检查 + 失败回滚；禁止直接把用户输入当 `path` | mihomo 会执行配置中的 `script`/`file` provider 语义；白名单内文件仍可被读取（见 T-I3） | `[实测]` + `[上游源码]` `constant/path.go:88-105` |
| T-T3 | 防火墙/策略路由规则残留或冲突 → 断网 | 所有 nft/iptables 操作走 FirewallPort；`apply` 与 `revert` 成对；写入前快照、失败自动回滚；规则带专属 table/comment 前缀；提供 `doctor` 检测残留 | 进程被 `SIGKILL` 时无法 revert；需要启动时 reconcile（`doctor` 修复） | `[上游文档]` 设计文档 §57 已有 `system.nftables.apply` 审计动作 |
| T-T4 | 审计日志被篡改/删除 | SQLite 文件 `0640 root:proxy-agent`；审计表只允许 append（应用层无 UPDATE/DELETE 路径）；高风险操作同时写 journald（`syslog.target` 或 stderr → journal） | 拥有 root 的攻击者可改一切；本地磁盘损坏 | `[推测]` 需在实现中固化 |

### 3.3 Repudiation（抵赖）

| ID | 威胁 | 缓解 | 残余风险 | 证据 |
|----|------|------|----------|------|
| T-R1 | 高风险操作（改配置、更新二进制、改防火墙）无法追责 | 审计 `who/when/what/target/result`；`actor` 使用可归因身份（`local:uid=0`、`unix:uid=1000`、`web:token:<token_id>`、`cli:uid=`）；记录 `request_id`/`config_version`/`config_sha256` | 共享 root 时无法区分具体人；token 复用时无法区分设备 | `[上游文档]` 设计文档 §57 audit_logs 结构 |
| T-R2 | 失败操作不记录 | 审计覆盖失败路径（`result=failure` + `error_code`），而不是只记成功 | 崩溃/断电瞬间的最后一条可能丢失 | `[推测]` |

### 3.4 Information Disclosure（信息泄露）

| ID | 威胁 | 缓解 | 残余风险 | 证据 |
|----|------|------|----------|------|
| T-I1 | 日志泄露：订阅 URL 中的 `?token=`/`password=`、Mihomo `secret`、节点 UUID、认证 token、完整敏感配置 | 统一 `redact()`：URL query 白名单化（只留 scheme/host/path，query 全 `***`）；字段名匹配 `secret|token|password|passwd|uuid|psk|private-key|authorization|cookie` 一律 redact；YAML 只记录 key 路径与 sha256，不记录 value；tracing 用结构化字段而非裸字符串 | 上游库自身的 debug 日志（mihomo 自身、reqwest 等）可能带原文；需 `RUST_LOG` 默认收口到 `info`，并对 mihomo 子进程 stdout 做二次过滤 | `[实测]` mihomo 自身不打印 secret；`[上游文档]` `AGENTS.md` 日志禁令 |
| T-I2 | `/api/v1/logs`（Agent 转发 mihomo 日志）泄露配置内容 | 转发前过 redact；默认只返回最近 N 行；不返回 mihomo 的 debug/silent 级别原文 | mihomo 自身错误信息可能包含节点名/地址（非凭据） | `[实测]` mihomo `/logs` 是原样文本流 |
| T-I3 | **配置目录内文件被 mihomo 自身读取并进入日志（CVE-2025-56499 残余类）** | 约束 `SAFE_PATHS` **只含 mihomo 工作目录**；Agent 的 secret/token/db 绝不放在该目录或 `SAFE_PATHS` 内；config 目录 `0700`（或 0750 且不含 secret 文件）；禁用/严格校验用户可控的 `file` 类型 provider；Agent 自己做「写入用户可见/可读的 mihomo 配置」而不是让用户经 mihomo `/configs` 写 | 即使路径校验存在（`IsSafePath`），白名单内任意文件仍可被读取并可能进日志；`SAFE_PATHS` 由环境变量扩展，若 Agent 为兼容 dashboard 而放宽即放大风险 | `[实测]` v1.19.30：`/etc/passwd` → 400，工作目录内文件 → 204 |
| T-I4 | Sub-Store 数据泄露（订阅、脚本） | 只允许本机访问；不把 Agent 的订阅写入 Sub-Store 数据卷以外；如必须暴露，前置反代 + 认证 + 随机路径（仅作 obscurity，不作认证） | Sub-Store 自身无认证，任何能访问其端口者即拿到全部订阅；Script Operator 可 RCE | `[上游文档]` Sub-Store README/issue #634，见 §5 |
| T-I5 | 审计/DB 文件权限过宽 | `database.sqlite`、`config.toml`、`configs/` 属主 `root:proxy-agent`，`0640`/`0750`；LXC 场景注意 GID 映射 | 备份/快照带走明文（需文档提示或加密） | `[推测]` |

### 3.5 Denial of Service（拒绝服务）

| ID | 威胁 | 缓解 | 残余风险 | 证据 |
|----|------|------|----------|------|
| T-D1 | 高频 `start/stop/update` 导致进程风暴或半启动状态 | 每实例生命周期锁（`AGENTS.md` 已有要求）；非法状态转换拒绝；操作幂等；单飞（single-flight）更新 | 死锁/长时间持锁导致 API 不可用（需要超时与可观测性） | `[上游文档]` `AGENTS.md` State and Concurrency |
| T-D2 | 订阅抓取放大（超大响应体、无限重定向、慢速攻击） | `Content-Length`/流式读取上限（如 10 MiB）；总超时（connect/read/overall）；重定向 ≤3 且每跳重新校验；单订阅并发去重 + 全局限流 | 上游上游正常但极慢时表现为「慢失败」；需退避与熔断 | `[推测]` |
| T-D3 | `/configs` reload 风暴打满 CPU | API 层限流 + 队列；配置校验结果缓存（sha256 key） | mihomo 自身 reload 开销不可控 | `[推测]` |
| T-D4 | 本地非特权用户通过 0666 mihomo socket 反复 reload 造成流量中断 | Agent 接管 socket 权限（`0660`）；提供 `doctor` 检测；文档明确「不要直接暴露 mihomo controller」 | 若用户自行按官方 systemd 单元跑 mihomo，Agent 无法阻止 | `[实测]` 0666 行为 |
| T-D5 | 磁盘被日志/版本文件写满 | 版本保留上限（如最近 N 个）；日志轮转；SQLite 增长监控 | 审计日志按合规需求可能必须长期保留（策略可配） | `[推测]` |

### 3.6 Elevation of Privilege（提权）

| ID | 威胁 | 缓解 | 残余风险 | 证据 |
|----|------|------|----------|------|
| T-E1 | **任意命令执行端点**（`POST /api/run-command` 类） | 硬规则禁止；所有特权操作映射显式 UseCase + Port；Port 参数为枚举/结构化类型，不接受字符串命令/参数数组；`ProcessManager` 只接受 `MihomoBinary` 等已知程序 + 参数白名单 | 未来若引入「自定义钩子脚本」需 ADR 与隔离（独立低权用户 + 无网络） | `[上游文档]` `AGENTS.md` Security Rules |
| T-E2 | Unix socket 劫持/替换：删除 socket 文件后抢先 bind（同目录可写时） | 目录必须为 `root` 所有且非组可写（`0750`）；socket 文件 `0660 root:proxy-admin`；客户端可校验 peer（若需）| 同组用户仍可在目录不可写的前提下无法替换；若目录被误设 `0770 proxy-admin` 则可被替换 | `[上游文档]` unix(7) |
| T-E3 | Web/CLI 参数注入到特权层（路径穿越 `/etc/shadow`、`..`、符号链接） | 路径类参数必须为「ID」而非路径（`ConfigVersionId`）；若必须接受路径，`canonicalize` 后校验为受管目录子路径，拒绝 symlink 逃逸（`O_NOFOLLOW`/`openat2(RESOLVE_BENEATH)`） | TOCTOU（校验后用）需靠 fd 传递规避 | `[实测]` mihomo 用 `filepath.Rel + IsLocal`，Agent 需更强 |
| T-E4 | Sub-Store 被当作转换器使用时，攻击者经其 Script Operator 在 Agent 主机上执行代码 | 将 Sub-Store 视为不可信外部服务；Agent 不向它传递可执行脚本；优先使用 `sub-store-convert` / 原生转换器；建议禁用它到内网以外的可达性 | 若用户自建 Sub-Store 并启用脚本，其主机权限即等于代码执行权 | `[上游文档]` Sub-Store issue #634 |
| T-E5 | LXC 未隔离（privileged LXC）+ Agent 具 `CAP_SYS_ADMIN` → 逃逸/影响宿主 | 只授予所需 capability（`CAP_NET_ADMIN` + `CAP_NET_RAW` + `CAP_NET_BIND_SERVICE`），禁止 `CAP_SYS_ADMIN`/`CAP_SYS_PTRACE`/`CAP_DAC_OVERRIDE`；LXC 场景检测并显式报告 | PVE privileged LXC 本身是宿主信任边界内；`doctor` 只能提示不能阻止 | `[上游文档]` Mihomo 官方 unit 反而授予了 `CAP_SYS_PTRACE CAP_DAC_OVERRIDE`（见 §9） |

---

## 4. Mihomo Controller 暴露面（默认值 + 源码证据 + CVE）

### 4.1 关键配置项的准确语义与默认值

| 配置项 | `[上游源码]` 默认值 | 官方文档示例 | `[实测]` v1.19.30 行为 | 结论 |
|--------|--------------------|--------------|------------------------|------|
| `external-controller` | **无内置默认端口**。`config.DefaultRawConfig()`（`config/config.go`）不设置该字段；`hub/route/server.go:170` 仅当 `len(cfg.Addr) > 0` 才监听 | 文档表格写 `127.0.0.1:9090`，仅为**推荐示例**，不是内建默认 | 不配置 ⇒ 不监听任何 TCP 端口 | Agent **必须显式**写 `127.0.0.1:<port>`；不能假定默认 loopback |
| `external-controller` host 为空 | 无校验 | — | `":19097"` ⇒ 实际监听 `[::]:19097`（`*:19097`） | **高危**：`":9090"` 会全网卡暴露；Agent 必须校验 host 非空且为 loopback |
| `secret` | `""`（Go 零值） | `secret: ""` | 无 `secret` / `secret: ""` ⇒ 全部接口匿名 `200` | **空 secret = 无鉴权** |
| `external-controller-cors.allow-origins` | `["*"]`（`config/config.go:595-598`） | 文档示例 `- '*'` | `ACAO: *`，即使显式配置 `[]` 也仍为 `*` | 默认允许任意来源 |
| `external-controller-cors.allow-private-network` | `true`（同上） | 文档示例 `true` | `Access-Control-Allow-Private-Network: true` | 允许公网页面访问本机私有网络（PNA 绕过） |
| `external-controller-unix` | `""`（无默认 socket 路径） | `mihomo.sock` | 启用后 socket 文件权限 **0666**，且 **不校验 secret** | Agent 若使用，必须自行收紧权限 |
| `external-controller-pipe` (Windows) | `""` | `\\.\pipe\mihomo` | 同样不校验 secret（源码同路径，`server.go:321`） | 非本项目目标平台，记录备查 |
| `external-controller-tls` | `""`；文档要求同时填 `external-controller` | `127.0.0.1:9443` | 支持 `client-auth-type`/`client-auth-cert`（mTLS，`server.go:219-226`） | 可选强化项（Phase 2） |
| `external-doh-server` | `""` | `/dns-query` | 文档明确「该 URL 不会验证 secret」 | 若启用需同等保护 |
| `external-ui` | `""` | `/path/to/ui/folder` | 路径必须在工作目录或 `SAFE_PATHS` 内 | 静态资源与 API 同源，存在 XSS→API 读取风险 |
| `external-ui-url` | `https://github.com/MetaCubeX/metacubexd/archive/refs/heads/gh-pages.zip` | 同 | 更新 UI 时从该 URL 下载 | 供应链面：SPA 从 GitHub 拉取 |

> 官方文档中文原文（`external-controller-unix`）：**「从 Unix socket 访问 api 接口不会验证 secret，如果开启请自行保证安全问题」**
> —— `[上游文档]` https://wiki.metacubex.one/config/general/

### 4.2 `secret` 为空时的行为（源码证据）

鉴权中间件只在 `secret != ""` 时挂载：

```go
// [上游源码] hub/route/server.go:119-122 (v1.19.30 与 master 一致)
r.Group(func(r chi.Router) {
    if secret != "" {
        r.Use(authentication(secret))
    }
    r.Get("/", hello)
    ...
```

中间件本体（Bearer 与 WebSocket query token）：

```go
// [上游源码] hub/route/server.go:336-365
func authentication(secret string) func(http.Handler) http.Handler {
    ...
    // Browser websocket not support custom header
    if r.Header.Get("Upgrade") == "websocket" && r.URL.Query().Get("token") != "" {
        token := r.URL.Query().Get("token")
        if !safeEqual(token, secret) { ... 401 ... }
    }
    header := r.Header.Get("Authorization")
    bearer, token, found := strings.Cut(header, " ")
    hasInvalidHeader := bearer != "Bearer"
    hasInvalidSecret := !found || !safeEqual(token, secret)
    ...
```

Unix socket 与 pipe 路径**硬编码**传空 secret，且显式 chmod 0666：

```go
// [上游源码] hub/route/server.go:277-291
_ = syscall.Unlink(addr)                       // 先删除旧 socket
l, err := lc.Listen(context.Background(), "unix", addr)
...
_ = os.Chmod(addr, 0o666)                      // ⚠ 所有本地用户可连接
server := &http.Server{ Handler: router(cfg.IsDebug, "", cfg.DohServer, cfg.Cors) }  // ⚠ secret = ""
```

**`[实测]` 结果（v1.19.30，darwin/arm64 原生）：**

```text
# secret 配置省略 / secret: ""
GET /version   (无认证头, over 127.0.0.1)  -> 200
GET /configs   (无认证头)                  -> 200
GET /proxies   (无认证头)                  -> 200
GET /rules     (无认证头)                  -> 200
GET /version   (over unix socket, 无认证)  -> 200
socket 文件                                 -> srw-rw-rw- (0666)

# secret: "s3cr3t-token-xyz"
无认证头                                    -> 401 {"message":"Unauthorized"}
Authorization: Bearer s3cr3t-token-xyz      -> 200
Authorization: s3cr3t-token-xyz  (无 Bearer) -> 401
Authorization: Bearer wrong                 -> 401
GET /version?token=s3cr3t-token-xyz (非 WS)  -> 401  (query token 仅对 WS Upgrade 生效)
```

结论：**无 secret ⇔ 无鉴权**（对 TCP 与 Unix socket 同时成立）。`[实测]` 与 `[上游源码]` 一致。

### 4.3 CORS 实测结果

```text
# 默认（未配置 cors 块）与显式 allow-origins: ['*'], allow-private-network: true
OPTIONS /version
  Origin: https://evil.example
  Access-Control-Request-Method: GET
  Access-Control-Request-Private-Network: true
-> HTTP/1.1 200 OK
   Access-Control-Allow-Origin: *
   Access-Control-Allow-Methods: GET
   Access-Control-Allow-Private-Network: true
   Access-Control-Max-Age: 300

GET /version  Origin: https://evil.example
-> HTTP/1.1 200 OK
   Access-Control-Allow-Origin: *

# 显式 allow-origins: []  (仍然)
-> Access-Control-Allow-Origin: *
```

含义：默认配置下，**用户浏览器中被访问的任意网站都能读取本机 mihomo API 的响应**（并且如果 API 允许简单写请求，可发起状态改变）。这是「本机代理软件」类产品的经典风险面（同一类问题也发生在 Sub-Store 上）。

### 4.4 已知 CVE / 安全公告

| 编号 | 影响产品与版本 | 影响 | 修复版本 | 与本项目关系 | 来源 |
|------|----------------|------|----------|--------------|------|
| **CVE-2025-56499** | `metacubex:mihomo:1.19.11`（NVD CPE 精确匹配该版本；Snyk 判 `<1.19.12`） | 认证低权用户经 `/configs` 注入 `rule-providers: {type: file, path: <任意文件>}`，解析错误把文件内容带入日志，经 `/logs` 读回 ⇒ **任意文件读取**（CVSS 3.1 `AV:N/AC:L/PR:L/UI:N/C:H/I:N/A:N` = 6.5 Medium，CWE-284） | Snyk：`1.19.12+`；Volerion 建议 `1.19.16+` | **直接相关**：Agent 会调用 `/configs`，且会把 mihomo 日志回传给用户 ⇒ 必须 (a) 固定 ≥1.19.12（建议最新 stable），(b) 不在 `SAFE_PATHS` 或工作目录中存放 Agent 凭据 | `[上游文档]` NVD https://nvd.nist.gov/vuln/detail/CVE-2025-56499 · Snyk https://security.snyk.io/vuln/SNYK-GOLANG-GITHUBCOMMETACUBEXMIHOMORULESPROVIDER-14054326 · Volerion https://volerion.com/vulnerabilities/CVE-2025-56499 · PoC https://github.com/Cherrling/CVE-2025-56499 |
| **CVE-2024-5732** | `Clash up to 0.20.1 on Windows`（原版 Clash，非 mihomo） | Proxy Port 组件 improper authentication（CVSS 3.1 9.8 由 NVD 给分） | 无（原版 Clash 已归档） | **不直接适用**（Windows + 已停止维护的原版 Clash）；作为「controller/proxy port 暴露即高危」的历史佐证 | `[上游文档]` NVD/Tenable https://www.tenable.com/cve/CVE-2024-5732 |
| CVE-2025-9474 | **Mihomo Party**（第三方 GUI，macOS）≤1.8.1 | `enableSysProxy` 创建不安全权限临时文件（本地、高复杂度） | 未核实具体修复版本 | **不适用**：是 Mihomo Party 客户端，不是 mihomo 内核 | `[上游文档]` NVD 查询结果 |

**检索方法与覆盖度**：NVD API `keywordSearch=mihomo` 返回 2 条（CVE-2025-9474、CVE-2025-56499）；`keywordSearch=clash proxy` 返回 3 条（仅 CVE-2024-5732 相关）。GitHub Security Advisories 页面经代理与 API 均不可读（403 / 限流），因此 **GHSA 列表完整性标注为 `[未验证]`**，建议实施阶段用 `cargo audit` 对 Rust 依赖、并在 CI 中对 mihomo 版本做 advisory 轮询。

### 4.5 CVE-2025-56499 类风险的实测复核（重要）

`[实测]` v1.19.30 对绝对路径穿越已有防护：

```text
PUT /configs {"payload":"rule-providers:\n  pwn:\n    type: file\n    behavior: classical\n    path: /etc/passwd\n"}
-> HTTP 400
   {"message":"path is not subpath of home directory or SAFE_PATHS: /etc/passwd \n allowed paths: [/tmp/r12-sec]"}
```

对应源码：

```go
// [上游源码] hub/route/configs.go (v1.19.30) 约 419-427
if !req.Path.IsAbs() { ... }                 // 必须绝对路径
if !C.Path.IsSafePath(req.Path) { 400 }      // 必须落在工作目录/SAFE_PATHS 内
```

```go
// [上游源码] constant/path.go:87-105 (v1.19.30)
// IsSafePath return true if path is a subpath of homedir (or in the SAFE_PATHS environment variable)
func (p *path) IsSafePath(path string) bool {
    if p.allowUnsafePath || features.CMFA { return true }   // ⚠ 存在全量旁路开关
    path = p.Resolve(path)
    for _, safePath := range p.SafePaths() {
        if rel, err := filepath.Rel(safePath, path); err == nil {
            if filepath.IsLocal(rel) { return true }
        }
    }
    return false
}
func (p *path) SafePaths() []string { return append([]string{p.homeDir}, p.safePaths...) }
```

但**残余风险依然存在**：`[实测]` 把 `path` 指到工作目录内的一个非配置文本文件时，`PUT /configs` 返回 `204`（被接受）。即「白名单目录内的任意文件仍可被 mihomo 读取，且读取失败/解析错误会进入日志」。因此对 Agent 的硬要求：

```text
R12-MIHOMO-1  生成 mihomo 配置时，rule-providers/proxy-providers 的 file 路径必须是
              Agent 自己生成的、位于 mihomo 工作目录内的固定文件名；不接受用户传入 path。
R12-MIHOMO-2  mihomo 工作目录内不得存放任何 Agent 凭据（secret/token/DB/订阅原文）。
R12-MIHOMO-3  SAFE_PATHS 只允许包含 mihomo 工作目录本身；不因 dashboard 需求而放宽。
R12-MIHOMO-4  Agent 回传 /logs 前必须 redact（见 §8）。
```

---

## 5. Sub-Store 暴露面

| 维度 | 事实 | 证据 |
|------|------|------|
| 运行时 | Node.js（`backend/package.json` 的 `"serve": "node sub-store.min.js"`；依赖 express/body-parser） | `[上游文档]` https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/package.json |
| 默认监听 | backend `SUB_STORE_BACKEND_API_HOST \|\| '::'`、`PORT \|\| 3000`；frontend `HOST \|\| host \|\| '::'`、`PORT \|\| 3001` ⇒ **默认绑定所有地址（`::` ≈ 0.0.0.0）** | `[上游文档]` https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/src/restful/index.js |
| 内置认证 | **无**。express 只挂 CORS + body-parser，无 auth 中间件；`/api/preview/sub` 等受权路由直接执行 | `[上游文档]` https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/src/vendor/express.js · .../restful/preview.js |
| 已知严重问题 | 官方 issue #634「ITW Unauthenticated RCE Vulnerability」：v2.11.4–2.37.1 经 Script Operator（`new Function` → `require`/`child_process`）未授权 RCE；v2.38.0 收紧 CORS、v2.38.2 修复浏览器路径；**API 仍无鉴权** | `[上游文档]` https://github.com/sub-store-org/Sub-Store/issues/634 · https://kabir.au/blog/uncovering-a-live-watering-hole-attack |
| 确认的第三方原文 | 「The privileged API remains unauthenticated and reachable to network clients or clients that omit the `Origin` header.」 | `[上游文档]` https://kabir.au/blog/uncovering-a-live-watering-hole-attack |
| `SUB_STORE_FRONTEND_BACKEND_PATH` | 是**前端反代 backend 的路径前缀**（须以 `/` 开头），与 `SUB_STORE_BACKEND_PREFIX`/`_MERGE` 配合时 backend 仅在该前缀下响应；报错原文：`SUB_STORE_FRONTEND_BACKEND_PATH must be set and start with "/" when SUB_STORE_BACKEND_PREFIX or SUB_STORE_BACKEND_MERGE is enabled` | `[上游文档]` https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/src/restful/index.js |
| 该机制的限制 | **只是 obscurity（随机路径），不是认证。** 路径一旦经订阅 URL 的 `?api=`、日志或 Referer 泄露即等同完全未授权访问 | `[推测]`（官方未声明其为访问控制）；`[未验证]` 官方安全声明 |
| 「不要暴露公网」官方原文 | README/wiki **未找到**明确官方声明（`[未验证]`）。最接近的官方告警是 CORS：**「Set the value to `*` only when you accept the risk of any website reading the local backend through browser CORS.」**（中文：「设为 `*` 可恢复旧的任意来源访问行为, 但任意网站都可能通过浏览器 CORS 读取本机 Sub-Store 后端响应, 不建议长期使用。」） | `[上游文档]` https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/README.md · .../config/README.md |
| 第三方建议 | 「不要把端口暴露公网；仅本地使用，不要 0.0.0.0 监听」 | `[上游文档]`（第三方）https://www.nodeloc.com/t/topic/106779 |
| CORS 默认 | allowlist `https://sub-store.vercel.app,http://substore.stash,https://substore.stash`；**无 `Origin` 头的请求被放行**（`if (!origin) return true;`） | `[上游文档]` https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/src/utils/cors.js |
| 速率限制 | 未发现官方实现或文档 | `[未验证]` |
| CVE/GHSA | 未发现登记条目（RCE 事件以官方 issue 形式处理） | `[未验证]`（本轮 NVD 查询未覆盖该产品名） |

**Agent 侧结论（供 ADR-005 采用）：**

```text
R12-SUBSTORE-1  Sub-Store 一律视为「本机不可信外部组件 / 可替换 Adapter」，
                不属于 Agent 的信任边界（TB-5），不得把 Agent 凭据交给它。
R12-SUBSTORE-2  绑定建议：SUB_STORE_BACKEND_API_HOST=127.0.0.1（或 ::1）+ 端口仅 loopback；
                需要 Web UI 时由 Agent 反代并加认证，绝不直接对外暴露 3000/3001。
R12-SUBSTORE-3  版本下限 ≥ 2.38.2（含）+ 禁用/不启用 Script Operator；
                默认优先 sub-store-convert 或 NativeConverter（AGENTS.md 已定 Port 抽象）。
R12-SUBSTORE-4  SUB_STORE_FRONTEND_BACKEND_PATH / 随机路径只作纵深防御，写入文档时
                必须明确「这不是认证」。
R12-SUBSTORE-5  Agent 调 Sub-Store 的 HTTP 客户端必须设置短超时、响应体上限，
                并对返回内容做「当不可信输入」处理（不 eval、不拼进 shell）。
```

---

## 6. Agent 接口安全（Unix socket / Web 认证 / SSRF）

### 6.1 Unix socket 权限方案

```text
路径   : /run/proxy-agent/agent.sock   (由 systemd RuntimeDirectory=proxy-agent 创建)
目录   : /run/proxy-agent              0750 root:proxy-admin   (RuntimeDirectoryMode=0750)
socket : /run/proxy-agent/agent.sock   0660 root:proxy-admin   (应用内 bind 后显式 chmod)
mihomo : /run/proxy-agent/mihomo.sock  0660 root:proxy-agent   (Agent 自己拉起 mihomo 时收紧)
```

要点与证据：

- `[上游文档]` unix(7)：pathname socket 的 `connect(2)` 需要**该 socket 文件的写权限**；socket 文件权限 = 创建时 `0777 & ~umask`；bind 后可 `chmod`/`chown`。`umask` 默认 `0022` ⇒ 必须先 `chmod(0o660)` 或 bind 前 `umask(0o007)`。
- `[实测]`：Node 创建 socket 后 `chmod 0660` 生效，同 uid 客户端 `connect` 返回 `200`；macOS 上无法在不 sudo 的前提下切换用户，因此「其他 uid 被拒绝」这一步为 `[上游文档]` 推导（POSIX 写权限语义），**未在本机跨 uid 复现**。
- **bind → chmod 之间存在权限过宽窗口**，因此 socket 应放在**他人不可写的私有目录**（§T-E2）。
- **授权只信 uid/gid**：`tokio::net::UnixStream::peer_cred() -> io::Result<UCred>`，`UCred::{uid(),gid(),pid()}`（Linux `pid()` 为 `Some`）。`[上游文档]` https://docs.rs/tokio/latest/tokio/net/struct.UnixStream.html · https://docs.rs/tokio/latest/tokio/net/unix/struct.UCred.html
- **不要在权限模型中使用 PID**：`SO_PEERCRED` 的 creds 是 `connect(2)` 时刻快照，PID 存在回收竞态；PID 级安全需 `SO_PEERPIDFD`（Linux 6.5+）。`[上游文档]` https://man7.org/linux/man-pages/man7/unix.7.html
- **axum 0.8 原生支持 UDS + connect info**（官方 example）：

```rust
impl connect_info::Connected<IncomingStream<'_, UnixListener>> for UdsConnectInfo {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        let peer_cred = stream.io().peer_cred().unwrap();
        Self { peer_addr: Arc::new(stream.io().peer_addr().unwrap()), peer_cred }
    }
}
```

`[上游源码]` https://raw.githubusercontent.com/tokio-rs/axum/main/examples/unix-domain-socket/src/main.rs · `[上游文档]` https://docs.rs/axum/latest/axum/serve/struct.IncomingStream.html

- **禁止使用抽象命名空间 socket（`@name`）**：`[上游文档]` unix(7) 明确其 `umask`/`fchown`/`fchmod` 全部无效，无法用文件权限做访问控制。
- **不要只依赖文件权限**：POSIX 不保证 socket 文件权限的跨平台语义（unix(7)：*"Portable programs should not rely on this feature for security"*）⇒ socket 权限 + `SO_PEERCRED` 双层。

最终方案（MVP）：

```text
 authorize(peer):
   if peer.uid() == 0                    -> Role::Admin         # root
   else if peer.gid() in {proxy-admin gid} -> Role::Admin
   else if peer.uid() in config.readonly_uids -> Role::ReadOnly
   else -> deny (401), 并记录审计 actor=unix:uid=<uid> result=denied
```

### 6.2 Web API 认证方案对比与 MVP 建议

| 方案 | 优点 | 缺点 | MVP |
|------|------|------|-----|
| **Bearer token（静态）** | 实现简单；CLI/脚本友好；无 CSRF 问题（非 cookie） | 需要安全存储与轮换；无浏览器会话管理 | ✅ **采用** |
| Session cookie + CSRF | 浏览器体验好；可服务端吊销 | 需 CSRF 防护、SameSite、会话存储；对 CLI 不友好 | Phase 2 |
| mTLS | 最强；可绑定设备 | 证书分发/轮换成本高；PVE LXC 用户负担大；mihomo 侧本可用但 Agent 侧复杂 | Phase 2（可选，反代提供） |
| 反代 Basic Auth | 零实现成本 | 凭据频繁重放；无角色；依赖反代配置正确性 | 接受作为「外层」，不代替 Agent 认证 |

**MVP 建议细节：**

```text
生成    : 首次启动若未配置 web token，则用 OS CSPRNG 生成 32 字节随机，
          以 base64url 呈现给用户一次（stdout/安装脚本/一次性文件），提示保存。
存储    : 只存 SHA-256(token)（或 Argon2id，若采用用户口令）；绝不回显明文；
          文件 /etc/proxy-agent/config.toml 权限 0640 root:proxy-agent。
比较    : 常量时间比较（subtle::ConstantTimeEq）；先对提交值做 SHA-256 再比较哈希。
标识    : 每个 token 有 token_id（如 tok_01，用于审计 actor=web:token:tok_01）。
轮换    : CLI `proxyctl auth token rotate` + 支持多 token（旧 token 宽限期后失效）；
          轮换操作本身写入审计。
传输    : 仅 HTTPS（经反代）或 loopback；Agent 不做 TLS 终止（MVP），
          但必须支持「监听非 loopback 时要求 token」。
```

**「远程可达时强制认证」的判定方式（必须可测）：**

```rust
// 启动期（bootstrap）硬校验，而不是运行期提示
fn assert_auth_posture(bind: SocketAddr, auth: &AuthConfig) -> Result<(), BootstrapError> {
    let is_loopback = bind.ip().is_loopback();          // 127.0.0.0/8, ::1
    let is_unix_only = matches!(bind_transport, Transport::UnixSocket);
    if !is_loopback && !is_unix_only && !auth.has_enabled_token() {
        return Err(BootstrapError::RemoteBindingRequiresAuth(bind));
    }
    Ok(())
}
```

- 判定输入必须是**实际 bind 的 `SocketAddr`**（`0.0.0.0`、`::`、具体内网 IP 全部视为「远程可达」），而不是配置文件字符串。
- `0.0.0.0`/`::`/非 loopback 且无 token ⇒ **拒绝启动**（fail-closed），并在日志给出明确原因。
- 反向代理场景：Agent 只信任来自 `127.0.0.1` 的连接，且**默认不信任** `X-Forwarded-For`（除非显式配置 `trusted_proxies`）；否则 IP 白名单可被伪造。
- `/api/v1/health` 是唯一可匿名端点（仅返回 `{"status":"ok"}`），其余全部要求 token；`/ws/v1/events` 要求 token 且**不使用 query string 传 token**（用 `Sec-WebSocket-Protocol` 或首帧认证），避免 token 进入访问日志。

**CORS 策略：**

```text
默认      : 不发送任何 CORS 头（同源部署；Web UI 由 Agent 自己托管）
需要跨源时: 显式 allowlist（配置项 web.allowed_origins），支持精确 origin，禁止 "*"
凭据      : 不开启 Access-Control-Allow-Credentials（因为不用 cookie）
私网访问  : 不发送 Access-Control-Allow-Private-Network（与 mihomo 默认相反）
预检      : 只允许 GET/POST/PATCH/DELETE + Content-Type/Authorization
```

### 6.3 输入校验与 SSRF

两个抓取发起方都要管：**Agent 自己**（订阅下载）与 **Sub-Store**（转换时抓取）。mihomo 也会抓取（`proxy-providers`/`rule-providers` 的 `url` 类型），因此 Agent 生成配置时必须校验这些 URL。

```text
S1 scheme      : 只允许 http/https（禁止 file/ftp/gopher/data/dict/unix）
S2 结构        : 禁止 URL userinfo（user:pass@host 视为凭据泄露 + 混淆）；
                 禁止非标准端口白名单外端口（建议只允许 80/443/8080/8443，可配置）
S3 解析后校验  : 必须自己解析 DNS 并拿到 IP 列表，对**每个** IP 判定；不信任域名字符串
S4 拒绝网段    : 0.0.0.0/8, 10/8, 100.64/10, 127/8, 169.254.0.0/16, 172.16/12, 192.0.0/24,
                 192.0.2/24, 192.168/16, 198.18/15, 198.51.100/24, 203.0.113/24, 224/4, 240/4,
                 255.255.255.255/32, ::/128, ::1/128, ::ffff:0:0/96(映射后再判), fc00::/7,
                 fe80::/10, ff00::/8, 2001:db8::/32, 64:ff9b::/96
                 特别点名：169.254.169.254（云元数据）、100.100.100.200（阿里云）、
                 fd00:ec2::254（AWS IPv6 元数据）
S5 重定向      : 最多 3 跳；每跳重新执行 S1-S4；跨 origin 重定向需再次校验
S6 大小/时间   : 响应体上限（默认 10 MiB）+ connect/read/total 超时 + 总并发上限
S7 接口白名单  : Sub-Store 侧只允许调用 Agent 已知的既有 endpoint 集合；
                 不接受用户在订阅里指定任意外部抓取 URL（避免把 Sub-Store 当 SSRF 跳板）
S8 代理边界    : 若允许经代理抓取，明确「经代理」不等于安全（代理可能在内网）；
                 元数据地址必须无条件拒绝，无论是否走代理
S9 收口实现    : 单个 `SafeFetcher` 适配器（Infrastructure），全项目唯一出口；
                 禁止在 UseCase/Handler 内直接 `reqwest::get`
```

`[实测]` 反证：mihomo 自身对 controller 的 `path` 有 `IsSafePath` 约束，但对 `url` 型 provider **没有**内网地址限制（`[推测]`，未逐行审计其 fetch 层）；因此 SSRF 防护必须由 Agent 侧负责，不能依赖 mihomo。

---

## 7. 最小权限表

图例：`✅ 允许` / `🚫 拒绝` / `🔶 有条件`（见备注列）

| 操作 | 本地 root | proxy-agent 用户 | 本地普通用户 | 远程 Web 用户 | 是否需要 root | 授权前提（备注） |
|------|-----------|------------------|--------------|----------------|----------------|------------------|
| 启动 Mihomo | ✅ | ✅ | 🚫 | 🔶 (Admin token) | **否** | 子进程以 Agent 自身 uid 运行；仅当配置含 `tun:`/`tproxy` 才需 `CAP_NET_ADMIN`；需持有实例锁 |
| 停止 Mihomo | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 需持有实例锁；幂等（已停止→成功 no-op + 审计） |
| 重启 Mihomo | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | = stop+start；需健康检查后再置为 running |
| 写配置版本（新增 vN） | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 只能写 `/var/lib/proxy-agent/configs/`；原子写（tmp+fsync+rename）；不修改 active |
| 激活/回滚配置 | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 必须先 `validate` 通过；切换后 reload + 健康检查；失败回滚；高风险 → 审计 |
| reload Mihomo（重载配置） | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 走 `MihomoController` Port（unix socket 优先）；需实例锁 |
| 校验配置（dry-run） | ✅ | ✅ | 🔶 (ReadOnly socket | 🔶 (ReadOnly token) | 否 | 纯计算 + 可选 mihomo `-t`；不落盘、不改状态；可只读角色 |
| 更新/替换 Mihomo 二进制 | 🔶 (经 usecase) | 🚫（无写权限） | 🚫 | 🔶 (Admin token) | **是**（写 `/usr/lib/proxy-agent`） | 下载→SHA256 校验→smoke test→切 symlink；安装目录 root 所有；**Web 触发时 Agent 需经受限提权通道**（见 §11） |
| 管理防火墙 / 策略路由（nftables/iptables/routing） | 🔶 (经 usecase) | 🔶（有 `CAP_NET_ADMIN` 时） | 🚫 | 🔶 (Admin token) | **是**（`CAP_NET_ADMIN`） | 只允许白名单规则模板；apply/revert 成对；规则带专属前缀；高风险 → 审计 |
| 读日志（Agent/ mihomo） | ✅ | ✅ | 🔶（仅自身 socket + ReadOnly） | 🔶 (ReadOnly token) | 否 | 必须 redact 后返回；默认不含 debug 原文 |
| 读订阅列表（含 URL 脱敏） | ✅ | ✅ | 🚫 | 🔶 (ReadOnly) | 否 | 默认只显示 `scheme://host/path/***`；明文 URL 需 Admin + 显式 `--show-secrets` 且审计 |
| 新增/修改/删除订阅 | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 写操作需 Admin；URL 需过 SSRF 校验；变更写审计 |
| 触发订阅更新 | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 单飞防重入；失败保留旧配置 |
| 读审计日志 | ✅ | ✅ | 🚫 | 🔶 (Admin) | 否 | 只读；不允许经 API 删除/修改 |
| 修改 Agent 自身配置（含 token 轮换） | ✅ | 🔶 (若允许) | 🚫 | 🔶 (Admin) | 建议是（MVP） | 轮换走 CLI + 本地 socket；Web 侧轮换列为 Phase 2 |
| TUN / TProxy 模式启用 | 🔶 | 🔶（有 cap 时） | 🚫 | 🔶 (Admin) | **是**（`CAP_NET_ADMIN` + `/dev/net/tun`） | 需 `doctor` 检测到 capability 且 LXC 允许；否则拒绝启用但不影响 HTTP/SOCKS |
| 修改 systemd unit / 安装服务 | 🚫（经安装脚本） | 🚫 | 🚫 | 🚫 | 是（离线执行） | 安装/升级属安装器职责，**绝不能**由 Web API 触发任意 unit 改写 |
| 任意命令执行 | 🚫 | 🚫 | 🚫 | 🚫 | — | **架构红线，永不存在** |

**「必须 root」的最小集合（明确结论）：**

```text
必须 root / 特权:
  - 安装或替换 /usr/lib/proxy-agent/ 下的二进制（写系统目录）
  - nftables / iptables / 策略路由 等内核网络配置（CAP_NET_ADMIN）
  - TUN 设备创建与配置（CAP_NET_ADMIN + /dev/net/tun 可访问）
  - 写 systemd unit、监听 <1024 端口（若需要，CAP_NET_BIND_SERVICE）
不需要 root:
  - Mihomo 进程的 start/stop/restart/reload（同 uid 子进程）
  - 配置版本读写、订阅抓取与转换、审计写库、日志读取
  - 绑定 9090 等高位端口（loopback），本地 Unix socket
```

因此推荐部署形态：`proxy-agent.service` 以**专用低权用户 `proxy-agent`** 运行，只通过 `AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE` 获得所需能力；**不要**以 `root` 运行整个 Agent，**不要**授予 `CAP_SYS_ADMIN`/`CAP_SYS_PTRACE`/`CAP_DAC_OVERRIDE`（注意：mihomo 官方 systemd 示例恰好授予了后三个，见 §9，Agent 不应照抄）。

---

## 8. 日志脱敏与审计规则

### 8.1 脱敏（Redaction）规则

设计文档已有的 `audit_logs` 结构（`id/action/actor/target/result/metadata/created_at`）+ `AGENTS.md` 日志禁令 → 落成可实现规则：

```text
R1  URL 统一处理：
    订阅 URL / webhook / provider URL 一律输出 safe_url(u)：
      scheme://host[:port]/path + (有 query ? "?<redacted>")，query 全部丢弃（不列举 key）
      host 保留（排障需要），userinfo 若存在 → 记录为 (userinfo-present) 并不输出
R2  字段名匹配（大小写不敏感，含分隔符归一化）：
    secret, token, access_token, refresh_token, password, passwd, pwd, api_key, apikey,
    authorization, auth, cookie, set-cookie, private_key, private-key, psk, uuid,
    subscription_url, url_with_credentials, proxy_password, sni? (否), session, csrf
    → 值替换为 "***REDACTED***"，并记录 `redacted_fields=[...]`（只记字段名）
R3  YAML/配置：只记录「key 路径 + sha256(值) + 长度」，不记录 value；
    Diff 输出时对敏感 key 只输出 modified/added/removed 而不输出内容
R4  HTTP：请求/响应头白名单记录（Content-Type、Content-Length、User-Agent 摘要）；
    Authorization/Cookie/Set-Cookie 永不记录（连长度都不记）
R5  进程：mihomo 子进程的命令行参数若含 `-secret` 之类，需在日志与审计中 redact
R6  错误：reqwest/URL 解析错误消息常含完整 URL ⇒ 统一经 safe_url() 后再写日志
R7  默认级别：RUST_LOG 默认 info；debug 需显式开启并在 UI 上给出「可能包含敏感信息」警告
R8  客户端镜像：CLI/TUI 默认不打印 secret（AGENTS.md 已要求）；`--show-secrets` 仅本地 TTY
```

必须 redact 的清单（来自任务 + AGENTS.md）：订阅 URL 中的凭据、Mihomo `secret`、认证 token、代理节点凭据（UUID/password/psk/私钥）、完整敏感配置内容、Cookie/Authorization 头。

### 8.2 审计日志应记录 / 不应记录

**应记录（append-only）：**

```text
who    : actor = local:uid=<n> | unix:uid=<n>,gid=<n>,pid=<n> | web:token:<token_id> | cli:uid=<n>
when   : created_at (UTC, RFC3339) + 可选 duration_ms
what   : action（枚举：mihomo.start/stop/restart/reload/update, config.validate/activate/rollback,
         subscription.create/update/delete/refresh, system.nftables.apply/revert, auth.token.rotate,
         auth.denied, doctor.run）
target : target（config:v41 / subscription:sub_01 / mihomo:default / firewall:ruleset:v3）+ 关键指纹
         （config_sha256、mihomo_version、binary_sha256）
result : success | failure | denied + error_code（枚举，不带原始错误文本中的敏感串）
metadata: request_id、client_ip（若远程）、user_agent 摘要、变化的字段名列表（不含值）
```

**不应记录：**

```text
- 订阅 URL 的 query（token/password 等）
- Mihomo secret / Agent token / 会话 cookie 的任何形式（含哈希用于日志）
- 节点 UUID、密码、psk、private-key、SNI 之外的凭据材料
- 完整配置内容（包括 redirect/rule 全文）
- 请求/响应体原文（除已知非敏感的小型元数据）
- 内网 IP 之外无需的信息？—— client_ip 可以记录（审计需要），但需在文档隐私说明中声明
```

**设计文档 audit_logs 需要补强的地方：**

| # | 现状（设计文档 §57 / §34） | 补强建议 |
|---|---------------------------|----------|
| 1 | 只有 `action/actor/target/result/metadata` | 增加 `request_id`、`error_code`、`config_sha256`、`mihomo_version`、`client_ip`、`duration_ms` |
| 2 | `actor=local-admin` 这类模糊标识 | 改为可归因的 `source:identity` 形式（见「应记录」），并区分 `denied` |
| 3 | 只列了 8 个高风险 action | 补齐 `auth.token.rotate`、`auth.denied`、`config.validate`、`system.nftables.revert`、`mihomo.file.write` |
| 4 | 未说明不可篡改 | 明确 append-only（应用层无 update/delete 路径）、DB 文件权限、同时输出到 journald |
| 5 | 未说明脱敏 | 明确 metadata/diff 一律不含值（R2/R3） |
| 6 | 未说明保留与轮转 | 增加保留策略（默认 90 天或 N 条）、`audit_logs` 索引与导出命令 |

---

## 9. systemd 沙箱安全建议（与 R09 呼应）

> **参考对照：mihomo 官方 systemd 示例**（`[上游文档]` https://wiki.metacubex.one/startup/service/）授予了
> `CapabilityBoundingSet`/`AmbientCapabilities = CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE CAP_SYS_TIME CAP_SYS_PTRACE CAP_DAC_READ_SEARCH CAP_DAC_OVERRIDE`。
> 其中 `CAP_SYS_PTRACE`（可读进程内存/注入）、`CAP_DAC_OVERRIDE`（绕过所有文件权限）、`CAP_SYS_TIME`（改系统时间）**远超代理所需**，Agent 不应照抄。本节给出的是 Agent 侧（`proxy-agent.service`）建议，并与 R09 的 unit 设计交叉引用。

只列安全相关指令与风险（不重复整份 unit）：

| 指令 | 建议值 | 作用 | 风险 / 注意 |
|------|--------|------|-------------|
| `User=` / `Group=` | `proxy-agent` / `proxy-agent` | 进程不跑 root | 需要特权操作时靠 capability 而非 uid；`proxy-agent` 用户不得在 docker 组/sudo 组 |
| `DynamicUser=` | **否**（用固定用户） | — | DynamicUser 会让 socket 属主/持久目录 uid 漂移，审计不可归因 |
| `AmbientCapabilities=` | `CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE` | 仅授予实际所需 | **不要** `CAP_SYS_ADMIN`/`CAP_SYS_PTRACE`/`CAP_DAC_OVERRIDE`/`CAP_SYS_TIME`；TUN 不可用时连 `CAP_NET_ADMIN` 都可省 |
| `CapabilityBoundingSet=` | 同上（收紧上界） | 防止运行期通过 file caps 提权 | 与 Ambient 保持一致，避免「上界很宽」 |
| `NoNewPrivileges=yes` | `yes` | 禁止 setuid/file-cap 提权 | 与需要内部提权的更新流程冲突 ⇒ 更新应由独立的最小 helper（见 §11） |
| `ProtectSystem=strict` | `strict` | `/usr`、`/boot`、`/etc` 只读 | 与「写 `/etc/proxy-agent/config.toml`」冲突 ⇒ 用 `ReadWritePaths=/etc/proxy-agent /var/lib/proxy-agent /run/proxy-agent` 精确开口；**不要**为图省事用 `ProtectSystem=no` |
| `ProtectHome=yes` | `yes` | 隐藏 `/home`、`/root` | 用户可能把订阅/密钥放 home ⇒ 需文档说明，不放宽 |
| `PrivateTmp=yes` | `yes` | 独立 `/tmp` | **注意**：若 mihomo 工作目录/`SAFE_PATHS` 在 `/tmp` 内（如本项目实验那样）会失效；生产必须用 `/var/lib/proxy-agent` |
| `PrivateDevices=yes` | `yes`，若需 TUN 则 `no` + `DeviceAllow=/dev/net/tun rw` | 限制设备访问 | 开 TUN 时不能开 PrivateDevices；两模式二选一，由 `doctor` 决定 |
| `ProtectKernelTunables=yes` / `ProtectKernelModules=yes` / `ProtectControlGroups=yes` | 全 `yes` | 减少内核面 | 若需要写 `/proc/sys/net/...`（TProxy/转发）需 `ReadWritePaths` 或放弃该项 ⇒ 明确取舍 |
| `RestrictNamespaces=yes`（或 `~user`） | `yes` | 禁止创建 namespace | 若未来用 netns 隔离抓取，则需放开对应项 |
| `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK` | 显式 | 只允许所需协议族 | 缺 `AF_NETLINK` 会导致 nftables/接口枚举失败；缺 `AF_PACKET` 时 DGRAM 抓包不可用（一般不需要） |
| `RestrictSUIDSGID=yes` | `yes` | 禁止 setuid/setgid 文件创建 | 与安装器解耦（安装器不是本服务） |
| `LockPersonality=yes` / `MemoryDenyWriteExecute=yes` | `yes` | 防 ROP/JIT 型利用 | 若依赖 JIT（本项目 Rust 无）无影响；`MemoryDenyWriteExecute` 可能影响某些 `seccomp` 库 |
| `SystemCallFilter=@system-service` + `~@privileged @obsolete` | 建议 | 缩小 syscall 面 | 若 mihomo/TUN 需要额外 syscall，需要 `SystemCallFilter=@system-service @network-io` 之类放宽并验证；**必须实测**（`[推测]`） |
| `SystemCallArchitectures=native` | `native` | 禁 32 位 syscall | 无风险 |
| `RestrictRealtime=yes` / `RestrictFileSystems=`（v253+） | `yes` / 可选 | 进一步收紧 | `RestrictFileSystems` 会限制所有 fs 访问，需仔细配 |
| `UMask=0022`（默认） | 保持 `0022`，由服务自己 `chmod 0660` socket | 避免全局 umask 影响 | 也可用 `UMask=0007` 配合 `RuntimeDirectoryMode=0750`；两者取一，必须与 §6.1 一致 |
| `RuntimeDirectory=proxy-agent` / `RuntimeDirectoryMode=0750` | 明确 | `/run/proxy-agent` 归 `proxy-agent` 所有、停止即删 | `RuntimeDirectoryMode` **只作用于目录**，socket 文件仍需服务内 `chmod(0o660)`（`[上游文档]` systemd.exec） |
| `_ReadWritePaths_` | `/etc/proxy-agent /var/lib/proxy-agent /run/proxy-agent` | 精确最小写权限 | 不要包含 `/usr/lib/proxy-agent`（二进制更新走 helper） |
| `ProtectProc=invisible` / `ProcSubset=pid` | 建议 | 隐藏其他进程信息 | 依赖 `/proc` 的 capability 检测需实测 |
| `WatchdogSec=` | 建议设（如 30s） | 卡死自动重启 | 需应用实现 `sd_notify`（`Type=notify`） |
| `LimitNOFILE=` | 按规模设（不要照抄 1000000） | 防资源耗尽 | 过小会限制连接数；过大放宽 DoS 面 |

---

## 10. MVP 安全基线（checklist，可映射成测试）

> 每条格式：`ID | 要求 | 验证方式`

**A. Mihomo 暴露面（9 条，其中 6 条本轮已 `[实测]`）**

1. `SEC-M01` 生成的配置中 `external-controller` host 必须为 `127.0.0.1` 或 `[::1]`，**禁止**空 host（`":9090"`）与 `0.0.0.0`/`::`/公网 IP。 → 单测：解析生成 YAML 并断言 host；`[实测]` `":19097"` 会绑 `[::]`。
2. `SEC-M02` `secret` 必须为**非空**、随机 ≥128-bit，且写入配置后 Agent 侧保留一份（加密或 0600 文件）。 → 单测 + 集成：起 mihomo 后断言无 token 请求返回 401。`[实测]`
3. `SEC-M03` 绝不在配置中出现 `allow-origins: ['*']`；若使用 mihomo controller 的 HTTP 端口，必须显式收敛 CORS（或干脆依赖 unix socket 关闭 TCP）。 → 单测：断言生成配置。`[实测]` 默认是 `*`。
4. `SEC-M04` 优先 `external-controller-unix`，且 Agent 在启动 mihomo 后把 socket `chmod 0660`（或直接不使用 0666 的默认）。 → 集成：断言 socket mode。`[实测]` 默认 0666。
5. `SEC-M05` 客户端访问 mihomo 时**始终**发送 `Authorization: Bearer <secret>`；包括 unix socket（防止未来 mihomo 变更或代理层）。 → 集成。
6. `SEC-M06` 强制 mihomo 版本 ≥ 1.19.12（建议最新 stable，本轮实测 1.19.30）；版本低于下限时 `doctor` 报警并拒绝更新以外的操作。 → 启动检查 + 单测。`[上游文档]` CVE-2025-56499。
7. `SEC-M07` 生成配置时拒绝用户可控的 `type: file` provider 的任意 `path`；只允许 Agent 生成的固定文件名（`R12-MIHOMO-1`）。 → 单测：注入 `path: /etc/passwd` 必须被 Agent 前置拒绝。`[实测]` mihomo 侧会 400，但白名单内文件仍可读。
8. `SEC-M08` mihomo 工作目录内不得存在 Agent 凭据文件；`SAFE_PATHS` 只含该工作目录。 → 集成：扫描工作目录断言无 secret/token/db。
9. `SEC-M09` Agent 回传 `/api/v1/logs` 前必须过 redact，且不返回 debug 原文。 → 单测：注入含 `password=`/`secret:` 的日志行，断言输出被替换。`[实测]` mihomo `/logs` 是原文流。

**B. Agent 自身接口（10 条）**

10. `SEC-A01` Unix socket 路径 `/run/proxy-agent/agent.sock`，目录 `0750 root:proxy-admin`，socket `0660 root:proxy-admin`；**禁止**抽象命名空间。 → 集成：`stat` 断言 + 代码审查。
11. `SEC-A02` 启动时校验真实 bind 地址：非 loopback 且无 enabled token ⇒ 拒绝启动（fail-closed）。 → 单测：`assert_auth_posture()` 各分支。
12. `SEC-A03` token 只存哈希（SHA-256 或 Argon2id）；日志/API/DB 中不出现明文。 → 单测 + grep 集成。
13. `SEC-A04` token 比较使用常量时间实现，且先哈希再比较。 → 代码审查 + 单测。
14. `SEC-A05` 提供 `auth token rotate` CLI 且轮换写审计；支持旧 token 宽限期。 → 集成。
15. `SEC-A06` CORS 默认关闭；启用时只允许显式 allowlist，禁止 `*` 与 `Access-Control-Allow-Private-Network`。 → 集成：带恶意 Origin 的预检必须无 ACAO。
16. `SEC-A07` `/api/v1/health` 是唯一匿名端点，其余（含 WebSocket）必须认证；`/ws/v1/events` 不得用 query string 传 token。 → 集成：逐端点断言 401/200。
17. `SEC-A08` 所有 Web/API handler 不得存在任意命令执行路径；命令执行只出现在 Infrastructure 的 `ProcessManager`，参数为枚举/结构化类型。 → 代码审查 + `cargo deny` 检查 `Command::new` 调用点白名单。
18. `SEC-A09` 只读角色（`READ_ONLY`）可读状态/日志/配置列表，但所有写操作返回 403 且写 `auth.denied` 审计。 → 集成 RBAC 矩阵测试。
19. `SEC-A10` 不信任 `X-Forwarded-For`（除非显式配置 `trusted_proxies`）；代理场景下只允许来源 `127.0.0.1`。 → 集成。

**C. 输入校验 / SSRF（5 条）**

20. `SEC-S01` 所有外部抓取经唯一 `SafeFetcher` 适配器；禁止其他 `reqwest::Client` 直连出口。 → 代码审查 + clippy 自定义 lint/`grep` 单测。
21. `SEC-S02` 拒绝非 http/https scheme、URL userinfo、超出端口的 URL。 → 单测表驱动（含 `file:///etc/passwd`、`http://user:pass@h/`）。
22. `SEC-S03` 解析后对**每个 IP** 执行私网/元数据网段拒绝（含 `169.254.169.254`、`100.100.100.200`、`fd00:ec2::254`）。 → 单测（无需真实网络，注入解析结果）。
23. `SEC-S04` 重定向 ≤3 且每跳重校验；响应体上限与超时生效。 → 单测/集成（本地 stub server）。
24. `SEC-S05` 配置中的 `proxy-providers`/`rule-providers` 的 URL 同样过 `SEC-S01`–`SEC-S04`。 → 单测。

**D. 审计与脱敏（5 条）**

25. `SEC-L01` `redact()` 覆盖字段名清单（§8.1 R2）并有单测覆盖每个字段名。 → 单测。
26. `SEC-L02` URL 输出永远丢弃 query 与 userinfo（`safe_url`）。 → 单测。
27. `SEC-L03` 审计写入覆盖 §8.2 的 action 枚举，含 `result=denied` 路径；写入失败不得静默。 → 集成。
28. `SEC-L04` 审计表 append-only（无 update/delete 代码路径）；DB 文件 `0640 root:proxy-agent`。 → 代码审查 + 集成断言权限。
29. `SEC-L05` 高风险操作审计含 `config_sha256`/`binary_sha256`/`request_id`。 → 集成。

**E. 系统与供应链（3 条）**

30. `SEC-P01` mihomo 二进制安装目录与文件 `root:root`，下载后强制 SHA256 校验，校验失败不切换 `current`。 → 集成（stub 下载）。
31. `SEC-P02` systemd unit 含 §9 的沙箱指令，且 `proxy-agent` 不具 `CAP_SYS_ADMIN`/`CAP_SYS_PTRACE`/`CAP_DAC_OVERRIDE`。 → `systemd-analyze security`（人类可读评分）+ unit 文本断言。
32. `SEC-P03` Agent 生成/写入的 mihomo 配置中不出现 `external-ui` 指向可写目录（防止 UI 被替换为 XSS 载体）；如需 dashboard，静态资源目录只读且由 Agent 校验哈希。 → 单测 + 代码审查。

**合计：32 条。**

---

## 11. 对 Agent 架构的影响（Auth Port / Secret 处理）

### 11.1 建议新增的 Port（Application 层定义，Infrastructure 实现）

```rust
// 认证与授权（不依赖 axum；由 interfaces 适配）
#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, credentials: Credentials) -> Result<Principal, AuthError>;
    fn authorize(&self, principal: &Principal, op: Operation) -> AuthorizationDecision;
}

pub enum Credentials {
    UnixPeer { uid: u32, gid: u32 },          // 来自 SO_PEERCRED（interfaces 提取，application 决策）
    BearerToken { token_hash: [u8; 32] },     // interfaces 先做 SHA-256，避免明文进入 application
}

pub struct Principal {
    pub id: PrincipalId,                       // local:uid=0 / unix:uid=1000 / web:token:tok_01
    pub role: Role,                            // Admin | ReadOnly
    pub kind: PrincipalKind,
}

// 秘密存储（token/secret 的生成、哈希、轮换、读取）
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn generate(&self, name: SecretName, policy: SecretPolicy) -> Result<SecretRef>;
    async fn verify(&self, name: SecretName, candidate_hash: &[u8]) -> Result<bool>;
    async fn rotate(&self, name: SecretName) -> Result<SecretRef>;
    async fn get(&self, name: SecretName) -> Result<Secret>;   // 仅 Infrastructure 可调用
}

// 出站抓取（SSRF 安全收口）
#[async_trait]
pub trait SafeFetcher: Send + Sync {
    async fn fetch(&self, req: SafeFetchRequest) -> Result<SafeFetchResponse>;
}

// 审计（append-only）
#[async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(&self, entry: AuditEntry) -> Result<()>;
}
```

要点：

- `Credentials` 用**枚举**而不是字符串，确保「任意命令/任意路径」无法经认证层进入 Application。
- `BearerToken` 只携带 hash，明文只在 interfaces 层短暂存在（且不写日志）。
- `UnixPeer` 只使用 uid/gid，**不带 PID**（§6.1）。
- `Operation` 为枚举（`MihomoStart`、`ConfigActivate`、`FirewallApply`…），`authorize()` 是纯函数 ⇒ Domain/Application 可单测，符合 `AGENTS.md` 依赖方向。

### 11.2 Secret 处理规则

```text
1. 生成：CSPRNG ≥32B；mihomo secret 与 Web token 分开生成、分开存储、分开轮换。
2. 存储：只存哈希（Web token）；mihomo secret 需要还原成明文写入 mihomo 配置 ⇒
   存于 /etc/proxy-agent/secrets/（0640 root:proxy-agent）或 SQLite 加密列，
   不得进入日志/审计/API 响应/DTO。
3. 传递：写 mihomo 配置时以内存字符串拼接/序列化；不要经 argv（进程列表可见）；
   不要经环境变量（/proc/<pid>/environ 同 uid 可读）——优先写入 0600 的 config 文件。
4. 轮换：mihomo secret 轮换 = 原子写新配置 + reload（失败回滚旧配置与旧 secret）；
   Web token 轮换见 SEC-A05。
5. 脱敏：所有出站字符串（日志、审计、HTTP 错误、panic message）必须过 redact()；
   建议给敏感类型实现自定义 Debug（打印 "***"）而不是靠人肉记得 redact。
6. 清理：临时文件用 O_TMPFILE/unlink-after-open；不落盘的 secret 用 zeroize 清零。
```

### 11.3 对 bootstrap / UseCase / DTO 的约束

- **bootstrap**：启动顺序 = 读配置 → `assert_auth_posture()`（§6.2）→ 初始化 SecretStore → 创建 socket 并 chmod → 启动 mihomo（生成含非空 secret、loopback host 的配置）→ 才开始接受请求。
- **UseCase**：每个特权 UseCase 的入参必须包含 `Principal`（或由 interfaces 注入），并在 UseCase 内调用 `authorize()`；**授权判断不放在 Handler**（防止遗漏端点）。
- **DTO**：不允许 `SubscriptionResponse { url: String }` 直出原始 URL；应为 `url_redacted: String` + `has_credentials: bool`，明文仅在 `GET /api/v1/subscriptions/:id/secret`（Admin + 审计）单独提供。
- **二进制更新（需 root 的部分）**：建议独立最小 helper（systemd `proxy-agent-updater.service`，`Type=oneshot`，仅接受「已校验的版本号」参数），Agent 经受限通道触发；**绝不**让 Web API 传任意命令或任意 URL。这与 `NoNewPrivileges=yes` 兼容。

---

## 12. 证据与来源

### 12.1 上游源码（本轮回读的具体文件与行号）

| 文件 | 关键行 | 内容 |
|------|--------|------|
| `hub/route/server.go` | 119-122 | `if secret != "" { r.Use(authentication(secret)) }` |
| `hub/route/server.go` | 277-291 | unix socket：`syscall.Unlink` → `Listen("unix")` → `os.Chmod(addr, 0o666)` → `router(..., "", ...)` |
| `hub/route/server.go` | 299-327 | Windows named pipe 同样传空 secret |
| `hub/route/server.go` | 330-334 | `safeEqual` = `subtle.ConstantTimeCompare` |
| `hub/route/server.go` | 336-365 | `authentication()`：WebSocket `?token=` 分支 + `Authorization: Bearer` 分支 |
| `hub/route/server.go` | 170-187 | `if len(cfg.Addr) > 0` 才监听 TCP controller |
| `config/config.go` | 103-118, 419-429 | Controller/Cors 结构与 YAML tags |
| `config/config.go` | 595-598 | `ExternalControllerCors: RawCors{AllowOrigins: []string{"*"}, AllowPrivateNetwork: true}` |
| `constant/path.go` | 36-52, 80-105 | `SetHomeDir`/`SAFE_PATHS` 解析、`Resolve`、`IsSafePath`、`SafePaths` |
| `hub/route/configs.go` | 约 400-427 | `/configs` PUT：`req.Payload` 直解；`req.Path` 必为绝对路径且 `IsSafePath` |

源码来源（master 用于行为比对，v1.19.30 用于版本一致性）：
`https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/hub/route/server.go`、
`https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/config/config.go`、
`https://cdn.jsdelivr.net/gh/MetaCubeX/mihomo@v1.19.30/hub/route/server.go`、
`https://cdn.jsdelivr.net/gh/MetaCubeX/mihomo@v1.19.30/config/config.go`、
`https://cdn.jsdelivr.net/gh/MetaCubeX/mihomo@v1.19.30/constant/path.go`、
`https://cdn.jsdelivr.net/gh/MetaCubeX/mihomo@v1.19.30/hub/route/configs.go`。

### 12.2 上游文档

- mihomo 全局配置（含各 controller 项示例与「Unix socket / Windows namedpipe / DoH 不验证 secret」警告）：https://wiki.metacubex.one/config/general/ · https://wiki.metacubex.one/en/config/general/
- mihomo systemd 示例（含能力集）：https://wiki.metacubex.one/startup/service/
- Sub-Store backend 入口与默认监听：https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/src/restful/index.js
- Sub-Store README / config README（CORS 告警）：https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/README.md · https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/config/README.md
- Sub-Store CORS 实现：https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/backend/src/utils/cors.js
- Sub-Store 未授权 RCE issue #634：https://github.com/sub-store-org/Sub-Store/issues/634
- 第三方分析（含「API 仍无鉴权」原文）：https://kabir.au/blog/uncovering-a-live-watering-hole-attack
- Sub-Store 部署建议（第三方，勿暴露公网）：https://www.nodeloc.com/t/topic/106779
- unix(7)（socket 权限、`SO_PEERCRED`、abstract namespace）：https://man7.org/linux/man-pages/man7/unix.7.html
- systemd.exec（`RuntimeDirectory*`、`UMask`、沙箱指令）：https://manpages.ubuntu.com/manpages/noble/man5/systemd.exec.5.html
- tokio `UnixStream::peer_cred` / `UCred`：https://docs.rs/tokio/latest/tokio/net/struct.UnixStream.html · https://docs.rs/tokio/latest/tokio/net/unix/struct.UCred.html
- axum UDS + connect info 官方 example：https://raw.githubusercontent.com/tokio-rs/axum/main/examples/unix-domain-socket/src/main.rs
- CVE-2025-56499：https://nvd.nist.gov/vuln/detail/CVE-2025-56499 · https://security.snyk.io/vuln/SNYK-GOLANG-GITHUBCOMMETACUBEXMIHOMORULESPROVIDER-14054326 · https://volerion.com/vulnerabilities/CVE-2025-56499 · https://github.com/Cherrling/CVE-2025-56499
- CVE-2024-5732（原版 Clash/Windows）：https://www.tenable.com/cve/CVE-2024-5732
- CVE-2025-9474（Mihomo Party/macOS）：NVD `keywordSearch=mihomo`

### 12.3 本轮实测环境与方法

```text
主机        : macOS 26 (Darwin arm64)
mihomo      : MetaCubeX/mihomo v1.19.30（darwin-arm64，官方 Release，经 ghfast.top 代理下载）
              Mihomo Meta v1.19.30 darwin arm64 with go1.26.6 / with_gvisor
容器        : Docker (OrbStack 29.4.0)，本地已有镜像 postgres:16-alpine (arm64)；
              ghfast.top 可下载 linux-amd64 "compatible"(musl static) 构建，
              经 OrbStack/Rosetta 在 arm64 容器中成功执行（-v 输出正常）。
实验内容    : 配置变体矩阵（无 secret / 空 secret / 非空 secret / 无 cors 块 /
              allow-origins 空 / unix-only / 冒号开头的 host）+ HTTP 状态码与 CORS 头抓取 +
              socket 文件权限 stat + SAFE_PATHS 路径穿越探测。
清理        : 两个实验目录（/tmp/r12-sec、/tmp/r12b）与容器（r12a/r12b）已全部删除；
              未做任何对公网的扫描或攻击性请求；未读取 ~/.ssh 或用户凭据。
```

### 12.4 本地项目内证据

- `AGENTS.md`「Security Rules」（privilege separation、Mihomo controller 默认 loopback/unix、Unix socket 权限、Web 认证、日志禁令）。
- `AGENTS.md`「State and Concurrency」「Process Management」（生命周期锁、`ProcessManager` Port、禁止 handler 内执行命令）。
- 设计文档 §23 systemd、§24 Web API、§30 安全模型、§31 Authentication、§32 存储、§34 `audit_logs`、§38 Mihomo 更新流程（含 SHA256 校验）、§56 Security Boundary、§57 Audit Log。
- `docs/phase-0-architecture-discovery.md` 第 623-647 行（R12 范围定义）。

---

## 13. 未验证假设与开放问题

### 13.1 未验证（需实施阶段确认）

| # | 项 | 说明 |
|---|----|------|
| U1 | GitHub Security Advisories 对 `MetaCubeX/mihomo` 的完整列表 | 本次 `api.github.com` 限流（core=0）且 `ghfast.top` 代理对 `github.com/*/security/advisories` 返回 403 ⇒ **无法确认「除 CVE-2025-56499/CVE-2024-5732/CVE-2025-9474 外无其他 advisory」**。建议 CI 中加 advisory 轮询/`cargo audit` 替代人工核查。 |
| U2 | WebSocket `?token=` 路径在 v1.19.30 的实际可用性 | `[实测]` 带 `Upgrade: websocket` + 正确 `?token=` 仍返回 401（源码逻辑本应放行）。**无法判定**是 curl 未完成握手、代理/Darwin 差异还是 1.19.30 行为变化。→ Agent 侧结论不受影响：**用 `Authorization` 头或 `Sec-WebSocket-Protocol`，不要用 query token**。 |
| U3 | 跨 uid 的 socket 文件权限拒绝 | macOS 无法在不 sudo 前提下切换用户 ⇒ 「其他 uid 因缺少写权限而 connect 失败」仅由 unix(7) 语义推导，**未在本机复现**。Linux 实施阶段应在容器/VM 中用两个 uid 复现。 |
| U4 | Sub-Store 官方「不要暴露公网」原文 | 官方 README/wiki 中**未找到**该明确声明（只有 CORS 警告）；「勿暴露公网」来自第三方。 |
| U5 | Sub-Store 默认 ENTRYPOINT / Dockerfile | 仓库根未取到 Dockerfile（404）；可确认发布产物为 `backend/sub-store.min.js` + `node sub-store.min.js`。frontend server 仅在设置 `SUB_STORE_FRONTEND_PATH` 时启动 ⇒ 裸跑只监听 3000。 |
| U6 | Sub-Store 是否有速率限制 | 未发现官方实现或文档。 |
| U7 | mihomo 对 `proxy-providers`/`rule-providers` 的 **URL** 是否有内网/元数据地址防护 | 未逐行审计其 fetch 层；本文按「无防护」处理（fail-safe）。 |
| U8 | `systemd-analyze security` 对建议 unit 的实际评分与 `SystemCallFilter=@system-service` 对 mihomo/TUN 的兼容性 | 需在 Debian/Ubuntu + systemd 真机实测；本轮 macOS 无法验证。 |
| U9 | PVE LXC（privileged/unprivileged）下 `AmbientCapabilities`/`ProtectSystem`/`PrivateDevices` 的实际可用组合 | 需在 PVE LXC 实测（与 R09/R10 交叉）。 |
| U10 | mihomo `client-auth-type`/`client-auth-cert`（mTLS）的证书校验细节与轮换行为 | 未实测；Phase 2 评估。 |

### 13.2 开放问题（需 ADR / 决策）

1. **Web API 是否在 MVP 强制 TLS？**（本文建议：Agent 不做 TLS 终止，非 loopback 必须有 token + 建议反代 TLS；是否需要内置 TLS 由 ADR 决定。）
2. **mihomo secret 是否落库？** 需在「可轮换性」与「明文存储风险」间取舍：建议 `/etc/proxy-agent/secrets/`（0600/0640）+ 文件即真相，DB 只存引用。
3. **是否允许用户自定义 mihomo 配置片段（含 `file` provider / `script`）？** 若允许，必须定义 AST 级白名单；否则应仅允许 Agent 结构化生成的字段（强烈建议后者）。
4. **Sub-Store 的部署方式**：由 Agent 拉起（则 Agent 必须生成 `SUB_STORE_BACKEND_API_HOST=127.0.0.1`）还是完全外部（则 Agent 只做 HTTP 客户端）？影响 R12-SUBSTORE-2 的落实位置。
5. **多用户/多租户**：MVP 是否需要多角色（现设计仅 `ADMIN`/`READ_ONLY`）？若需要，`Principal` 与 `authorize()` 需前置设计以支持 scoped token。
6. **审计保留与隐私**：是否记录 `client_ip`；保留多久；是否需要导出/签名（影响 §8.2）。
7. **二进制更新的提权路径**：独立 oneshot helper vs `pkexec` vs 安装器重启服务——需 ADR（与 `NoNewPrivileges` 强相关）。
8. **`external-ui` / metacubexd 的凭据存放**：SPA 需把 API secret 存 localStorage（XSS 即失守）还是由 Agent 反代注入（更安全但改动 dashboard）；建议 Agent 自托管 UI + 服务端注入，需与 R08（dashboard）协同决策。
