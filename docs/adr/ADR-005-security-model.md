# ADR-005 — 安全模型（信任边界、权限模型、认证）

| 字段 | 值 |
|---|---|
| Status | Accepted（Phase 0 定稿） |
| Date | 2026-09-12 |
| Related | ADR-001、ADR-003、ADR-004、ADR-006、`docs/research/12-security.md`、`09-linux-runtime.md`、`10-pve-lxc.md`、`13-licenses.md` |

---

## 1. Context

### 1.1 不可协商的安全规则（`AGENTS.md`）

- Web/API **绝不**暴露任意命令执行（禁止 `POST /api/run-command`）。
- Mihomo controller 默认 `127.0.0.1` 或 unix socket；禁止默认绑 `0.0.0.0`。
- Unix socket 文件权限是安全模型的一部分。
- 远程可达的 Web 访问必须认证。
- 日志禁止输出订阅 URL 凭据、Mihomo secret、token、代理凭据。

### 1.2 调研得出的关键风险（均为实测/源码证据）

| # | 风险 | 证据 |
|---|---|---|
| R1 | **`secret` 为空 = 完全无鉴权**（`if secret != "" { r.Use(authentication(secret)) }`） | R12 §1、R01 §1 |
| R2 | **Unix socket 完全不校验 secret**（`router(cfg.IsDebug, "", ...)`） | R01 §1、R12 §1、R09 C8（三方独立确认） |
| R3 | **Unix socket 被硬编码 `chmod 0666`** → 同机任意用户可获完整内核控制权（含 `/upgrade`、`/restart`） | R01 §1、R12 §1、R09 C8 |
| R4 | **CORS 默认 `allow-origins: ["*"]` + `allow-private-network: true`**；即使显式写 `allow-origins: []` 仍返回 `*` | R12 §1、本 ADR 独立复核 `config/config.go:595-598` |
| R5 | **`external-controller: ":9090"` 会绑 `[::]`（全网卡）** | R12 §1（实测） |
| R6 | 已知漏洞 **CVE-2025-56499**（mihomo ≤1.19.11 任意文件读取）；v1.19.30 已修绝对路径穿越，但**工作目录/`SAFE_PATHS` 内文件仍可被读取并可能进日志** | R12 §1（实测） |
| R7 | **Sub-Store 默认无认证 + 源码默认绑 `::`**；其 `/api/preview/sub` Script Operator 历史上有未授权 RCE（issue #634，v2.38.2 修复） | R12 §1、R05 §1、R04 §8 |
| R8 | **sub-store-convert 不得使用**（许可 + 失败面） | R06、R13 |
| R9 | ShellCrash 的"服务用户"实为 **uid 0 + 独立 gid**（假隔离） | R08 §1 |
| R10 | **`/dev/net/tun` 存在且 `open()` 成功，仍可能 `TUNSETIFF` EPERM** → 安全/能力判定不能用 bool | R10 §1 C1–C2 |
| R11 | `PrivateDevices=yes` 会让 `/dev/net/tun` 消失；`ProtectKernelTunables=yes` 破坏 nftables/sysctl 写入 | R09 C6 |
| R12 | Agent 若需 systemd 操作，polkit 默认 `manage-units` = `auth_admin` | R09 C7 |
| R13 | `age` 加密配置受支持（`age.DecryptBytes`）；`/configs` 注入 `rule-providers.type=file` 是文件读取向量 | R12 §1、R01 |

---

## 2. Decision

### D1. 信任边界（四层）

```text
①  Internet / 局域网
        │  ← 默认不暴露任何端口到 0.0.0.0
        ▼
②  反向代理（可选）+ 认证          ← 远程访问唯一入口（Bearer token）
        ▼
③  proxy-agent（控制平面，非 root 优先）
        │   Unix socket: /run/proxy-agent/agent.sock  (0660 root:proxyctl)
        ├──────────────┬──────────────┬───────────────┐
        ▼              ▼              ▼               ▼
④  Mihomo 子进程   systemd/D-Bus   内核网络       Sub-Store（本机，不可信）
   （数据面，同用户）                 (nftables/TUN)  （可选外部组件）
```

**信任级**：Agent > Mihomo（本地 socket）> Sub-Store（**不可信**，即使在本机）。

### D2. Mihomo Controller 加固（强制三重）

Agent 生成的每一份配置**必须**满足：

```yaml
# 1) 只绑 loopback（或使用 unix socket）
external-controller: 127.0.0.1:9090      # 绝不允许 ":9090" / 0.0.0.0 / 空 host
# 2) secret 必须非空（TCP 模式下）
secret: "<随机生成，>=32 字节>"
# 3) CORS 必须显式收窄（默认值是危险的）
external-controller-cors:
  allow-origins: []            # 由 Agent 反代，不需要浏览器直连 controller
  allow-private-network: false
```

**生成期硬校验**（启动/校验阶段即失败，而不是部署约定）：

- `external-controller` 的 host 必须非空且为 loopback；否则**拒绝生成/激活**。
- TCP 模式下 `secret` 为空 → 拒绝。
- 使用 `external-controller-unix` 时：Agent **必须在 socket 创建后 `chmod 0660` + 设置 group**，并确保 `/run/proxy-agent` 为 `0750`（R2/R3 的补偿控制）。

> **为何必须要补偿控制**：上游在 unix 模式下既不校验 secret、又把 socket 设为 0666（R2/R3）。这是**上游行为**，我们无法通过配置关闭，只能在 Agent 侧收紧。

### D3. Agent 自身接口

| 通道 | MVP 方案 | 依据 |
|---|---|---|
| 本地 CLI/TUI | Unix socket `/run/proxy-agent/agent.sock`，`0660 root:proxyctl`，**并用 `SO_PEERCRED` 校验 uid/gid** | R12 §1（推荐）、R09 C8 |
| Web（远程） | **Bearer token**（存储为哈希，可轮换）+ 严格 CORS 白名单；`ADMIN`/`READ_ONLY` 角色 | R12 §1 |
| 会话 cookie + CSRF、mTLS | 列入 Phase 2（不是 MVP） | R12 §1 |
| 启动期硬校验 | **非 loopback 监听 ⇒ 必须已配置 token，否则拒绝绑定/拒绝启动** | R12 §1、REQ-SEC-004 |

### D4. 权限模型（最小权限表）

| 操作 | 本地 root | `proxy-agent` 用户 | `proxyctl` 组成员 | 远程 Web(ADMIN) | 远程 Web(READ_ONLY) |
|---|---|---|---|---|---|
| 查看状态/日志/Doctor | ✓ | ✓ | ✓ | ✓ | ✓ |
| 启停/重启/reload Mihomo | ✓ | ✓（自身子进程） | ✓（经 Agent） | ✓ | — |
| 写/激活/回滚配置 | ✓ | ✓ | ✓ | ✓ | — |
| 更新内核二进制 | ✓ | **需要特权路径**（见 D5） | ✓（经 Agent） | ✓ | — |
| 应用防火墙规则 | ✓ | 需 `CAP_NET_ADMIN`（MVP 不做 apply） | — | — | — |
| 修改用户/组/unit | ✓ | — | — | — | — |

- **不需要 root 的能力**：配置版本化、订阅管理、状态查询、Doctor（只读探测）。
- **需要 `CAP_NET_ADMIN`**：TUN 创建、nftables/策略路由写入、`ip` 操作（R10 §1）。
- **不引入 `sudo`**（R09 C7）：需要 systemd 操作时走 D-Bus + 窄化 polkit rule，或 Agent 以 root 运行并用 `SystemCallFilter=` 兜底。

### D5. 内核二进制更新路径（提权隔离）

```text
下载（无特权，可降级为普通文件写） → 校验 checksum → 原子替换到 /opt/proxy-agent/bin/ → 重启
```

- 替换目标目录由 Agent 拥有，**不需要 root 写 `/usr/local/bin`**。
- 若必须写 root 所属目录 → 用**独立 oneshot systemd 单元**（最小、可审计），**不要**给 Agent 常驻进程 `NOPASSWD`。
- **禁止依赖 Mihomo 的 `/upgrade`**（ADR-003 D2：无签名校验、半截二进制风险）。
- 校验：至少 SHA-256；若上游提供签名则优先（R13 提醒下载渠道可信性问题）。

### D6. SSRF 与出站目标控制

订阅 URL 由用户提供，抓取可能由 Agent 或（更常见）Sub-Store 发起：

- Agent 侧（NativeConverter 直连）：必须拒绝环回、链路本地、元数据地址（`169.254.169.254`、`::1`）、RFC1918（除非显式白名单）。
- Sub-Store 侧：Agent **无法控制其出站** → 必须文档化该风险，并要求 Sub-Store 只绑 loopback + 依赖主机防火墙；在 MVP 中把"订阅 URL 指向内网"作为**用户自担风险**并给出告警。
- 关联 open-questions Q012（是否要求所有抓取都经 Agent）。

### D7. 日志与审计脱敏（强制）

**必须 redact**：

```text
订阅 URL 的 query/path 中疑似凭据（token/password/key/secret/subscribe 等参数与 path 段）
Mihomo secret、Agent token、代理节点密码/UUID
age 私钥、Sub-Store 的 SUB_STORE_* 环境变量整体（/api/utils/env 会回显全部）
```

**审计记录**（append-only）：

```text
who（本地 uid / 远程 principal）
when
action（mihomo.start|stop|restart、config.activate|rollback、subscription.update、kernel.update、system.*）
target（实例 ID / config version / subscription id）
result（success|failure + 原因码）
```

**不得写入审计**：完整配置内容、订阅原文、secret。

### D7b. ⚠️ hardening 指令在 LXC 中可能静默失效（真机实测修正）

在 Debian（systemd 261，`/run/systemd/container` = `lxc`）上的实测结论：

| 指令 | 实测结果 |
|---|---|
| `AmbientCapabilities=CAP_NET_ADMIN` | ✅ 生效（子进程 `CapAmb=0x1000`） |
| `NoNewPrivileges=yes` | ❌ **静默失效**（子进程 `NoNewPrivs=0`，`systemctl show` 报 `no`，无警告） |
| `PrivateDevices=yes` | ❌ **静默失效**（`systemctl show` 报 `no`，`/dev/net/tun` 仍可见） |

**含义（必须遵守）**：

1. **不得把 `NoNewPrivileges=yes` 或 `PrivateDevices=yes` 当作已生效的安全保证**。在本项目的
   主要目标环境 PVE LXC（同为 LXC 容器）中它们可能被静默忽略。
2. **doctor 必须检测指令是否真正生效**，而不是读 unit 文件里写了什么。可实现的探测：
   读 `/proc/self/status` 的 `NoNewPrivs`（期望 1）、检查 `/dev` 是否被隔离，
   与 unit 声明的期望值比对，不符则判 `Misconfigured`。这是"检测而非假设"原则的应用。
3. **安全边界不能只靠 unit hardening**：认证、socket 权限、secret 校验必须独立成立，
   以便在 hardening 全部失效时仍然有效。这也是 D2/D3 不依赖 hardening 的原因。
4. `AmbientCapabilities=` 是可靠的，因此"靠 ambient cap 传递 `CAP_NET_ADMIN`"的方案成立。

### D8. 容器/PVE 特有风险的处理

- **能力判定不得用 bool**（R10 C1/C2）：`/dev/net/tun` 存在但 `TUNSETIFF` EPERM 必须表达为 `Misconfigured` 并给出修复建议，而不是 `Supported`。
- doctor **必须检测**：是否监听非 loopback、socket 权限是否被收紧、CORS 是否被收窄、代理端口是否真的在监听。
- **探测必须无副作用**（REQ-LXC-005）：写类探测默认关闭（`ProbeOptions{allow_write_probes:false}`）。

---

## 3. Alternatives

| 方案 | 拒绝理由 |
|---|---|
| 依赖 Mihomo `secret` 做唯一防线 | secret 为空时完全无鉴权（R1）；unix 模式下 secret 被忽略（R2） |
| 直接用 unix socket 而不收紧权限 | 上游 chmod 0666（R3）→ 同机任意用户控制内核 |
| 保留默认 CORS | 默认 `*` + private-network（R4）→ 浏览器侧任意站点可控制本机内核 |
| 绑 `0.0.0.0:9090` 便于远程访问 | 违反 `AGENTS.md`；应通过反代 + 认证 |
| 用 `sudo NOPASSWD: ALL` | 等于把 root 交给 Web 层；R09 C7 明确反对 |
| 会话 cookie 作为 MVP 唯一认证 | 需 CSRF 防护与更多状态；Bearer token 更适合 API/CLI 同源场景 |
| 给容器加 `SYS_ADMIN` "以防万一" | R10 §1 C4 实测：TProxy/TUN 均不需要；扩大攻击面 |

---

## 4. Consequences

### 4.1 正面

- 三个最危险的上游默认行为（无鉴权、0666 socket、宽 CORS）都有明确的补偿控制。
- 权限模型可映射成测试（REQ-SEC-001~010）。
- 与 ADR-003/004 的生命周期设计一致（健康检查可校验 socket 权限与监听地址）。

### 4.2 负面 / 成本

- socket 权限收紧存在短暂窗口（Mihomo 创建 → Agent chmod）；需在 health check 中复检，并在文档中说明该窗口。
- 非 loopback + 无 token 时**拒绝启动**会让某些"先跑起来再配"的用户困惑 → 需要清晰的错误信息与文档。
- `SO_PEERCRED` 的实现需要非平凡代码（tokio 生态需 `SO_PEERCRED` 或 `UnixStream::peer_cred`）。

### 4.3 必须遵守

```text
1. 生成的配置必须：controller 绑 loopback、secret 非空、CORS 收窄（REQ-SEC-001/003）
2. unix socket 必须收紧为 0660 且目录 0750，并纳入 health check（REQ-SEC-002）
3. 非 loopback 监听 + 无 token ⇒ 拒绝启动（REQ-SEC-004）
4. 接口层不得持有 root shell；特权操作映射到 Use Case + Port（REQ-ARCH-004/006）
5. 日志/审计脱敏为强制项，需正则矩阵单测（REQ-SEC-006）
6. 能力判定用五值枚举，禁止 bool（REQ-LXC-002）
```

---

## 5. Evidence

- `docs/research/12-security.md` §1/§2/§3/§10（资产、威胁、CVE-2025-56499、32 条 checklist）
- `docs/research/01-mihomo.md` §1（secret 空值行为、unix socket 免鉴权、0666）
- `docs/research/09-linux-runtime.md` C3/C6/C7/C8/C10（ambient cap、hardening 与 TUN 冲突、polkit、socket 权限、conffile）
- `docs/research/10-pve-lxc.md` §1（TUN 双条件、Misconfigured 判定、容器内无 systemd）
- `docs/research/04-sub-store.md` §8、`05-sub-store-deployment.md` §1（无认证、`::` 默认绑定、`/api/utils/env` 回显）
- `docs/research/06-sub-store-convert.md`、`13-licenses.md` §1/§3（拒绝理由与许可证边界）
- `docs/research/08-shellcrash.md` §1（uid 0 假隔离的对照）
- 独立复核（本次会话直接抓源码）：`hub/route/server.go`（`startUnix` 传空 secret + `os.Chmod(addr, 0o666)`）、`config/config.go:595-598`（CORS 默认值）
