# ADR-008 — 内核二进制获取与配置字段校验

| 字段 | 值 |
|---|---|
| Status | Accepted（2026-09-12，批次 C 实施时定稿） |
| Date | 2026-09-12 |
| Related | ADR-003（Mihomo 集成）、ADR-004（配置生命周期 D2）、ADR-006（部署模型 D4）、ADR-007（元数据持久化）、Q015、Q020 |

---

## 1. Context

批次 C 实现 `ConfigValidator`、`CapabilityProbe`、`ServiceManager`、`KernelInstaller` 四个适配器时，实测推翻了两条既有假设，并暴露了一个 `AGENTS.md` 硬规则与一项必要检查的冲突。

### 1.1 `mihomo -t` 的副作用（回答 Q020）

| 假设 | 实测（Debian aarch64，v1.19.30） |
|---|---|
| 含 `GEOIP`/`GEOSITE` 会真实下载 geodata | ✅ **确认**：下载 `geoip.metadb` (8.5 MB) + `GeoSite.dat` (4.2 MB)，耗时 ~4s |
| 对不存在的文件返回 exit 0 | ✅ **确认**：exit 0，且**创建了该文件** |
| 不检测未知字段 | ✅ **确认**：`mixed-portt` → exit 0 通过 |
| 含 `tun:` 产生设备副作用 | ❌ **未发生**：探测设备未创建 |

**Q020 结论**：`-t` 有网络与磁盘副作用，必须在隔离目录中执行。

### 1.2 上游不提供 checksum 文件

| 假设 | 实测 |
|---|---|
| 存在 `.sha256` asset | ❌ **404** |
| 唯一类校验 asset | `version.txt`，内容仅版本号 |
| 可用渠道 | GitHub API 的 asset `digest` 字段（`sha256:<hex>`），**已独立复算确认一致** |
| `github.com` 直连 | ✅ 200（**macOS 阶段的超时是本地网络问题**） |

### 1.3 规则冲突：`ioctl(TUNSETIFF)` 与 `forbid(unsafe_code)`

`CapabilityProbe` 的 TUN 判定需要三级检查，其中只有 `ioctl(TUNSETIFF)` 是决定性的：设备存在 + 可打开两项在 `CapEff=0` 的容器上**都会通过**（实测 `TUNSETIFF` 才返回 `EPERM`）。但该 ioctl 在 `nix` 0.30 中生成 `unsafe fn`，而 `crates/infrastructure/src/lib.rs` 有 `#![forbid(unsafe_code)]`。

---

## 2. Decision

### D1. 内核二进制只从上游 release 直连获取，不支持镜像（收口 Q015）

**理由**：镜像**无法提供可信摘要**——镜像自己的 `sha256sums` 与它给的二进制**同等不可信**；从镜像下载却去上游 API 取 digest 则使镜像成为无意义跳转。支持镜像只剩两条路：放弃校验，或假装校验过。**不做镜像让校验保持诚实。**

### D2. 校验凭证是 GitHub API 的 asset `digest`，而非 `.sha256` 文件

```text
fetch():
  1. GET api.github.com/repos/MetaCubeX/mihomo/releases/tags/<tag>
  2. 取该 asset 的 digest 字段（无 digest → 拒绝，不安装未校验二进制）
  3. 下载 raw asset（必须跟随 302 到签名 CDN）
  4. SHA-256(压缩产物) == digest ? 否 → 丢弃
  5. gunzip → 临时文件 → 试运行 -v → 原子 rename
```

**关键细节**：digest 覆盖的是**压缩产物**，不是解压后的二进制。因此：

- `verify()` 比较**记录的 digest**（"这是发布方给出的那个产物吗？"）
- `verify_file_digest()` 重新哈希**磁盘文件**（"字节还对得上吗？"）

两者是不同的问题，必须是不同方法。把它们混为一谈会让一个正确获取的产物**永远无法通过校验**。

### D3. 隔离目录是 `mihomo -t` 的唯一安全执行方式

每次 `validate_semantic` 创建一次性目录并**无条件删除**；跑之前预置已存在的 geodata（若有）以避免重复下载。不这么做的话，每次校验泄漏 12.8 MB。

### D4. 字段白名单从上游源码生成，未知字段**报告而非拒绝**

- 白名单由 `scripts/gen-config-whitelist.py` 从上游 `config.go` 的 yaml tag 生成（v1.19.30：65 个顶层字段 + 11 个嵌套 section）。**不手写**——手写会以"这个检查正要发现的方式"随上游漂移。
- 未知字段**报告在失败原因里**，而不是让上游新增字段在旧 Agent 上硬失败。拼写错误因此可见而不静默。

### D5. 新建 `proxy-sys` crate 承载唯一的 unsafe 代码

**替代方案（已否决）**：把 infrastructure 的 `forbid(unsafe_code)` 降级为 `deny` 并局部 `#[allow]`。否决理由：`deny` 可被**任何后续模块**用一行 `#[allow]` 绕过，`forbid` 不能；把 unsafe 收进一个约 130 行、可单独评审、只包装一次 `ioctl` 的 crate，能让其余所有 crate 保持 `forbid`。

`proxy-sys` 的非 Linux 实现返回 `Unsupported` 而非 `Missing`：**"我无法检查"与"设备不存在"是不同的答案**，混淆它们会让能力模型把"平台不支持"报成"内核功能缺失"。

### D6. `ServiceManager` 调用 `systemctl` 子进程，不引入 D-Bus 客户端

`AGENTS.md` 的依赖基线无 D-Bus crate，而该 port 只问三个只读问题。代价显式承担：输出是文本，因此**优先解析 exit code**，仅在 `is-system-running`（其状态无退出码区分）时读文本。

**关键修正**：`degraded` 必须判定为 systemd。它表示"有 unit 失败"，不是"没有 init system"——真机实测本容器仅因无关 unit 而报 `degraded`，误判会**在可用的主机上关闭服务管理**。

---

## 3. Alternatives

| 方案 | 否决理由 |
|---|---|
| 支持镜像渠道 | 镜像无法提供可信摘要，等于放弃校验或假装校验（D1） |
| 依赖上游 `.sha256` 文件 | **不存在**（404） |
| `verify()` 哈希解压后的二进制 | digest 覆盖压缩产物，**正确产物也永远无法通过**（D2，实测踩中） |
| 复用同一个临时目录做校验 | 首次下载的 geodata 会掩盖后续的下载失败，且每次泄漏 12.8 MB |
| 手写字段白名单 | 随上游漂移，且漂移方式正是该检查要发现的（D4） |
| 硬拒绝未知字段 | 上游在版本间新增字段，硬拒绝会让合法新配置在旧 Agent 上失败（D4） |
| infrastructure 降级 `forbid` → `deny` | 打开可被逐个绕过的口子（D5） |
| 引入 D-Bus 客户端 | 为三个只读查询引入大型依赖栈，与基线不符（D6） |
| TUN 只做前两级检查 | 实测在 `CapEff=0` 容器上**误报为可用**——正是能力模型要防的假阳性 |

---

## 4. Consequences

**正面**

- **真机验证通过**：Debian aarch64 上 **607 passed / 0 failed**，含真实 GitHub release 的 `fetch` → digest 校验 → 安装 → 识别全链路。
- 实测 digest `58896873736d28628f66de3677c8654fa0f180662523148e136cff4f6e890069` 与独立复算一致。
- 隔离目录方案有测试守护（含"scratch 目录跑完必须为空"），防止 geodata 泄漏回归。
- `proxy-sys` 让 workspace 其余部分保持 `forbid(unsafe_code)`。

**代价与约束**

- **升级 Mihomo 时必须重新生成字段白名单**（`scripts/gen-config-whitelist.py`）。这是显式动作，比静默漂移好，但确实是维护成本。
- `ConfigValidator` 每次 `validate_semantic` 会在无缓存 geodata 时下载 12.8 MB。**设计据此降级**：`preflight` 在离线且无缓存时返回 `Skipped` 而非 `Failed`。
- 未认证 API 限流 **60 次/小时**。每次更新消耗一次；403 返回可读原因而非裸状态码。
- `systemctl` 的文本解析是 D6 的显式代价。

**新增依赖（均记录理由）**

| 依赖 | 用途 | 备注 |
|---|---|---|
| `serde_yaml` 0.9 | L1 语法 + 字段遍历 | 上游标记 deprecated，但其继任者尚未稳定；**需在未来复评** |
| `sha2` | 二进制校验 | RustCrypto，与 GitHub digest 算法一致 |
| `flate2` | 解压 `.gz` 产物 | |
| `libc`（`proxy-sys` 专用） | `ioctl` 常量与调用 | 仅此 crate |

---

## 5. Evidence

- `mihomo -t` 副作用实测（Debian aarch64 / v1.19.30）：geodata 下载 8.5 MB + 4.2 MB、4 秒；不存在文件 exit 0 且被创建；`mixed-portt` exit 0；`tun:` 未创建设备。
- GitHub API `digest` 字段与下载字节的 SHA-256 **独立复算一致**；`.gz.sha256` → 404。
- `ioctl(-1, TUNSETIFF)` 实测返回 `-1` / `EBADF`；`/dev/null` 返回 `ENOTTY`。
- TUN 三级检查真机结论：`CAP_NET_ADMIN` 单独足够；`CAP_SYS_ADMIN` 单独仍 `EPERM`。
- 测试总量：host **594 passed / 0 failed**；真机 **607 passed / 0 failed**（批次 C 前为 567）。

### 5.1 实施中发现并修复的缺陷

| 缺陷 | 后果 | 如何发现 |
|---|---|---|
| `hex_digest` 直接编码 `Bytes` 而非先哈希 | 产生长度错误但看似合理的值，**永不匹配** | 打印 `computed_len=146`（应为 64） |
| `verify()` 哈希解压后二进制 | 正确产物**永远无法通过校验** | **真机 live test** |
| `digest_matches` 只剥离一侧前缀 | 参数顺序颠倒时误报不匹配 | 新增对称性测试 |
| 测试用 HTTP server 每连接只读一次 | 客户端复用连接时**服务上一次的响应体**，harness 看似工作实则什么都没测 | 真机/本地 live test |
| `current()` 对不可执行文件返回错误 | 二进制定文件被替换时**状态查询也失败**——恰是运维最需要它的时候 | 单测 |
| `io::ErrorKind::Uncategorized` | host 工具链稳定、**Debian 的旧版 Rust 不稳定** | 真机编译 |
| `Architecture::Unknown` / `OperatingSystem` 变体不存在 | 因 cfg 分支未在开发机编译而未被发现 | 代码审查 |
