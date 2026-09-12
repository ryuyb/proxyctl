# R15 — 竞品与同类项目 Feature Matrix

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：以上游 README / 上游源码 / GitHub API 抓取为主，未做端到端实测
> 关键结论一句话：**存在直接竞品**——Go/Rust 系 Linux 服务器端 mihomo 管理器（mihari、mihomo-tui(Go)、Proxy-RS、clashtui、flclash-tui）已经覆盖了"daemon + CLI/TUI + 订阅 + 内核版本管理 + 原子写 + 失败回滚"的大部分骨架；我们真正的空白点只剩 **配置的不可变版本历史 + 显式回滚 + 面向 PVE LXC 的能力检测/降级语义 + 统一 Doctor 与审计**，而不是"Mihomo 管理"这件事本身。

---

## 1. 结论摘要（TL;DR）

1. **"Linux 服务器端 Mihomo 管理器"不是空白市场。** 本次抓取到至少 5 个活跃的同类项目：`mihari-proxy/mihari`（Go，1.2 万行级架构文档，daemon 控制面 + CLI/TUI/Web 三端共享）、`WangZhongDian/mihomo-tui`（Go，tview，明确宣传 *targets headless Linux servers*）、`MiChongs/Proxy-RS`（Rust/Ratatui，声明"适合桌面或服务器"）、`JohanChane/clashtui`（Rust，670 star，系统服务管理）、`yqlay/flclash-tui`（Dart，122 star，明确 *Linux/SSH/headless Mihomo TUI*）。**"未发现直接竞品"这一说法不成立，不能写进结论。**
2. **"配置版本化 + 原子回滚"已经被部分实现，但语义普遍弱于本项目的目标。** 证据：`flclash-tui` 的 *"Backend validates, writes atomically, reloads Core, and rolls back on failure"* 与 `config backup/restore`；`mihari` 的 *validated atomic config generation with rollback* + *revision 预检* + *offline switching*；`metacubexd` server agent 的 *"compose → validate with `mihomo -t` → restart only after validation succeeds"*；`ShellCrash` 的 `config.yaml.bak`。**但这些都不是"不可变配置版本库 + 任意历史版本回滚 + 激活记录/审计"**：没有任何一个项目提供 `config list / show / diff / activate <id> / rollback <id>` 这套面向历史版本的接口。这是我们最硬的差异化。
3. **Doctor / 环境能力检测几乎无人做。** 本次抓取到的文档中，只有 `flclash-tui` 有 `flclash doctor [--json]` 命令名、`WangZhongDian/mihomo-tui` 有 `tun_diagnose` / `tun_debug`（TUN 路由 dry-run）、`Proxy-RS` 有"故障排查"章节。**没有任何项目做 LXC/容器/unprivileged 的能力分层检测**：ShellCrash 只有一行 `systype='container'` 的布尔判断（`init.sh:20`），而它的容器分支只是改 `CRASHDIR` 路径，不是能力降级模型。
4. **Dashboard 与订阅转换绝对不该重做。** `metacubexd`（MIT，4.3k star，官方 Dashboard，含 profile/monaco/mihomo -t 校验的 agent）与 `Sub-Store`（AGPL-3.0，10k star）+ `subconverter`（GPL-3.0，17k star）/`sub-store-convert`（MIT，2 star，可独立部署）已经把这两块做成生态标准。本项目应只做适配器。
5. **替代风险真实存在且不低。** `mihari` 距离"配置版本化 + 回滚"只差一个历史版本库；`metacubexd` 的官方 agent 距离"内核生命周期 + 校验后重启"只差一层 systemd/LXC 语义。如果它们补上这一层，我们的窗口期会显著缩短（见第 6 节）。

---

## 2. 候选项目客观事实表（抓取日期 2026-09-12）

> 数据来源标注：`[API]` = 本次 `api.github.com/repos/<repo>` 抓取（2026-09-12，未认证，额度耗尽前成功抓到前 11 个）；`[shields]` = 本次通过 shields.io badge JSON 端点抓取（同源 GitHub 数据，star/last-commit 为**聚合值**，例如 `13k` / `august`，精度低于 API）；`[README]` = 上游 README 原文。
> 抓取时间窗口：`2026-09-12 12:36–12:50 CST`。`api.github.com` 未认证额度在抓取中途耗尽（HTTP 403 `rate limit exceeded`），因此 **JohanChane/clashtui 起的项目只有 `[shields]` 聚合值**。

| # | 项目 | 一句话定位 | 语言 | Star | License | 最后提交 | 最近 Release | 数据源 |
|---|---|---|---|---|---|---|---|---|
| 1 | `juewuy/ShellCrash` | Shell 环境下一键部署管理 mihomo/sing-box 的脚本工具（路由器 + Linux 服务器 + Docker/PVE） | Shell | 13,261 | **GPL-3.0** | 2026-09-11T18:17:21Z | `[shields]` 有，tag 未取 | `[API]` |
| 2 | `MetaCubeX/metacubexd` | Mihomo 官方 Web Dashboard（纯面板 + 桌面 app + **all-in-one server agent**） | TypeScript | 4,340 | **MIT** | 2026-09-10T07:03:05Z | `[shields]` | `[API]`+`[README]` |
| 3 | `sub-store-org/Sub-Store` | 高级订阅管理器（QX/Loon/Surge/Stash/Egern/Shadowrocket），含脚本化节点处理 | JavaScript | 10,450 | **AGPL-3.0** | 2026-09-12T00:46:21Z | `[shields]` | `[API]`+`[README]` |
| 4 | `tbxark/sub-store-convert` | 把 Sub-Store 节点转换逻辑抽成独立 core/CLI/HTTP/CF Worker | JavaScript | 2 | **MIT** | 2026-09-11T06:38:41Z | `[shields]` | `[API]`+`[README]` |
| 5 | `vernesong/OpenClash` | OpenWrt 上的 Clash/Mihomo LuCI 客户端（iptables/nftables TProxy、TUN） | HTML(实际 Lua/Shell) | 27,437 | **MIT** | 2026-09-11T20:18:39Z | v0.47.156 `[shields]` | `[API]`+`[README]` |
| 6 | `Openwrt-Passwall/openwrt-passwall` | OpenWrt LuCI 代理客户端（PassWall 主仓库，GPL-3.0） | Lua | 9,909 | **GPL-3.0** | 2026-09-12T03:57:21Z | `[shields]` | `[API]` |
| 6b | `Openwrt-Passwall/openwrt-passwall2` | PassWall2：面向 sing-box 的 OpenWrt LuCI 代理插件 | Lua | 3,566 | **GPL-3.0** | 2026-09-11T16:44:49Z | `[shields]` | `[API]` |
| 7 | `clash-verge-rev/clash-verge-rev` | Tauri 桌面 GUI（Win/macOS/Linux），Rust 编写 | Rust | 143,901 | **GPL-3.0** | 2026-09-12T02:26:30Z | `[shields]` | `[API]` |
| 7b | `mihomo-party-org/clash-party` | Mihomo 桌面 GUI（原名 mihomo-party，仓库已改名 clash-party） | TypeScript/Electron | 26,378 | **GPL-3.0** | 2026-09-12T01:54:16Z | `[shields]` | `[API]` |
| 8 | `mihari-proxy/mihari` | **daemon 控制面** + CLI/TUI/Web 三端的跨平台 mihomo 管理器 | Go | 118 | **GPL-3.0** | `[shields]` yesterday | v0.9.3 `[shields]` | `[shields]`+`[README]`+`[docs]` |
| 9 | `WangZhongDian/mihomo-tui` | Go/tview TUI + IPC daemon，明确面向 **headless Linux servers** | Go | 4 | **MIT** | `[shields]` july | v0.2.1 `[shields]` | `[shields]`+`[README]` |
| 10 | `JohanChane/clashtui` | Rust TUI，Mihomo + sing-box 双内核，systemd 服务控制 | Rust | 670 | **MIT** | `[shields]` august | v0.3.1 `[shields]` | `[shields]`+`[README]` |
| 11 | `MiChongs/Proxy-RS` | Rust/Ratatui 跨平台管理器（sing-box + mihomo），service 后端 + headless CLI | Rust | 5 | **MIT** | `[shields]` july | v0.1.0 `[shields]` | `[shields]`+`[README]` |
| 12 | `yqlay/flclash-tui` | Linux/SSH/headless Mihomo TUI/CLI（Flutter/Dart），带 doctor 与 failover | Dart | 122 | **GPL-3.0** | `[shields]` last tuesday | v0.5.28 `[shields]` | `[shields]`+`[README]` |
| 13 | `potoo0/mihomo-tui` | Rust/Ratatui 纯 API 仪表盘（**明确不管配置文件**） | Rust | 129 | **MIT** | `[shields]` july | v0.4.5 `[shields]` | `[shields]`+`[README]`+`[Cargo.toml]` |
| 14 | `JimZhang168872/vpnkit` | Go 单二进制终端管理器，**完全非 root、明确 out of scope: TUN** | Go | 19 | **MIT** | `[shields]` july | v1.0.4 `[shields]` | `[shields]`+`[README]` |
| 15 | `totrytakeoff/verge-tui` | 从 clash-verge-rev 抽出的 Rust TUI（8 star，早期） | Rust | 8 | **GPL-3.0** | `[shields]` july | v0.2.0 `[shields]` | `[shields]` |
| 16 | `Morningxxx/rATC` | Rust/Ratatui TUI，**驱动 xray-core 而非 mihomo**（Clash YAML → xray 转换） | Rust | 0 | **MIT** | `[shields]` june | v0.1.0 | `[shields]`+`[README]` |
| 17 | `nkanf-dev/mihomot` | 自称 AI-native 的 Rust Mihomo 管理器（活跃度低） | Rust(推测) | 6 | **MIT** | `[shields]` may | `[未验证]` | `[shields]` |
| 18 | `tindy2013/subconverter` | 订阅格式转换器（多格式互转，HTTP API `/sub`） | C++ | 17,060 | **GPL-3.0** | 2026-07-09T15:40:27Z | `[shields]` | `[API]`+`[README]` |
| 19 | `nelvko/clash-for-linux-install` | Linux 一键部署脚本 `clashctl`（内核/面板/TUN/订阅，含 subconverter 集成） | Shell | 14,754 | **MIT** | 2026-09-07T07:21:25Z | `[shields]` | `[API]`+`[README]` |
| 20 | `MetaCubeX/mihomo` | 代理内核本体（Data Plane，非竞品，作为基线） | Go | 34k | **MIT** | `[shields]` february 2025（shields 聚合，疑似陈旧） | v1.19.30 | `[shields]` |

### 2.1 "Linux 服务器端 Rust/TUI 类管理器"搜索结果（重点）

搜索关键词（均通过 `api.github.com/search/repositories`，2026-09-12）：

```text
"mihomo tui"                 → 51 个结果，取 top 15 逐一核实
"clash manager rust"         → 5 个结果
"openwrt-passwall"           → 186 个结果（用于确认 PassWall 真实 owner）
"sub-store-convert"          → 12 个结果（用于确认真实仓库）
```

**结论：存在真正的同级竞品，不是"未发现直接竞品"。** 与"Linux 服务器 + 命令行/终端 + Mihomo 管理"最贴近的三个：

| 项目 | 为什么算直接竞品 | 为什么又不完全等同 |
|---|---|---|
| `mihari-proxy/mihari` | daemon 控制面 + CLI/TUI/Web 三端共享同一控制面；原子配置生成 + reload 回滚 + revision；systemd 服务托管；TUN/system-proxy；web panel 安装/回滚 | Go（非 Rust）；无不可变配置版本历史与显式 `rollback <version>`；无 PVE LXC 能力检测；无 nftables/TProxy 透明代理自动化 |
| `WangZhongDian/mihomo-tui` | 明确 *targets headless Linux servers*；IPC daemon；订阅池 + **保留上一份可用配置（preserves the last working version）** + 离线引导；内核多版本下载/切换；内置 systemd service 安装；TUN + `tun_diagnose`；Docker TUN 兼容 | Go/tview；仅 4 star、活跃度低（july）；无配置版本历史；无 LXC 语义；无 Doctor 全量环境检测 |
| `yqlay/flclash-tui` | 明确 *Linux/SSH/headless*；Backend 事务化原子写 + **失败回滚**；`flclash doctor`；`config validate/backup/restore`；TUN lease helper；多前端 attach 单一 Backend | Dart/Flutter（非 Rust，依赖较重）；doctor 检测项未验证；无版本历史；无 LXC；无审计 |

**未发现直接竞品的部分**：本次搜索**没有发现**任何同时满足"Rust + Linux server + Mihomo 生命周期 + 配置版本化/回滚 + Doctor + PVE LXC 能力检测"的项目。`Morningxxx/rATC` 是 Rust TUI 但驱动 xray-core；`potoo0/mihomo-tui` 是 Rust 但**明确声明不管理配置文件**（*"It does not manage any actual configuration files"*）。

---

## 3. Feature Matrix

**图例**：`✅` = 有明确上游文档/源码证据；`◐` = 部分支持/有边界（脚注说明）；`❌` = 上游显式不支持或未提及；`[未验证]` = 本次抓取无法确认（不得当作 ❌）；`—` = 不适用。
**每一格的事实依据见第 8 节来源。** 空白格一律 `[未验证]`，不为填表而猜。

| 项目 | 安装方式 | 内核/版本管理 | 配置校验 | 配置版本化 | 配置回滚 | 订阅管理 | 订阅转换 | Dashboard | CLI | TUI | systemd | PVE LXC 支持 | TUN | nftables/TProxy | 故障自救 | Doctor/环境检测 | 多实例 | 远程管理 | 审计/安全 | 许可 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **本项目 proxy-agent**（目标） | deb/install.sh + systemd | ✅ 指定版本下载/切换 | ✅ `config validate` | ✅ **不可变版本库** | ✅ **`config rollback <id>`** | ✅ | ✅ 可替换 adapter | ◐ 集成 metacubexd | ✅ | ✅ | ✅ | ✅ **能力检测 + 降级** | ✅ 视能力 | ✅ 视能力 | ✅ 保留 last-known-good | ✅ **统一 doctor** | ◐ 单实例优先（MVP 推迟多实例） | ✅ REST + WS | ✅ 审计 + 最小权限 | 待定 |
| ShellCrash | ✅ 一键脚本 | ✅ 内核类型/版本下载 | ◐ 文本启发式校验（非 YAML schema） | ◐ `config.yaml.bak` 单备份 | ◐ `.bak` 覆盖，非按需回滚 | ✅ 订阅链接 + providers | ◐ 规则转换（在线生成/规则 ini） | ✅ 内置多个面板（meta_xd/zashboard/yacd…） | ❌ 交互菜单脚本 | ◐ 全屏菜单（非 TUI 框架） | ✅ systemd/openrc/procd 三份 unit | ◐ 容器布尔检测，无能力降级 | ✅ | ✅ iptables + nftables，TProxy/TUN/redir | ◐ 启动前置检查、失败重试 3 次 | ◐ 启动检查/网络检查脚本，非统一 doctor | ❌ `禁止多实例`（`start.sh:38`） | ◐ external-ui + secret | ◐ 专属用户/面板 secret | GPL-3.0 |
| OpenClash | ✅ OpenWrt ipk/apk | ✅ | ✅ 配置文件校验流程 | `[未验证]` | `[未验证]` | ✅ | ◐ 内置规则/订阅 | ✅ LuCI | ❌ | ❌ | — OpenWrt procd | ❌ OpenWrt 专用 | ✅ | ✅ 依赖 kmod-nft-tproxy / iptables-mod-tproxy | `[未验证]` | `[未验证]` | ❌ | ✅ LuCI 远程 | ◐ 面板账号 | MIT |
| PassWall / PassWall2 | ✅ OpenWrt opkg | ✅ | `[未验证]` | `[未验证]` | `[未验证]` | ✅ | ◐ | ✅ LuCI | ❌ | ❌ | — procd | ❌ OpenWrt 专用 | ✅ | ✅ | `[未验证]` | `[未验证]` | ❌ | ✅ LuCI | `[未验证]` | GPL-3.0 |
| **mihari** | ✅ install.sh + OS 服务 + 离线 AIO 包 | ✅ stable/alpha 通道 + 安装/更新/重启 | ✅ `ValidateConfig` + 发布前校验 | ◐ 原子配置生成 + revision，无历史版本库 | ✅ reload 失败补偿回滚 + 离线切换 | ✅ 独立缓存/间隔/代理 | ◐ 依赖 mihomo 自身 | ✅ 一键装 zashboard / MetaCubeXD | ✅ 完整 CLI + `--json` | ✅ | ✅ systemd/Windows Service/launchd | `[未验证]`（有 TUN 冲突检测，无 LXC 分层） | ✅ 托管 TUN + 冲突检测 | ❌ 未发现 nftables/TProxy 自动化 | ✅ 崩溃退避重启 + 降级控制面 | ◐ 诊断/健康/degraded，非环境能力矩阵 | ◐ 单 daemon，多用户共享 | ✅ loopback Web gateway + Unix socket | ✅ token 认证 + 目录权限 + 日志脱敏 | GPL-3.0 |
| flclash-tui | ✅ deb / 便携 tar / install.sh | ✅ | ✅ `config validate` / `check --config` | ❌ 仅 backup/restore | ✅ 事务失败回滚 | ✅ profile 管理 | ◐ | ◐ TUI 内 Dashboard（非 Web） | ✅ `flclash`/`flc` | ✅ | ✅ deb 内含 tun-helper service | `[未验证]` | ✅ TUN lease（user/system scope） | `[未验证]` | ✅ Core 故障 failover + SSH 隧道恢复 | ✅ `flclash doctor [--json]`（检测项未验证） | ◐ 每用户一个 Backend，多前端 attach | ◐ private Unix socket + SSH 隧道 | ◐ loopback-only listener + UID 校验 | GPL-3.0 |
| WangZhongDian/mihomo-tui | ✅ install.sh + 手动 | ✅ 多版本下载/切换/删除 | ◐ daemon 校验后缓存 | ◐ **保留上一份可用配置** | ◐ 失败时保留 last working | ✅ 订阅池（failover/merge） | ❌ | ❌ | ◐ 少量子命令 | ✅ tview TUI | ✅ | ◐ Docker 容器 TUN 路由修复，无 LXC 能力检测 | ✅ | ◐ 路由配置（实现未验证） | ✅ 订阅失败保留旧配置 | ◐ `tun_diagnose` / `tun_debug` | ❌ 单 daemon | ◐ IPC 授权用户（`grant_operator`） | ◐ IPC + 授权用户 | MIT |
| clashtui | ✅ pacman/AUR + 手动 | ✅ 双内核 | ◐ 模板生成 | ❌ | ❌ | ✅ | ◐ Template | ❌ | ✅ profile/mode/service/update | ✅ | ✅ service 控制 | `[未验证]` | ✅（需 root） | `[未验证]` | `[未验证]` | `[未验证]` | ❌ | `[未验证]` | `[未验证]` | MIT |
| Proxy-RS | ✅ Release 二进制（+ `--root` 便携） | ✅ 从上游 Releases 查/装/切换 | ✅ 校验命令 + 失败恢复原核心 | ❌ 仅"修改前备份" | ✅ 校验失败恢复备份 | ✅ 订阅导入/批量更新 | ❌ | ❌ | ✅ `headless` 子命令 | ✅ Ratatui | ✅ systemd（+ Windows Service/launchd） | `[未验证]` | ✅ tun 模式（按次提权） | ❌ 未发现 nftables/TProxy | ✅ 核心替换失败恢复原核心 | ◐ "故障排查"章节，非命令 | ❌ | ◐ 本机 IPC + 256 位令牌 | ✅ IPC 帧限制/令牌/服务持有高权限 | MIT |
| vpnkit | ✅ install.sh | ✅ `vpnkit update`（自身 + mihomo） | ◐ 节点/规则字段校验 | ❌ | ❌ | ✅ 多源订阅 + 本地节点 | ❌ | ❌ | ✅ 全量 CLI + `--json` | ✅ 7 tab TUI | ✅ systemd **user** unit | ◐ 显式 Out of scope: TUN（非 root） | ❌ | ❌ | ◐ systemd-user 托管 + flock 串行化 | ❌ | ❌ 端口随机化避免同机冲突 | ❌ | ✅ 0600 权限 + basic-auth + netrc 0600 | MIT |
| potoo0/mihomo-tui | ✅ install.sh / cargo | ❌ 仅操作 API | ❌ 明确不管配置文件 | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ CLI 参数 | ✅ Ratatui | ❌ | `[未验证]` | `[未验证]` | `[未验证]` | ❌ | ❌ | ◐ 多控制器（社区分支） | ◐ 通过 API 连远程 controller | ◐ controller secret | MIT |
| verge-tui | `[未验证]` | `[未验证]` | `[未验证]` | ❌ | ❌ | ✅（继承 Verge） | ❌ | ❌ | `[未验证]` | ✅ | `[未验证]` | `[未验证]` | `[未验证]` | `[未验证]` | `[未验证]` | `[未验证]` | ❌ | `[未验证]` | `[未验证]` | GPL-3.0 |
| rATC | ✅ Release / cargo | ◐ xray-core（非 mihomo） | ❌ | ❌ | ❌ | ✅ 多订阅 + 缓存 | ◐ Clash→xray 规则转换 | ❌ | ❌ | ✅ Ratatui | ❌ | `[未验证]` | ❌ | ❌ | ✅ 离线用缓存启动 | ❌ | ❌ | ❌ | ◐ | MIT |
| metacubexd（纯面板） | ✅ 静态托管 / Docker / 桌面 app | ✅（仅 agent 模式：切换已发布版本） | ✅ Monaco schema + **`mihomo -t` 激活前校验** | ❌ profile 列表，非版本历史 | ❌ | ✅ agent 模式 URL 导入 + 用量卡 | ❌ | ✅ 本体 | ◐ `/api/control`（agent） | ❌ | ❌ 容器内监督 | ❌ | ◐ 取决于配置 | ❌ | ◐ 内核崩溃日志 SSE，无自动回滚 | ◐ 故障排查文档 | ✅ 多 profile | ✅ 浏览器远程 | ◐ `CONTROL_TOKEN` / `CLASH_SECRET` | MIT |
| Sub-Store | ✅ Node/Docker/模块 | — | — | — | — | ✅ 本体 | ✅ 本体（脚本引擎） | ✅ 自带前端 | ◐ API | ❌ | — | — | — | — | — | — | ✅ 多订阅/多设备 | ✅ HTTP API | ◐ API token | AGPL-3.0 |
| subconverter | ✅ Docker/二进制 | — | — | — | — | ◐ | ✅ 本体 | ◐ 简易 | ❌ | ❌ | — | — | — | — | — | — | — | ✅ HTTP API | ◐ | GPL-3.0 |
| sub-store-convert | ✅ Node/Bun/Docker/CF Worker | — | — | — | — | ◐ | ✅ 本体（Sub-Store 内核抽出） | ❌ | ✅ CLI | ❌ | — | — | — | — | — | — | — | ✅ HTTP API（subconverter 兼容 `/sub`） | ◐ | MIT |
| clash-for-linux-install (`clashctl`) | ✅ git + install.sh | ✅ 内核升级 | `[未验证]` | `[未验证]` | `[未验证]` | ✅ 多订阅源/切换/更新 | ✅ 集成 subconverter | ✅ Web 面板一键装 | ✅ `clashctl` | ❌ | ✅ systemd/OpenRC | ◐ 声称适配容器环境，无能力检测 | ✅ | `[未验证]` | `[未验证]` | ❌ | ❌ | ◐ 访问密钥 | ◐ 访问密钥 | MIT |
| clash-verge-rev / clash-party | ✅ GUI 安装包 | ✅ | ✅ | ◐ 桌面配置多 profile | ◐ | ✅ | ◐ | ✅ GUI | ❌ | ❌ | ❌ | ❌（桌面） | ✅ | ✅（桌面） | ◐ | ❌ | ❌ | ❌ 本地 | ◐ 本地 | GPL-3.0 |

**Matrix 关键读法（差异化定位）**：

- 只有本项目把 **配置版本化（不可变历史）** 与 **显式按版本回滚** 同时作为一等能力；其余全部停留在"单备份 / last-working / revision 补偿"。
- **PVE LXC 能力检测与降级** 这一列，全表**没有任何** ✅，最多是布尔容器判断（ShellCrash）或 TUN 冲突检测（mihari）。
- **Doctor/环境检测** 列同样只有零星点位（`flclash doctor`、`tun_diagnose`、Proxy-RS 排障章节），没有统一能力矩阵。
- **审计/安全** 列中，真正做到"权限分离 + 审计 + 默认 loopback + 脱敏"的只有 mihari / Proxy-RS / vpnkit 三家部分覆盖。

---

## 4. 逐项目分析（定位 / 重叠 / 可借鉴点）

### 4.1 ShellCrash（`juewuy/ShellCrash`，GPL-3.0，13.3k star）`[API]`+`[上游源码]`

- **定位**：Shell 脚本形态的 mihomo/sing-box 部署管理工具，覆盖 OpenWrt 路由器、标准 Linux、Docker/PVE。安装方式是 `curl | sh` 式一键脚本（`install.sh` / `install_en.sh`）。
- **与我们重叠**：内核类型/版本下载切换；订阅导入与定时更新；TUN / TProxy / redir 转发模式；nftables 与 iptables 双后端；systemd / openrc / procd 三种服务定义；内置多种 Web Dashboard（`bin/dashboard/` 下有 zashboard、meta_xd、meta_yacd、yacd、clashdb）；配置备份。
- **证据要点（全部来自本次下载的 1.9.5beta3 源码包）**：
  - 配置校验是**文本启发式**而非 YAML schema：`starts/clash_config_check.sh` 用 `sed`/`grep` 判断是否存在 `server:`、是否含 `proxy-providers:`、是否旧格式、是否含已废弃的 `cipher: chacha20`，并会**直接 `sed -i` 删掉它认为无效的策略组**。
  - 配置替换是"备份 + 覆盖"：`starts/core_config.sh:100` — `compare` 不同则 `mv -f "$core_config" "$core_config".bak && mv -f "$core_config_new" "$core_config"`。只有一个 `.bak`，没有版本目录、没有 `rollback <id>`。
  - 备份/还原在菜单里：`menus/2_settings.sh` 用 `tar -zcf "$CRASHDIR/configs.tar.gz"` 做备份、`tar -zxf` 做还原（还原前把当前配置再备份一份到同一个 tar，即"上一次"语义）。
  - 容器检测是**布尔**：`init.sh:20` — `grep -qE '/(docker|lxc|kubepods|crio|containerd)/' /proc/1/cgroup ... && systype='container'`；容器分支只把 `CRASHDIR` 改成 `/etc/ShellCrash`（`init.sh:22`），**不产出能力降级模型**。
  - 防火墙后端检测：`init.sh:98-99` — 默认 `firewall_mod=iptables`，`nft add table inet shellcrash` 成功则切 `nftables`。
  - systemd unit 的 `ExecStart` 以 `shellcrash` 用户运行核心（`starts/shellcrash.service`），但该用户在 `starts/shellcrash.procd` 里被检查为 `shellcrash:x:0:7890`（**UID 0**），实际仍是 root 权限——这是一个值得在安全设计中避开的模式。
  - **禁止多实例**：`start.sh:38` 与 `start.sh:112` — `[ -n "$(pidof CrashCore)" ] && $0 stop`。
  - 内核升级是替换而非并存：`starts/core_exchange.sh` 直接 `rm -rf CrashCore` 后换新核心，**没有旧内核保留**。
- **可借鉴点**：多 init 系统兼容（systemd/openrc/procd）的封装思路；nftables/iptables 运行时探测；`bfstart`/`afstart`/`fw_stop` 的前后置钩子划分（对应我们的 pre/post activation hooks）；容器检测至少要有。
- **不该学的**：文本启发式改配置（会破坏用户配置）、单一 `.bak`、UID 0 的"专用用户"。

### 4.2 metacubexd（`MetaCubeX/metacubexd`，MIT，4.3k star）`[API]`+`[上游 README]`

- **定位**：Mihomo 官方 Web Dashboard。**三种形态**：纯面板（指向任意远端 mihomo）、桌面 app（Electron + 内置内核）、**all-in-one server（`ghcr.io/metacubex/metacubexd-server`，UI + control agent + 内置 mihomo）**。
- **重叠（重要，容易低估）**：在 agent 模式下，它已经做了：
  - Profile 管理器：创建/复制/改名/删除多 profile，URL 导入订阅（带 `Subscription-Userinfo` 用量卡片）；
  - Monaco 编辑器内嵌 **mihomo YAML schema 校验**（advisory，不阻止保存）；
  - **激活 profile 时用 `mihomo -t` 做最终校验，只有校验通过才重启内核**（"Activate a profile to compose it, validate it with mihomo -t, and restart the kernel only after validation succeeds"）；
  - 内核版本管理：桌面端可下载/切换已发布 mihomo 版本；server 支持 `MIHOMO_BIN` 自定义路径；
  - 内核进程 stdout/stderr 通过 SSE 暴露（"Kernel logs, control agent"），容器内监督内核；
  - `/api/control` agent API + `CONTROL_TOKEN` 认证；TUN 通过 profile 配置 + compose override 支持。
- **不重叠 / 空白**：**没有配置版本历史与回滚**（只有 profile 列表 = 配置的"并行分支"，不是"同一配置的时间线"）；没有内核升级失败自动回滚；没有环境能力检测/Doctor；没有 LXC 语义；没有审计（除 token）；不提供系统级 systemd 托管（server 形态是容器内监督）。
- **对项目的直接影响**：**Dashboard 绝不自研**。我们应把 metacubexd 作为可替换的前端（`frontend/metacubexd`），Agent 只负责提供外部控制器与（必要时）profile 数据的接口。注意：metacubexd agent 已具备部分"配置校验 + 校验后重启"能力，**它的存在意味着"Web 端配置编辑"不再是我们的差异化**。

### 4.3 Sub-Store / subconverter / sub-store-convert（订阅转换）`[API]`+`[上游 README]`

- **Sub-Store**（AGPL-3.0，10.4k star）：高级订阅管理器，支持 QX/Loon/Surge/Stash/Egern/Shadowrocket，带脚本引擎和自带前端。定位是"订阅的全生命周期（多设备同步、节点处理、脚本）"，比我们的订阅管理野心大得多。
- **subconverter**（GPL-3.0，17.1k star）：C++ 多格式转换器，`/sub?target=...&url=...` HTTP API。它是**事实标准接口**。
- **sub-store-convert**（MIT，2 star，`tbxark/sub-store-convert`）：把 Sub-Store 的节点转换逻辑抽成独立 core/CLI/HTTP/Cloudflare Worker，**HTTP API 兼容 subconverter 风格 `/sub`**，`@sub-store-convert/core` 导出 `convert()`。
- **对我们的意义**：`AGENTS.md` 与设计文档要求"`SubscriptionConverter` 可替换"是完全正确的方向，而且**已经有三个不同许可的实现可以挂到同一个 Port 后面**：Sub-Store（AGPL-3.0，注意传染性）、subconverter（GPL-3.0）、sub-store-convert（MIT，最适合作为默认/内嵌候选）。**不要自研完整转换器**（设计文档已把"full native subscription converter"列为 defer）。

### 4.4 OpenClash / PassWall / PassWall2（OpenWrt 路由器）`[API]`+`[上游 README]`

- **定位**：OpenWrt LuCI 插件，管理 Clash/Mihomo（OpenClash）或 sing-box/xray（PassWall/PassWall2）。依赖 `kmod-tun`、`iptables-mod-tproxy`、`kmod-nft-tproxy`、`dnsmasq-full`、`ruby` 等。
- **重叠**：内核版本管理、配置校验、TProxy/TUN、规则与订阅、Web UI（LuCI）。
- **明确不竞争**：这些是**路由器固件生态**（opkg/ipk/apk + procd + LuCI），不是服务器生态。我们的目标环境 Debian/Ubuntu + systemd + PVE LXC 与它们的交付形态没有交集。**不应把 OpenWrt 支持列入路线图**（设计文档也未列）。唯一可借鉴的是它们对 nftables/iptables TProxy 的工程经验（作为未来 Linux 网络栈的参考实现）。

### 4.5 桌面端：clash-verge-rev / clash-party（mihomo-party）`[API]`

- **clash-verge-rev**（GPL-3.0，143.9k star，Rust + Tauri）：Windows/macOS/Linux 桌面 GUI；有 TUN、系统代理、订阅、配置多 profile 与"配置增强"。
- **mihomo-party / clash-party**（GPL-3.0，26.4k star，Electron）：另一款桌面 GUI。
- **为何不构成服务器端竞品**：交付物是 GUI 安装包，依赖桌面会话（托盘/系统代理/自动启动），没有"daemon + 远程 API + 无头 CLI/TUI"的形态；无法在 PVE LXC 里无头运行并远程编排。**不竞争桌面 GUI 场景。**
- 但要注意：`clash-verge-rev` 是 **Rust** 实现且开源（GPL-3.0），它的部分基础设施（如内核下载/校验、服务模式）可以作为工程参考，**但不能复制代码**（GPL-3.0 传染性 + 我们的许可待定）。

### 4.6 直接竞品群：mihari / flclash-tui / WangZhongDian-mihomo-tui / Proxy-RS / clashtui / vpnkit

这一组是本报告最重要的发现，逐一说明。

#### 4.6.1 mihari（Go，GPL-3.0，118 star）`[上游 README]`+`[上游 docs/architecture.md]`

- **定位**：跨平台 mihomo 管理器，"**CLI, TUI, and browser panels share one daemon-owned control plane**"（与本项目设计文档的 Agent/Client separation 几乎同构）。
- **已覆盖（比预想的多得多）**：
  - 单一 daemon 控制面，CLI/TUI/Web 都通过本地命名管道 / Unix socket 接入，**控制 API 从不绑定 TCP 端口**（安全模型比 ShellCrash 好）；
  - 订阅：per-subscription 独立缓存、离线切换、独立刷新间隔、per-profile fetch proxy（direct/proxy/auto）、**validated atomic config generation with rollback**；
  - Web 面板：一键 install / update / activate / **rollback** zashboard 与 MetaCubeXD，loopback Web gateway + 独立访问凭据；
  - 内核：install/update/restart，stable/alpha 通道切换，安装事务（`transactions/<ID>/`）与恢复；
  - 服务：Windows Service / systemd / launchd，崩溃退避重启（crash backoff restart）；
  - TUN / 系统代理：daemon 托管、持久化，遇到外部 TUN/系统代理冲突时拒绝执行（`tun_conflict` / `system_proxy_conflict`）除非 `--force`；
  - 架构文档中明确写入：**单文件 replace 是提交点**、replace 成功后 sync 失败只记 durability warning 不回滚、补偿失败则 `health=degraded` 且后续 mutation 返回 `invalid_state`；订阅链有 `prepareContent → ValidateConfig → commitRuntimeConfig(reload/restore/reload)` 的补偿顺序与 generation/hash/revision 概念；
  - 日志脱敏：显式要求订阅 URL、token、路径不进入 JSON；服务器 controller secret、浏览器凭据不输出。
- **未覆盖（我们的空隙）**：**没有面向用户的配置版本历史**（只有 revision 与 last-error，不是可列举/可 diff/可按 ID 回滚的版本库）；没有 nftables/TProxy 透明代理自动化（只有 TUN）；没有 LXC 能力检测；没有统一 Doctor 输出的能力矩阵；Go 而非 Rust（对我们不是问题，但说明"Rust"本身不是差异化）。
- **威胁等级：高。** 它的架构文档质量与失败语义设计与我们的目标高度重合。如果我们只交付"更好的失败语义"，它随时可以追平。

#### 4.6.2 flclash-tui（Dart/Flutter，GPL-3.0，122 star）`[上游 CLI_LINUX.md]`

- **定位**：*"Linux/SSH/headless Mihomo TUI. Default silent: only flc COMMAND uses the proxy."* 面向无头 Linux 服务器 + SSH 使用场景。
- **已覆盖**：Backend（per-user coordinator，唯一受管写入者）/ Core（mihomo）分离；CLI 与 TUI **提交带 revision 的事务到 Backend**；"Backend validates, writes atomically, reloads Core, and rolls back on failure"；`flclash config path|show|validate|edit|backup|restore`；`flclash check --config PATH`；`flclash doctor [--json]`；TUN 作为独立 lease（user/system scope，需 root helper）；系统代理；`flc COMMAND` 单命令代理包装；多 TUI 前端 attach 同一 Backend；Up-to-500 条 persistent History；端口变更也走事务并 rollback。
- **未覆盖**：无不可变配置版本库（backup/restore 是单份）；doctor 检测项未验证；无 LXC；无 nftables/TProxy；Dart 运行时依赖较重（对最小化服务器镜像不友好）。
- **威胁等级：中高。** 它的"事务 + 原子写 + 失败回滚 + doctor + 无头优先"几乎就是我们的 MVP 描述；差异点只剩版本历史、LXC 能力检测、审计。

#### 4.6.3 WangZhongDian/mihomo-tui（Go，MIT，4 star）`[上游 README]`

- **定位**：*"targets headless Linux servers: the TUI communicates with an IPC daemon to manage the mihomo process, subscriptions, rules, kernel versions, and networking safely."*
- **已覆盖**：IPC daemon + TUI 客户端（root 装服务、普通用户跑客户端）；订阅池（failover/merge）+ **"preserves the last working version"** + 离线引导；内核多版本下载/切换/删除；规则启用/禁用/排序/恢复；systemd 服务安装/卸载；TUN + `tun_diagnose` / `tun_debug`（打印 TUN 路由 dry-run 计划，默认不改动系统）；Docker TUN 兼容修复；`grant_operator <user>` 授权普通用户访问 IPC。
- **未覆盖**：无配置版本历史（只有 last-working）；无 Web Remote API（只有 IPC）；无 nftables/TProxy；无 LXC 能力模型；star 极少、活跃度低（`july`）。
- **威胁等级：低-中。** 思路与我们高度一致但工程体量与活跃度不足；不过它证明了"IPC daemon + TUI 客户端 + 非 root 客户端"这一形态在服务器端是自然解法，我们的 `proxyctl ↔ proxy-agent (Unix socket)` 设计应保留。

#### 4.6.4 Proxy-RS（Rust/Ratatui，MIT，5 star）`[上游 README]`

- **定位**：Rust 实现的 sing-box + mihomo 跨平台管理器，默认 `service` 后端（Linux systemd）持有高权限核心，TUI/托盘/CLI 保持普通用户权限。
- **已覆盖**：内核选择/启动/停止/重启/**校验**/版本展示，并从上游 Releases 安装（支持镜像回退）；"可靠更新"——替换失败时**恢复原核心**；"结构化配置处理"——修改前创建备份，**校验失败则恢复**；四种代理模式（system/tun/mixed/manual）；本机 IPC（协议 v2、帧上限 1 MiB、32 字节令牌认证、安装用户访问控制）；`headless` 脚本友好子命令；systemd/Windows Service/launchd。
- **未覆盖**：无版本历史（只有备份+恢复）；无 nftables/TProxy；无 LXC；无统一 doctor；发布资产未签名（README 自己声明）。
- **威胁等级：低。** 但它是**唯一**同时具备"Rust + systemd 服务后端 + 配置备份/校验/恢复 + IPC 令牌"的项目，是我们在技术形态上最接近的同类；它的失败处理证明了"校验失败恢复备份"是业界可接受的下限，而我们要做的是把它提升为版本化。

#### 4.6.5 clashtui（Rust，MIT，670 star）`[上游 README]`+`[Cargo.toml]`

- **定位**：Rust TUI，同时支持 Mihomo 与 sing-box 双内核；有 profile（File/URL/Template）、节点切换与测速、连接监控、**通过 systemd 管理核心启停重启**、日志、CLI 子命令（`profile`/`mode`/`service`/`update`）、`core_override_config` 覆盖、Template 系统。
- **未覆盖**：无配置版本化/回滚；无 doctor；无 LXC；无 nftables/TProxy（TUN 需 root）；无 Web。
- **威胁等级：低-中。** 它是"TUI + systemd"路线的代表；**star 数（670）说明该形态有真实用户需求**。

#### 4.6.6 vpnkit（Go，MIT，19 star）`[上游 README]`

- **定位**：单 Go 二进制、**完全非 root、无 daemon**（与我们的 Agent 形态相反）。
- **已覆盖**：多源订阅 + 手输本地节点 + 本地规则（本地规则永远优先于订阅规则）；`vpnkit update`（自身 + mihomo）；systemd **user** unit；CLI 全量 + `--json` + 稳定退出码；并发变更用 POSIX flock 串行化；端口随机化（30000-60000）避免同机多用户冲突；`allow-lan:false` + `bind-address:127.0.0.1` + basic-auth，凭据 0600。
- **显式不覆盖**：*"Out of scope: TUN mode, Windows/macOS, command palette, theme switcher, GUI."*
- **威胁等级：低（形态不同）**，但**它的安全与并发细节值得直接借鉴**：flock 串行化 config 变更、随机端口避免多用户冲突、0600 凭据、`--json` + 退出码契约。这些正好对应我们"per-instance lock / 并发激活协调 / 不泄漏凭据"的要求。

### 4.7 其他

- **potoo0/mihomo-tui**（Rust，MIT，129 star）：纯 API 仪表盘，README 明确写 *"The tool is designed only to interact with the API. It does not manage any actual configuration files."* → **不与本项目重叠**，但它是"TUI 只管运行时"的分工样本。
- **clash-for-linux-install / clashctl**（MIT，14.8k star）：Linux 一键脚本，`clashctl on/off/status/ui/sub add/sub update/node`，集成 subconverter，支持 systemd/OpenRC。**是一个"够用"的低成本替代品**——如果用户只需要"装好能用"，它已经足够。我们的价值必须体现在它做不到的：版本化、回滚、Doctor、审计。
- **rATC**（Rust，MIT，0 star，june）：Rust TUI 但驱动 xray-core（Clash YAML → xray 配置），不是 mihomo 管理器，**不算竞品**。
- **verge-tui / nkanf-dev/mihomot / cublae/mihomo-manifold**：早期或小众项目（8 / 6 / 0 star），活跃度低（july / may / 上周）。`mihomo-manifold` 是 GTK4 桌面 GUI，不属服务器端。
- **Nezha 类监控**：按任务说明，监控面板不计入竞品。

---

## 5. 差异化分析

### 5.1 竞品已覆盖 → 我们**不应该重做**

| 能力 | 已被谁覆盖 | 我们的策略 |
|---|---|---|
| Mihomo 二进制下载/版本切换 | ShellCrash、mihari、metacubexd agent、Proxy-RS、mihomo-tui(Go)、clashtui、clash-verge-rev | 只做到"够用即可"（下载 + 校验 + 原子替换），**不投入差异化资源** |
| Web Dashboard（流量/连接/规则/日志/节点切换） | **metacubexd（官方）**、OpenClash LuCI、ShellCrash 内置面板、mihari 面板管理 | **不自研**，集成为可替换前端，见 `docs/research/07-metacubexd.md` |
| 订阅抓取/多订阅/定时刷新 | ShellCrash、mihari、vpnkit、mihomo-tui(Go)、clashctl | 做最薄的一层（下载 + 缓存 + 调度），不重做 Sub-Store |
| 订阅转换（格式互转/脚本/规则处理） | Sub-Store、subconverter、sub-store-convert | **只做 `SubscriptionConverter` Port + 适配器**，不自研转换内核 |
| 节点延迟测试/连接管理/日志流 | 所有 Dashboard 与 TUI | 复用 Mihomo API，不重做 |
| systemd unit 安装与管理 | ShellCrash、mihari、Proxy-RS、clashtui、vpnkit | 按 Debian/Ubuntu 规范做一份打包，不做多 init 抽象（OpenWrt/K8s 不在范围） |
| TUN 开关 | ShellCrash、OpenClash、mihari、flclash-tui、Proxy-RS、mihomo-tui(Go) | 只做"能力检测 + 配置写入 + 冲突拒绝"，不重做 TUN 数据面 |
| TProxy / nftables 透明代理 | OpenClash、PassWall、ShellCrash | **MVP 不做**（设计文档已列为 defer），仅在 Doctor 中报告能力状态 |
| Profile（多份配置）管理 | metacubexd agent、mihari、flclash-tui | 与我们的"配置版本"是不同的概念：profile 是并行分支，版本是时间线。**两者都做**，但要讲清楚区别 |

### 5.2 竞品未覆盖 → 我们的价值主张

| 能力 | 现状（证据） | 我们的差异化定义 |
|---|---|---|
| **不可变配置版本库 + 显式回滚** | 所有竞品最多做到"单 `.bak`"（ShellCrash）、"last working"（mihomo-tui Go）、"backup/restore"（flclash-tui）、"修改前备份 + 校验失败恢复"（Proxy-RS）、"revision + reload 补偿"（mihari）。**没有一个提供 `config list / show / diff / activate <id> / rollback <id>`** | 每次激活生成带 checksum 的不可变版本；`proxyctl config list/show/diff/activate/rollback` 面向历史版本；**任意历史版本可回滚**，而不是只有"上一份" |
| **激活的原子性与健康检查回滚** | flclash-tui / mihari 已做到"校验 + reload + 失败回滚"，这已是行业下限 | 我们做**更完整的序列**：写临时文件 → fsync → 原子 rename → 激活记录 → reload → health check → 失败自动回滚到上一个 known-good，并把整个序列作为单一 Application Use Case 暴露给 CLI/TUI/Web |
| **PVE LXC 能力检测与降级** | **全表空白**。ShellCrash 只有 `systype='container'` 布尔；mihomo-tui(Go) 只修 Docker TUN 路由 | 把 `Supported / Unsupported / Unavailable / Misconfigured / Unknown` 作为一等模型；privileged 与 unprivileged LXC 分开建模；**TUN 不可用时 HTTP/SOCKS 仍必须正常工作**，并把这个降级状态显式呈现 |
| **统一 Doctor** | 只有 `flclash doctor`（检测项未验证）、`tun_diagnose`（仅 TUN 路由 dry-run）、Proxy-RS 的排障文档 | Doctor 输出**完整能力矩阵**：`/dev/net/tun`、`CAP_NET_ADMIN`、nftables、iptables、systemd、LXC 类型、DNS、端口占用、内核版本兼容性；机器可读（`--json`）且同一 Use Case 供 CLI/TUI/Web 复用 |
| **同一 Use Case 供 CLI/TUI/Web** | mihari 已明确做到"one daemon, three surfaces"；flclash-tui 做到"CLI/TUI 共享 Backend" | 这已**不再是独占优势**；我们的差异在于**用 Hexagonal + DDD-lite 把 Use Case 边界固化**，确保三个 Adapter 不可能各自实现业务逻辑（可用架构测试/依赖规则验证） |
| **可替换转换器（Port）** | 生态里已有 Sub-Store / subconverter / sub-store-convert 三个实现，但竞品都是硬集成某一个 | 我们把它抽象成 `SubscriptionConverter` Port，**默认 sub-store-convert（MIT）+ 可选 Sub-Store（AGPL）**，切换不改应用层 |
| **审计与最小权限** | mihari（token + 目录权限 + 脱敏日志）、Proxy-RS（IPC 令牌 + 服务持有高权限）、vpnkit（0600 + basic-auth）部分覆盖；**没有任何一个提供变更审计日志**（谁在何时激活/回滚了哪个配置版本） | 配置激活/回滚/订阅更新/内核更新都产生**审计事件**（含操作者、来源 Adapter、版本 ID、结果）；Web 层永不暴露任意命令执行；敏感字段默认脱敏 |

### 5.3 我们**不打算竞争**的场景

1. **OpenWrt 路由器**（OpenClash / PassWall / PassWall2）：交付形态（opkg/ipk、procd、LuCI）、依赖模型（kmod-*）、目标硬件完全不同。我们的目标环境是 Debian/Ubuntu + systemd + PVE LXC。
2. **桌面客户端**（clash-verge-rev / clash-party / metacubexd desktop / mihomo-manifold）：依赖桌面会话（托盘、系统代理、GUI），不提供"无头 daemon + 远程编排"。我们不但不做 GUI，还要**明确不把系统代理作为核心场景**。
3. **Kubernetes**（设计文档已列 defer）：不做 Operator/CRD/多节点编排。
4. **代理内核本身**（Mihomo）与**完整 Dashboard**、**完整 Sub-Store**：设计文档已明确不重实现。
5. **一键脚本的"装完即用"竞争**（clash-for-linux-install，14.8k star）：我们不打"最短安装命令"的仗；如果用户只需要这个，`clashctl` 已经赢了。

---

## 6. 替代风险与回应

**问题：如果 ShellCrash 或 OpenClash 增加了配置版本化 / Doctor，我们的差异化还剩什么？**

**诚实的评估：会显著削弱，但不会归零。** 分三层看：

### 6.1 风险等级评估（按威胁高低排序）

| 竞品 | 补齐"版本化 + Doctor"的可能性 | 补齐后的剩余差异化 |
|---|---|---|
| **mihari** | **高**。它已经有 revision、generation/hash、原子提交点、补偿回滚、degraded 状态、诊断体系与完整架构文档；从"revision"扩展到"可列举的版本历史"是自然的下一步 | 只剩：Rust/资源占用、PVE LXC 能力分层、nftables/TProxy 路线、许可（GPL-3.0 vs 我们待定）。**风险最高** |
| **metacubexd（官方 agent）** | **中高**。它已经在官方 Dashboard 内做"compose → `mihomo -t` → 校验通过才重启"，而且有官方生态加持 | 它受限于"Web/容器内"定位：不易做 systemd 级服务托管与主机级能力检测（LXC/CAP_NET_ADMIN 在容器视角是宿主问题） |
| **ShellCrash** | **中**。它是"维护脚本"，加 `.bak` 轮转（多份备份 + 选择还原）成本很低；但它的架构（Shell 文本启发式改配置、单一实例、UID 0 服务）决定了它很难做到"带 checksum 的不可变版本 + 结构化校验 + 审计" | 即使有版本化，仍缺：结构化校验、能力模型、审计、安全模型、可测试性 |
| **flclash-tui** | **中**。已有事务 + 回滚 + doctor 命令；扩展成版本库需要新增存储层 | 缺 LXC、nftables、审计；Dart 运行时 |
| **OpenClash** | **低-中**。OpenWrt 生态，与服务器场景不重叠；它加 Doctor 也不影响我们在 PVE LXC 的价值 | 场景不重叠 |

### 6.2 差异化"护城河"排序（从最稳到最不稳）

1. **PVE LXC 能力检测与降级语义**（最稳）：这是**场景绑定**的差异化，不是功能差异。桌面/路由器/容器内 Dashboard 项目没有动力去做"privileged vs unprivileged LXC、CAP_NET_ADMIN 缺失时如何优雅降级"这件事，因为它们的部署面不在这里。mihari 的 TUN 冲突检测是最接近的，但它是布尔拒绝（`tun_conflict`），不是能力分层。
2. **不可变版本库 + 任意版本回滚**（较稳，但会被追）：语义清晰、用户价值明确，但工程上不难，任何有 revision 的项目都能扩展。
3. **审计事件（谁/何时/激活或回滚了哪个版本）**（较稳）：竞品普遍没有，因为它们的操作者模型简单（单用户本地）。一旦我们强调"多 Adapter、可能远程 Web"，审计就成为刚需，而它们要补需要改数据模型。
4. **同一 Use Case 的架构约束（Hexagonal 依赖规则 + 架构测试）**（不稳，但"可验证"）：架构本身不构成用户价值，但如果能通过架构测试证明"CLI/TUI/Web 行为一致"，这是竞品（Go 单体 + 多前端、Shell 脚本）难以低成本复制的质量属性。
5. **Rust 本身**（最不稳）：clashtui、Proxy-RS、potoo0、rATC、verge-tui 都是 Rust。**"用 Rust 重写"不构成任何差异化**，这一点必须在设计文档里说清楚（设计文档第 1 节已正确写明"不是 ShellCrash 的 Rust 重写"）。

### 6.3 回应策略（对产品与范围的建议）

1. **把"版本化 + 回滚 + Doctor + LXC 降级"做成一条完整闭环，而不是四个散点功能。** 竞品各自只有一个点；我们的壁垒来自"失败时永远能回到 last-known-good，且用户能看见为什么"这一不变量（与设计文档第 75 节"最终核心原则"第 8、9 条一致）。
2. **主动把 Dashboard/转换器做成外部依赖**，把节省的工程量投入到 Doctor 与版本语义；不要在被覆盖的战场上消耗。
3. **不宣称"唯一竞品"或"没有竞品"**：R15 的证据表明这是错误的。产品叙事应是"面向 PVE LXC 的、可回滚的、可审计的 Mihomo 控制面"。
4. **设定时间窗意识**：mihari 是最可能追平的项目，建议在 MVP 中优先交付"配置版本库 + rollback + doctor --json"三件套，而不是先做 Web UI。
5. **关注 mihari 与 metacubexd 的发布节奏**（本次抓取：mihari `yesterday` 有提交、v0.9.3；metacubexd `last thursday` 有提交）。建议 Phase 0 结束时把这两个仓库列入"持续观察清单"。

---

## 7. 对 Agent 架构与产品范围的影响

1. **Port 设计验证**：`SubscriptionConverter` Port 有至少三个真实实现可挂载（Sub-Store / subconverter / sub-store-convert），设计文档的"可替换转换器"不是空想。建议默认 `sub-store-convert`（MIT），Sub-Store 作为可选（AGPL-3.0 需在 `docs/research/13-licenses.md` 单独结论）。
2. **Process/Service 抽象验证**：ShellCrash 用三份 init 定义（systemd/openrc/procd）、Proxy-RS/mihari/flclash-tui/WangZhongDian 都用"服务后端持有高权限核心 + 普通用户前端"。**我们的 `ProcessManager` / systemd 适配器方向正确**，且应坚持"高权限只属于 Agent 的特定 Use Case"。
3. **配置存储模型**：竞品的 .bak/last-working/backup-restore 说明行业下限很低，我们的 `configs/v001.yaml ... v004.yaml + active -> v004.yaml`（设计文档已写）是正确的升级方向；**必须补上"版本元数据（时间、来源、checksum、激活结果）持久化"**，否则只是文件目录，不是版本库。
4. **TUI 范围**：`potoo0/mihomo-tui` 证明"TUI 只管运行时"是可行分工；`clashtui`（670 star）证明"TUI + systemd"有需求；`WangZhongDian` 的 `tun_diagnose` 提示 TUI 应能展示 Doctor 结果。我们的 TUI 优先级（overview/status/groups/logs/subscription/config rollback/doctor，AGENTS.md 已列）与市场一致。
5. **Web 范围**：metacubexd agent 已覆盖"Web 编辑 + 校验 + 重启"，因此**我们的 Web UI 应聚焦 Agent 自身状态**（配置版本列表/diff/回滚、Doctor、审计、订阅、内核版本），并把 Dashboard 交给 metacubexd。
6. **多实例**：ShellCrash 明确禁止多实例，vpnkit 用随机端口避免同机多用户冲突，mihari 是单 daemon 多用户共享。设计文档把多实例列为 defer 是合理的；但**per-instance 锁**必须从第一版就有。
7. **安全模型**：ShellCrash 的 UID 0 "专用用户"是反面教材；vpnkit 的 `bind-address: 127.0.0.1` + `allow-lan:false` + basic-auth + 0600 凭据、mihari 的"控制 API 不绑定 TCP"与日志脱敏、Proxy-RS 的 IPC 帧上限与令牌认证，都应作为我们的默认基线。
8. **命名与叙事**："Mihomo 管理 Agent"这个词已被 mihari（"Manager"）、metacubexd（"control agent"）、mihomo-tui(Go)（"daemon management tool"）大量使用。建议产品叙事聚焦"**可回滚的（rollback-first）PVE LXC Mihomo 控制面**"。

---

## 8. 证据与来源

**抓取方式**：`api.github.com` REST（未认证，2026-09-12 12:36–12:50 CST，额度耗尽后改用 shields.io badge JSON 与 `raw.githubusercontent.com`）；ShellCrash 源码通过 `https://ghfast.top/https://github.com/juewuy/ShellCrash/archive/refs/heads/master.tar.gz` 下载后本地读取（版本 `1.9.5beta3`）。
**说明**：`github.com` HTML 直连与 `ghfast.top`/`ghproxy.net` 的 GitHub HTML/API 代理均被拒绝（403）；`raw.githubusercontent.com` 可用。

### 8.1 GitHub API 抓取原始结果（2026-09-12）

```text
juewuy/ShellCrash               stars=13261  forks=1862  license=GPL-3.0  lang=Shell  created=2020-07-08  pushed=2026-09-11T18:17:21Z
MetaCubeX/metacubexd            stars=4340   forks=520   license=MIT      lang=TypeScript  created=2023-07-11  pushed=2026-09-10T07:03:05Z
sub-store-org/Sub-Store         stars=10450  forks=1359  license=AGPL-3.0 lang=JavaScript  created=2020-08-19  pushed=2026-09-12T00:46:21Z
vernesong/OpenClash             stars=27437  forks=4016  license=MIT      lang=HTML  created=2019-05-29  pushed=2026-09-11T20:18:39Z
tindy2013/subconverter          stars=17060  forks=3845  license=GPL-3.0  lang=C++  created=2019-10-31  pushed=2026-07-09T15:40:27Z
nelvko/clash-for-linux-install  stars=14754  forks=1634  license=MIT      lang=Shell  created=2024-03-11  pushed=2026-09-07T07:21:25Z
clash-verge-rev/clash-verge-rev stars=143901 forks=10359 license=GPL-3.0  lang=Rust  created=2023-11-21  pushed=2026-09-12T02:26:30Z
mihomo-party-org/clash-party    stars=26378  forks=...   license=GPL-3.0  lang=TypeScript  pushed=2026-09-12T01:54:16Z   （仓库已由 mihomo-party 改名为 clash-party）
Openwrt-Passwall/openwrt-passwall  stars=9909 forks=2975 license=GPL-3.0 lang=Lua  created=2018-10-06  pushed=2026-09-12T03:57:21Z
Openwrt-Passwall/openwrt-passwall2 stars=3566 forks=742  license=GPL-3.0 lang=Lua  created=2022-03-06  pushed=2026-09-11T16:44:49Z
tbxark/sub-store-convert        stars=2      forks=1     license=MIT      lang=JavaScript  created=2024-11-15  pushed=2026-09-11T06:38:41Z
```

### 8.2 shields.io 抓取结果（2026-09-12，聚合值）

```text
JohanChane/clashtui         stars=670  license=MIT      last=august      release=v0.3.1
potoo0/mihomo-tui           stars=129  license=MIT      last=july        release=v0.4.5
yqlay/flclash-tui           stars=122  license=GPL-3.0  last=last tuesday release=v0.5.28
mihari-proxy/mihari         stars=118  license=GPL-3.0  last=yesterday   release=v0.9.3
WangZhongDian/mihomo-tui    stars=4    license=MIT      last=july        release=v0.2.1
JimZhang168872/vpnkit       stars=19   license=MIT      last=july        release=v1.0.4
MiChongs/Proxy-RS           stars=5    license=MIT      last=july        release=v0.1.0
totrytakeoff/verge-tui      stars=8    license=GPL-3.0  last=july        release=v0.2.0
xream/mihomo-tui-dashboard  stars=10   license=GPL-3.0  last=january     release=v0.1.1
Morningxxx/rATC             stars=0    license=MIT      last=june        release=v0.1.0（README）
nkanf-dev/mihomot           stars=6    license=MIT      last=may
cublae/mihomo-manifold      stars=0    license=GPL-3.0  last=last thursday
MetaCubeX/mihomo            stars=34k  license=MIT      last=february 2025（shields 聚合，疑似陈旧）release=v1.19.30
```

### 8.3 上游文档/源码引用

- ShellCrash：`README.md`、`LICENSE.txt`（GPL-3.0）；源码（`ShellCrash.tar.gz` 内，version=`1.9.5beta3`）：`init.sh`（第 20/22/41-58/98-99/162/167 行）、`start.sh`（第 38/112 行）、`starts/core_config.sh`（第 100 行）、`starts/clash_config_check.sh`（全文）、`starts/core_exchange.sh`、`starts/shellcrash.service`、`starts/shellcrash.openrc`、`starts/shellcrash.procd`、`menus/2_settings.sh`（第 144-185 行备份/还原）、`starts/fw_nftables.sh`、`starts/fw_iptables.sh`。
- metacubexd：`README.md`（三种部署形态、agent profile manager、`mihomo -t` 校验、kernel logs SSE、`CONTROL_TOKEN`/`CLASH_SECRET`、`MIHOMO_BIN`）、`packages/agent/MANUAL.md`。
- Sub-Store：`README.md`、`LICENSE`（AGPL-3.0）。
- sub-store-convert：`README.md`（master 分支；monorepo core/app/cli；subconverter 风格 `/sub`）。
- subconverter：`README.md`（`/sub` 接口、多格式互转）。
- OpenClash：`README.md`（依赖 `kmod-tun`、`iptables-mod-tproxy`、`kmod-nft-tproxy`；LuCI）。
- PassWall / PassWall2：GitHub API 元数据（README 未抓取，能力列标 `[未验证]`）。
- clash-for-linux-install：`README.md`（`clashctl` 功能、systemd/OpenRC、集成 subconverter）。
- clash-verge-rev / clash-party：GitHub API 元数据（桌面 GUI，未抓取功能文档）。
- mihari：`README.md`、`docs/architecture.md`（控制面、原子提交点、补偿与 degraded、revision、日志脱敏、目录权限）、`docs/commands.md`（完整命令表、无 config 版本命令）。
- flclash-tui：`README.md`、`CLI_LINUX.md`（进程模型、revisioned transactions、原子写+失败回滚、`config validate/backup/restore`、`doctor`、TUN lease、History、多前端）。
- WangZhongDian/mihomo-tui：`README.md`（headless Linux 定位、订阅池保留 last working、内核多版本、systemd、TUN、`tun_diagnose`/`tun_debug`、`grant_operator`）。
- Proxy-RS：`README.md`（service 后端、可靠更新恢复原核心、结构化配置备份/校验失败恢复、四种代理模式、IPC 协议 v2 + 令牌、`headless`）。
- clashtui：`README.md`、`Cargo.toml`（MIT OR Apache-2.0，package version 0.3.2）。
- vpnkit：`README.md`（非 root、无 daemon、多源订阅、update、systemd-user、flock、随机端口、0600、out of scope: TUN）。
- potoo0/mihomo-tui：`README.md`（"It does not manage any actual configuration files"）、`Cargo.toml`（edition 2024，version 0.4.5）。
- rATC：`README.md`（Rust TUI + xray-core）。

### 8.4 搜索关键词记录

```text
api.github.com/search/repositories?q=mihomo+tui&sort=stars            → 51 items（top15 已核实）
api.github.com/search/repositories?q=clash+manager+rust&sort=stars    → 5 items（全部核实）
api.github.com/search/repositories?q=openwrt-passwall&sort=stars      → 186 items（top8 核实）
api.github.com/search/repositories?q=sub-store-convert&sort=stars     → 12 items（top8 核实）
```

---

## 9. 未验证假设与开放问题

### 9.1 未验证（本次无法确认，不得当作已否定）

1. **OpenClash 是否有配置版本化/回滚**：本次只抓取到 `README.md` 与仓库元数据（`[API]`），未做源码审查。能力列标 `[未验证]`。**假设（未验证）**：OpenClash 有配置备份机制（其 LuCI 页面通常提供"配置文件"页），但不应在无证据时写入。
2. **PassWall / PassWall2 的全部能力列**：仅拿到元数据。全部标 `[未验证]`。
3. **`flclash doctor` 的具体检测项**：只确认命令存在（`CLI_LINUX.md` 第 143 行），**检测范围未验证**（是否检测 LXC/CAP_NET_ADMIN/nftables 未知）。
4. **`verge-tui` 的全部能力**：仅元数据（8 star，early）。全部标 `[未验证]`。
5. **`mihari` 的 LXC 支持**：README/architecture 未提 LXC；TUN 检测是"冲突拒绝"而非能力分层。**标记为"未发现"，而非"明确不支持"。**
6. **`nkanf-dev/mihomot` 的语言与能力**：语言为 `[推测]`（Rust，依据仓库描述"built with Rust"），release 与功能未取。
7. **`MetaCubeX/mihomo` 最后提交时间**：shields.io 返回 `february 2025`，与"内核持续维护"的常识冲突，**疑似 shields 缓存/聚合异常**，未用 API 复核（额度耗尽）。**标记 `[未验证]`**，不应在其它文档中引用该日期。
8. **各项目"最近 release tag 与日期"的精确值**：仅 `MetaCubeX/mihomo` v1.19.30、`vernesong/OpenClash` v0.47.156 为 shields release badge；其余为 tag 名，**tag 日期未验证**。
9. **`flclash-tui` 的 `config backup/restore` 是否保留多份**：CLI 文档只列命令名，语义（单份/多份）未验证。
10. **metacubexd agent 是否保留 profile 的历史版本**：README 只描述 profile 的 CRUD 与激活校验，未提及历史；**标 `[未验证]`**（Matrix 中按"无版本历史"记录，但这是基于"未发现"）。
11. **`mihari` 的 `mihomo -t` 等价校验是否也在 Web/TUI 路径生效**：架构文档描述了 `ValidateConfig` 与 reload 补偿，但未逐 Adapter 验证。
12. **各项目在 PVE LXC（unprivileged）下的实际可用性**：本次**未做任何容器实测**（Docker Hub 不可达、不执行第三方安装脚本）。Matrix 的 LXC 列基于文档与源码，非实测。

### 9.2 开放问题（建议由后续调研或 ADR 回答）

1. **许可策略**：默认转换器选 MIT 的 `sub-store-convert` 还是 AGPL-3.0 的 Sub-Store？集成 AGPL 组件的边界（进程隔离 vs 代码链接）需要法律与架构双重结论 → 交给 `docs/research/13-licenses.md` / ADR。
2. **配置版本库的保留策略**：保留多少个版本？是否对订阅生成的大 YAML 做去重（内容寻址）？是否需要在 SQLite 存元数据 + 文件系统存内容的两层结构？
3. **Doctor 的检测项清单与判定语义**：`Unavailable` 与 `Misconfigured` 的边界如何定义（例如 `/dev/net/tun` 存在但无 `CAP_NET_ADMIN` 时应报哪个）？
4. **是否提供"自动回滚"还是"建议回滚"**：health check 失败后自动回滚 vs 保留现场等待人工确认——两种策略的安全性与可观测性权衡。
5. **多实例的边界**：MVP 明确 defer，但 per-instance 锁与配置目录布局需要现在就设计成可扩展的。
6. **是否需要持续跟踪 mihari / metacubexd 的 release**：建议在 `docs/research/open-questions.md` 中增加"竞品持续观察"条目。
7. **Rust 是否构成优势**：本次证据（clashtui/Proxy-RS/potoo0/rATC/verge-tui 均为 Rust）表明 "Rust 重写" **不构成差异化**；若产品叙事依赖此点，需要修正。
