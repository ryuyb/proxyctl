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

### D6：`logs` 明确报告未实现

`logs` / `logs -f` 打印 `LOGS_NOT_IMPLEMENTED` 并以 7 退出。留空并返回 0 更糟：脚本会把沉默当成「没有日志」。它需要 observer port（ADR-003），而那个 port 还没有实现。

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
