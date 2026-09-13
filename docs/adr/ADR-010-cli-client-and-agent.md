# ADR-010：CLI 客户端与 Agent 的形态

## Status

Accepted（阶段 B 实现）

## Context

AGENTS.md 已规定 CLI 是 Interface Adapter，并给出目标形态：

```text
proxyctl
   |
Unix socket
   |
proxy-agent
```

但有几件事它没有回答，必须在实现阶段决定：

1. **一个 bin 还是两个。** 业界两种都有：`docker` ↔ `dockerd` 是两个二进制加一个 socket；`systemctl` ↔ `systemd` 是两个二进制加 D-Bus；`podman` 是**单二进制、无守护进程**（`podman ps` 直接读本地状态）。
2. **CLI 与内核之间走什么。** 选项是「CLI 直接调用 Application use case」（`--direct`）或「一律走 Agent 的 socket」。
3. **退出码怎么划分。** AGENTS.md 只要求「区分成功与运行失败」，没有给具体码。

阶段 A 已经交付了 socket 上的 HTTP interface（`f2b2bab`），因此问题 2 有一个已经存在的答案可以对齐。

## Decision

### D1：只有一个二进制 `proxyctl`，承担两个角色

```text
proxyctl <command>      客户端：发起一次请求，打印响应
proxyctl agent run      守护进程：组装 context，服务 socket
```

发布物只有一个；`packaging/systemd` 里是**一个 unit、两个进程**（常驻 `proxyctl agent run`，短命 `proxyctl <command>`）。

### D2：socket 是唯一通路，不提供 `--direct`

即使同一个二进制里已经链接了 Application 与 Infrastructure，客户端也**不直接调用 use case**。三条性质依赖这个约束：

- **访问控制边界。** socket 的 `0660` 权限 + `SO_PEERCRED` 校验是 ADR-005 D3 划定的唯一边界。直连没有 peer，也没有文件权限。
- **串行化。** 每实例锁（`InstanceLocks`）是**进程内**的。直连会落在另一个进程的锁表里，可能与 Agent 自己的操作交错。
- **单一契约。** 所有操作都经过 API 被测到；第二个入口不会被任何测试覆盖。

### D3：单个二进制里，客户端与守护端在源码上分离，并由测试强制

`crates/cli/src/client/**` 与 `command/**` 不得命名 `proxy_application` / `proxy_domain` / `proxy_bootstrap`，也不得依赖 `agent`/`dispatch`。这条规则写在 `crates/cli/tests/architecture.rs` 里，读源码文本判定，违反即构建失败。

同一组测试还把客户端构造的每一条路径与服务端路由表逐条比对，因此改路由名会在同一个提交里失败。

### D4：退出码

| 码 | 含义 | 来源 |
|---|---|---|
| 0 | 成功 | 2xx |
| 1 | 未归类的失败 | 其它 4xx/5xx，以及本地渲染失败 |
| 2 | 用法错误 | clap；也包括本地输入不可读（如 `config validate` 的文件不存在） |
| 3 | 目标不存在 | 404 |
| 4 | 与当前状态冲突 | 409 / 412 / 422 |
| 5 | 不允许 | 401 / 403 |
| 6 | 依赖不可达 | 502 / 504，以及连不上 Agent socket |
| 7 | 尚未实现 | 501，以及 `logs` |

全部小于 100，与「被信号杀死」（`128 + n`）可区分。

### D5：`--json` 原样透传响应体

`--json` 把服务端的字节**直接写到 stdout**，不在本地反序列化再序列化。理由：本地重建意味着这个 crate 成为响应形状的第二份定义，而两份定义会漂移。已在真实机器上验证：`--json status` 的输出与原始 HTTP 响应体逐字节相同。

### D6：`logs` 曾明确报告未实现（**已随 observer 落地**）

阶段 B 时 `logs` / `logs -f` 打印 `LOGS_NOT_IMPLEMENTED` 并以 7 退出，因为 observer port 尚无实现。
留空并返回 0 会比这更糟：脚本会把沉默当成「没有日志」。

**2026-09-12 更新**：`KernelObserver` 已实现（ADR-003 D7），`logs` 已是真实流式命令，
退出码 7 不再由它使用；无内核时返回 6（依赖不可达），级别非法时返回 1（用法类失败）。
7 保留在码表中，但当前无命令使用。

`logs` 不是 `Command`：其他命令都是「发一次请求、渲染一次响应」，而它保持连接并在行到达时逐行打印。
把它塞进 trait 会让每个命令都背上一个它没有的流式关切。

### D7：配置来源与优先级

`proxyctl agent run` 的配置有四级来源，从高到低：

```text
① --config / --flag        显式、一次性、覆盖一切
② 环境变量 PROXYCTL_*      容器与 CI
③ /etc/proxy-agent/config.toml   部署配置（dpkg conffile）
④ 内置默认值
```

- **flag 高于文件**：排障时「临时改一个值试试」必须不需要动配置文件；反过来会让 `--root` 这类开发参数失效。
- **环境变量夹在中间**：编排系统注入比挂文件方便，但它不该压过显式 flag。
- **生效值必须可观测**：启动时打印最终配置，且每个字段标注来源（`[flag]` / `[env]` / `[file]` / `[default]`）。现场最常见的失败是「我改了配置但没生效」，没有来源标注只能靠猜。`--print-config` 做同样的事并退出。

**格式**：TOML（本 ADR 沿用 ADR-006 D4 的既定选择；注释友好，§4 要给人手写）。每个 `[section]` 可整体省略，**空文件合法**——否则「只想改一个字段」就得抄全份。

**字段名与 `RuntimeConfig` 一一对应**，不引入配置 DSL。转换层做形状映射而非语义翻译：`endpoint` 含 `/` 即 socket 路径这条规则，在文件与 flag 两条路径上**共用** `runtime::parse_controller`。因该规则会误判相对路径（`./mihomo.sock` 不含 `/`，会被当成 host:port），**文件中的 socket 路径必须绝对**，加载时校验并拒绝相对路径。

### D8：`mihomo_secret` 可以写在文件里，但语义分三种

文件中的 `mihomo_secret` 有三种状态，行为必须分开：

| 文件中 | 含义 | 行为 |
|---|---|---|
| 字段省略 | 未指定 | 从 SQLite 生成并持久化（`Bootstrap::resolve_secret` 的现有行为） |
| 存在但空/空白 | 用户写了空值 | **拒绝启动**。空串不是「未设置」，是**关闭内核认证** |
| 存在且非空 | 人工指定 | 直接使用，不生成、不覆盖 store |

第三种是易错点：文件值与 store 值会冲突。**文件赢**（它是更显式的意图），但必须在启动日志中**警告**「文件指定了 secret，store 中的已有值被忽略」——否则用户改了 store 却不生效且无从得知。

**权限因此升级为硬约束**：`config.toml` 有任何 group/other 位（`mode & 0o077 != 0`）即**拒绝启动**。这是本条决策的直接后果——在 D8 允许 secret 进文件之前，方案只打算对写位报错、对读位警告；那个判断的前提（文件里没有凭据）现在不成立了。

`RuntimeConfig` 中已有的 `mihomo_secret` 字段保持不动，只增加文件这一入口；`RealFactory::new` 对「loopback 且 secret 空」的硬拒保留，作为第二道防线。

### D9：TCP 监听与 token（2026-09-12）

**默认不监听。** `[api] bind` 省略即只有 unix socket。绑定端口是一个显式动作。

**硬校验只有一条，无例外**：监听 TCP ⇒ 必须已存在至少一个 token，否则**拒绝启动**（退出码 1）。

**loopback 不豁免**——这是最容易被质疑的一条，故写明理由：

- socket 上调用者身份由**内核**给出（`SO_PEERCRED` + 文件权限）；TCP 上没有任何东西可问，
  **token 就是身份本身**。没有 token 时，每个请求要么被拒（agent 无法被控制），
  要么被接受（agent 谁都能控制），两者只差一个漏掉的检查。
- **loopback 不是信任边界**：任何本地进程（包括共享网络命名空间的容器内进程）都能访问 `127.0.0.1`。
  说「只是本机」等于引入一个比 socket 弱得多的假设。
- **一条规则只有一个验证点。** 带条件的规则有第二条路径，而错误就住在那条路径上。

**token 存哈希，不存明文。** 迁移前实测确认 `api_principals` 在所有数据库中都是 0 行，
即该功能从未真正可用（没有 listener，`Role` 无来源），故直接改 schema，无需就地哈希化。

用 SHA-256 + 每行独立 salt，而**不是** Argon2/bcrypt：token 是 `generate_secret` 生成的
高熵随机值，不是人类密码——没有字典可攻，而慢哈希会拖慢每个请求。salt 逐行独立，
使两份数据库无法互相比对。schema 版本 2 → 3。

**`Role` 至此第一次有真实来源。** 此前所有连接都是 `Admin`（socket 天然可信），
所以 connections 的隐私过滤与 events 的日志过滤**写了但从未真正生效**；现在它们可达了。

**token 三条命令不走 socket**（`proxyctl token issue|list|revoke`）：token 是监听器**被允许存在**的前提，
所以签发必须能在 agent 起来之前工作，包括全新安装。这是唯一直接打开元数据数据库的命令。
数据库访问放在 `proxy-bootstrap`（组合根），因为 CLI 客户端那半边**不允许**依赖 storage 适配器——
架构测试强制这一点。

### D10：客户端支持 socket 与 TCP（2026-09-12）

**一个 `--socket`，按值判别。** 含 `://` 即视为 URL，否则视为 socket 路径：

```text
proxyctl --socket /run/proxy-agent/agent.sock status          # 本地，默认
proxyctl --socket http://host:8765 --token <T> status         # 远程
```

复用同一个 flag 而非新增 `--server`：用户只记一个「连到哪里」，
且脚本改一个值就能在本地/远程之间切换。缺点是 `--socket http://...` 读起来别扭，
但两个 flag 的代价是每个操作员都得知道该用哪个。

**`Endpoint` 是两个变体，不是两个可选字段。** `Socket` 一定没有 token（文件权限已是边界），
`Remote` 一定需要。两个 `Option` 会允许「有 URL 也有 socket」「有 URL 没 token」这类组合，
每一种都要在运行时检查；类型让它们无法构造。

**缺 token 在解析时拒绝（退出码 2），不发出等 401。** 401 的意思是「凭据不对」，
而这里是「没有提供凭据」，两者的修复动作不同，混在一起会让操作员跑错方向。

**token 来源**：`--token` > `PROXYCTL_TOKEN`。**不做客户端配置文件**——
token 落盘就会被备份、被贴进 issue，而客户端放 token 到磁盘只是图省事，
没有 agent 侧那种必要性（agent 需要 loopback controller 的 secret）。

**明文 HTTP 警告而非硬拒**，与 agent 侧对 off-host bind 的处理对称。
loopback 静默（流量不经网络），`https://` 静默。硬拒会挡住 VPN 用户，
而工具无法判断他们的网络路径是否已受保护。
`https://` **技术上现在就能用**（`reqwest` 已带 `rustls-tls`），配反代即可。

**401/403 给出可操作的提示**：401 提示 token 未被接受（未签发/已撤销/未带）。
403 原本提示「角色不足」，现已改为提示「方法不被该端点接受」——见 D12。
远程场景最常见的失败就是这两种，只回状态码等于让操作员自己猜。

**`doctor` 报告连接**：endpoint、传输类型、可达性、平台。这是远程部署的第一个问题，
且**agent 自己回答不了**（不可达时它什么也不回），所以由客户端先行打印。

**一处易漏点已覆盖**：`stream()` 自己构建 client（需要不同的超时行为），
token 在那里也必须带上——否则远程 agent 上「除流式命令外全部可用」，
而那是只有试过那条命令才会发现的裂缝。

### D11：TUI 是 `proxyctl tui`（2026-09-12）

**同一个二进制，子命令。** 与 ADR-010 D1 一致；TUI 不引入第二条「怎么连」的实现——
它用与 CLI 相同的 `Client`，所以 `--socket https://...` 远程场景**免费获得**。

**实测体积：+524 KB（+5.1%）**。Linux aarch64 release stripped：10,179,488 → 10,703,856。
远低于需要 feature gate 的阈值，故**不做**「重新编译才能用 TUI」的构建配置——
那不是免费的，它把复杂度转嫁给每个打包者和用户。这个数字是实测而非估计，
与上一轮纠正 2.4 MB 那次同样的理由。

**分层，因为 AGENTS.md 禁止在 widget/渲染/按键处理里写业务逻辑**：

```text
keys.rs   KeyEvent -> Action      纯函数，单元测试
app.rs    状态 + 事件循环          不渲染、不碰网络
ui/       &App -> Frame          纯函数，TestBackend 测试
fetch.rs  客户端调用              与 CLI 共用 client 层
```

实际收益是可测性：`keys.rs` 与 `app.rs` 用普通单元测试覆盖，只有渲染需要 backend——
而 `TestBackend` 不需要终端。这是选 `ratatui` 而非裸用 `crossterm` 的一个理由。

**轮询 + 事件流，不是二选一。** `EventPublisher` 的契约写明事件只通知、不是 ledger，
错过就要重读状态（通道有界，错过是常态）。所以**轮询是事实来源，事件是「提前重读」的提示**。
只轮询反应慢；只靠事件则在漏事件后显示陈旧数据。

**日志面板读 `/api/v1/logs`，不读事件流的 `mihomo.log`。** 后者依赖
`publish_mihomo_logs = true`（默认关闭），前者与该开关无关——所以「TUI 看得到日志」
不依赖操作员可能没设的配置。两个面板各司其职，也解释了开关关闭时 events 面板为何没有日志行。

**「未取到」与「取到但是空」是两种状态**（`Option<T>` 而非空集合），
失败**不清空已有数据**——agent 重启不该让操作员正在看的屏幕变空。

**写操作需确认**（`s`/`S` → 确认），与 `connections close --all --yes` 同一理由：
它会中断服务，误触有真实代价。**只读 token 在本地就拒绝写**并说明原因，
而不是发出去等 403。

### 真机跑出来的两个缺陷

1. **TUI 完全不绘制。** `app::run` 的注释写着「循环顶部会重绘」，而**那段代码不存在**。
   更糟的是 `Terminal::new` 从零尺寸缓冲区开始，`ratatui` 只画变化过的单元格——
   没有尺寸就永远不画任何东西。真终端会在首次 resize 时补上尺寸，所以症状是**空白屏而非报错**，
   而这正是它自己的测试抓不到的原因：`TestBackend` 用显式尺寸构造。
   只有把界面放到 pty 里跑才暴露。
2. **终端过小时 panic。** 1 行高的窗口下，三行布局（tab + body + status）索引到缓冲区外。
   终端 resize 可以随时发生（拖拽窗口时最常见），已加最小尺寸守卫。

### D12：移除角色模型（2026-09-20）

**决策。** 整个应用不再区分「管理员」与「只读」：唯一的授权维度是「是否已通过认证」，
认证成功的调用方可以做这个接口能做的任何事。

删掉的东西：

| 曾经 | 现在 |
|---|---|
| `Role::{Admin, ReadOnly}`（application 端口） | 类型已删；`Principal` 只有 `id` |
| `api_principals.role`、`sessions.role` 两列 | schema v5 显式 `DROP COLUMN` |
| `proxyctl token issue --role` | 参数删除；签发的 token 权力相同 |
| `require_write(&caller)`（14 处） | 全部删除 |
| `ConnectionView::redact_for(role)` | 删除；连接列表**总是**带 uid/进程/进程路径 |
| `Event::is_visible_to(role)` | 删除；事件流不再按调用方过滤 |
| `SessionDto.role` | 响应不再返回该字段 |
| 前端 `useIsAdmin()`、角色徽章、`roles.*` 翻译、logs/connections 两页的门控 | 全部删除 |
| 403 的「角色不足」提示 | 改为「方法不被该端点接受」 |

**为什么删，而不是留一个永远为真的默认值。** 本地 socket 现在任何人可达（ADR-005 D3b），
在那里「角色」无法被强制；只有 TCP 上能强制，而 TCP 的 token 本来就是操作员自己签发的。
也就是说这个模型在其中一条传输上不可执行，而它较窄的那一级没有任何已发布的客户端用过。
留着它会让读者以为存在一道并不存在的边界——那比没有边界更糟。

**关于 `redact_for` 与 `is_visible_to`。** 它们保护的数据是真实敏感的：连接列表能回答
「本机哪个程序访问了什么」，内核日志含有网络拓扑。但它们保护的方式是「对某类调用方隐藏」，
而角色消失后不存在「那类调用方」。准入控制因此上移到唯一还在的位置：**能否连上 agent**。
能连上就能重启内核，这严格大于读它的日志或看它连了什么，所以再过滤只是看起来像限制。

**`/clash-api` 的方法门保留。** 它检查 **HTTP 方法**（`GET`/`HEAD`/`OPTIONS` 放行，其余 403），
不检查调用方身份。删掉它等于让任何能打开 `/clash-api` 的页面直接 `PUT /configs`
替换内核运行配置——一次与角色无关的 CSRF/误操作风险。升级路径上的同一检查同理保留。

**schema 升到 v5，这是首个「删除」型迁移。** `CREATE TABLE IF NOT EXISTS` 无法表达删列：
在 v4 库上该语句是空操作，旧列会留下。因此新增一段显式迁移，用 `PRAGMA table_info`
探测列存在后再 `ALTER TABLE ... DROP COLUMN`，且只在 `user_version < 5` 时执行——
全新库的表本来就没有该列，不受影响。

**已实测**：把一个真实 v4 库（两表各有 `role` 且有数据行）交给新二进制后，
两个 `role` 列均被删除、行数据与身份完整保留、`user_version` 变为 5，
再次打开幂等；全新库直接建于 v5 且不含该列。

## Alternatives

### A1：两个二进制（`proxyctl` + `proxy-agent`）

采纳 docker/systemctl 的形态。**否决理由**：在 D3 的约束下，两个二进制省下的是体积，付出的是版本可能偏斜——客户端与服务端对某条路径或字段的理解不一致，正是「生产环境 404」的来源。单二进制让这种不一致在物理上不可能，代价是体积，见 Consequences。

### A2：`--direct` 直连（单二进制，绕过 socket）

**否决理由**：见 D2 的三条性质。尤其是每实例锁是进程内的——直连不只是「绕过权限」，它会让串行化假设失效。用户已明确否决此方案。

### A3：podman 式无守护进程

`proxyctl` 自己管进程、自己读写状态，不需要常驻 Agent。**否决理由**：内核需要**活着的宿主**（ADR-003 D4b：内核会 reparent，不写 pid 文件，因此只能 `discover()`）。没有常驻进程就没有可以重新发现内核的地方，调度、健康检查、事件流也都没有落脚点。

## Consequences

### 体积

aarch64 Linux release 构建，`lto = "thin"` + `strip = true`：**8.7 MB**（`.text` 6.7 MB + `.rodata` 2 MB）。这是完整的守护端：axum、hyper、rusqlite（含 bundled SQLite）、reqwest（含 TLS）。

只编译客户端会小得多，所以 D1 确实付出了体积代价。这个代价被记为已知事实，而不是被说成「没有代价」：如果将来体积成为真实约束（例如镜像层预算），可重新评估 A1，届时应同时加上「客户端与服务端版本必须一致」的校验。

### 部署

一个 unit 两个进程：`proxy-agent` 常驻，`proxyctl` 短命。`proxyctl restart` 只是向 Agent 发一次请求，不会重启 Agent 自身。

### 测试

- `crates/cli/tests/architecture.rs`（8 个）强制执行 D3 与 D5 的路径一致性。
- `crates/cli/src/dispatch.rs` 的测试覆盖 D4 的每个码，以及 D6。
- 真实机器验证：Debian aarch64 + OrbStack LXC，9 条命令、5 个退出码、`--json` 字节一致性、SIGTERM 关闭后 socket 被移除。

### 真实机器发现的两个缺陷

均只在 Linux 上暴露，macOS 通过：

1. **peer 校验拒绝连接时被 RST 丢弃响应。** `reject()` 写完 401 就关闭，而客户端已经把请求写进缓冲区；Linux 对「带未读数据的 close」回 RST，RST 会丢弃刚写的响应，客户端看到 `ECONNRESET` 而不是 401。已改为拒绝后先排空请求再关闭（有界、带超时）。macOS 容忍这个差异，所以只有 Linux 跑才看得到。
2. **`config validate` 文件不存在时退出码错误。** `build()` 返回 `None` 被当成「未实现」，于是打印日志流相关的消息并以 7 退出。已改为：`logs` 在构造 client 之前单独判定，其余 `None` 归为用法错误（2）。
