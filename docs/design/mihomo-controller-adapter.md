# MihomoController 适配器设计与实现

> 状态：已实现并通过真实内核验证
> 日期：2026-09-12
> 依据：`docs/adr/ADR-003`（D2/D3）、`docs/adr/ADR-005`（D2/D7b）、`docs/research/01-mihomo.md`、`docs/research/09-linux-runtime.md`
> 产物：`crates/infrastructure/src/mihomo/`

---

## 1. 结构：一个适配器，两种传输

```text
crates/infrastructure/src/mihomo/
├── transport.rs   Request/Response + Transport trait（内部抽象）
├── unix.rs        UnixSocketTransport（含 socket 权限收紧）
├── http.rs        LoopbackTransport（强制 loopback + 非空 secret）
├── wire.rs        线格式 DTO（serde）
├── socket.rs      socket 权限模型与收紧
└── adapter.rs     HttpMihomoController 实现 MihomoController
```

**为什么把 `Transport` 抽成内部 trait**：两种传输共享**同一套业务语义**——不 force、
必带 body、L4 端口检查、错误映射。若各写一个 `MihomoController` 实现，这些规则会有两份，
必然分叉。这与 ADR-002 拒绝"两套激活路径"是同一理由。

`Transport` 是适配器内部细节，不是 application 定义的 Port，因此不违反"Port 由 Application 定义"。

---

## 2. 关键实现决策

| # | 决策 | 依据 |
|---|---|---|
| 1 | **`force` 在类型上不可表达** | 真机实测：上游**接受** `?force=true`（204）→ "禁止 force" 是**我们自己的责任**，不能指望上游拒绝。`ReloadBody` 结构体没有 force 字段 |
| 2 | **PUT 必须带 body** | 实测空 body → 400。`Request::put_json` 强制要求 body |
| 3 | **L4 用真实 TCP 连接** | 实测：listener bind 失败时 `/version` 仍 200。只查 API 会把僵尸态判为健康 |
| 4 | **端口 `0` 判为"未配置"** | 实测：未配置端口返回 `0` 而非 `null`，因此必须判 `!= 0` |
| 5 | **解析失败与字段缺失分开** | 字段改名会导致静默解析成 `None`，进而误判健康并**错误回滚一个正常的配置** |
| 6 | **socket 权限收紧由适配器负责** | 内核硬编码 `chmod 0666` 且不校验 secret，权限是唯一访问边界 |
| 7 | **预建 `0750` 目录** | 实测：内核对**不存在**的目录 `MkdirAll(0755)`，但**不覆盖**已存在目录的权限 |
| 8 | **TCP 传输强制 loopback + 非空 secret** | 可达的 controller 等于进程重启权限；实测无 secret 时全部 route 401 |
| 9 | **`shutdown()` 是 no-op** | 内核没有关闭端点。发信号属 `ProcessManager` 职责，放在这里会让进程控制落在错误的适配器 |
| 10 | **禁止跟随重定向** | controller 是本地 API，跟随重定向可能把 secret 泄漏到其他 origin |

---

## 3. 线格式细节（真机实测，手写解析必须知道）

```text
GET /version  →  {"meta":true,"version":"v1.19.30"}
GET /configs  →  {"mixed-port":17892,"port":0,...,"tun":{"enable":false,...}}
GET /proxies  →  {"proxies": { "<name>": {...} }}      ← 以名字为键的对象，非裸数组
GET /rules    →  {"rules": [ {"type":"Match",...} ]}   ← 数组包在 rules 键下
```

**按裸数组解析 `/proxies` 会静默得到空结果** —— 这是最危险的一类错误，因此有针对性测试
（`an_array_shaped_proxies_body_fails_loudly`）。

`/configs` 的端口在未配置时为 **`0` 而非 `null`**。

---

## 4. 真机验证（Debian，mihomo v1.19.30 linux arm64，Rust 1.95）

**10 个集成测试全部对真实内核通过**：

| 测试 | 验证内容 |
|---|---|
| `version_reports_the_meta_kernel` | `/version` 解析与 `meta:true` 识别 |
| `runtime_config_exposes_only_configured_ports` | `0` → `None` 的转换 |
| `health_check_detects_a_listening_proxy_port` | 正常态四层全通 |
| `health_check_reports_an_unlistening_proxy_port_as_degraded` | **僵尸态判为 degraded** |
| `proxies_parse_into_groups_and_nodes` | 对象形态解析 |
| `rules_parse_from_the_wrapped_response` | 包装数组解析 |
| `an_invalid_payload_is_rejected_without_killing_the_kernel` | 拒绝后内核存活 |
| `delay_of_a_timeout_is_a_result_not_an_error` | 504 → 业务结果 |
| `socket_transport_reaches_the_kernel` | unix socket 传输可用 |
| `socket_permissions_are_detected` | 检测 0666 并收紧到 0660 |

### 4.1 ⭐ 僵尸态的端到端验证（本设计存在的理由）

构造方法：先用 Python 占住 `mixed-port`，再启动内核。实测内核日志：

```text
level=info  msg="RESTful API listening at: 127.0.0.1:19099"
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:17893: bind: address already in use"
```

此时 `/version` **仍返回 200**。适配器的健康检查报告：

```text
HealthReport { process_alive: true, controller_reachable: true,
               config_loaded: true, proxy_port_listening: false }
healthy   = false
degraded  = true      ← 正确分类
unhealthy = false
```

**只检查 API 的实现会把这里判为健康**，然后 `ActivateConfig` 会认为激活成功。
L4 检查是唯一能发现它的手段。

### 4.2 socket 权限收紧验证

```text
内核创建的 socket        →  srw-rw-rw-  (0666)
适配器 tighten() 之后    →  srw-rw----  (0660)
收紧后 API 仍可访问      →  version() 成功
```

### 4.3 平台验证

crate 在 **Debian（Rust 1.95）** 上编译通过并运行测试。开发机是 macOS，
但 `socket.rs` 的 unix 分支与 TCP 连接都只在 Linux 上被真实验证。

---

## 5. 已知局限与未验证项

| 项 | 说明 |
|---|---|
| **socket 权限收紧失败时不阻断**（按设计确认的 A 方案） | `enforce()` 返回观测状态；调用方（doctor）负责暴露，而不是让一次 `chmod` 失败导致内核无法启停 |
| `shutdown()` 为 no-op | 内核无关闭端点；终止由 `ProcessManager` 负责 |
| WebSocket 观测流未实现 | NDJSON 足够；WS 留到 TUI 需要时 |
| `/connections` 只提供原始 JSON | 完整映射属 `MihomoConnectionOps` 适配器 |
| **未在真实 PVE LXC 上验证** | 仅在 OrbStack 的 Debian machine 上验证 |
| `patch_runtime` 不在 Port 上 | 只暴露 `mode`/`log-level` 白名单，危险字段（`tun`/端口）刻意不可达 |

---

## 6. 测试策略

| 类型 | 数量 | 说明 |
|---|---|---|
| 单元测试 | 42 | 线格式解析、请求渲染、HTTP 响应解析、权限检测、URL 编码 —— **不需要内核** |
| 集成测试 | 10 | 对真实内核，默认 `#[ignore]`，通过环境变量启用 |

集成测试用环境变量启用而非硬编码端点：

```bash
PROXYCTL_TEST_CONTROLLER=127.0.0.1:19099 \
PROXYCTL_TEST_SECRET=testsecret \
PROXYCTL_TEST_SOCKET=/run/proxy-agent/mihomo.sock \
cargo test -p proxy-infrastructure --test live_kernel -- --ignored
```

未设置时测试**提前返回而非失败**，这样在没有内核的 CI 上也是绿的。
