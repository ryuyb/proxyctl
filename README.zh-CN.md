<div align="center">

# proxyctl

**面向 Linux 的 Mihomo 代理内核控制面。**

生命周期、可回滚的配置版本、订阅管理、运行能力探测 —— 外加 Web 界面与终端界面。

[**English**](README.md) · [**中文**](README.zh-CN.md)

[安装](#安装) · [快速开始](#快速开始) · [命令行](#命令行) · [Web 界面](#web-界面) · [工作原理](#工作原理)

</div>

---

## 这是什么

Mihomo 是一个出色的代理内核，控制 API 也相当完备。它缺的是**运维**：配置的不可变历史、从坏配置回退的路、不会毁掉现有可用配置的订阅调度，以及对「这个容器到底能做什么」的诚实回答。

`proxyctl` 就是这一层。它是**一个二进制、两种角色** —— 客户端与守护端 —— 并且拒绝对运行环境做任何猜测。

```text
                     ┌─────────────────────────────┐
   浏览器 ───────────►│  proxy-agent                │
                     │  ├── /          自研管理界面 │
                     │  ├── /ui        仪表盘       │
                     │  ├── /clash-api 内核数据通道 │
                     │  └── /api/v1    REST        │
                     └──────────┬──────────────────┘
                                │ unix socket
                     ┌──────────▼──────────────────┐
   客户端 ───────────►│  proxyctl（同一个二进制）    │
                     └─────────────────────────────┘
                                │
                     ┌──────────▼──────────────────┐
                     │  mihomo（代理内核本身）      │
                     └─────────────────────────────┘
```

它**不是** Mihomo 的替代品、不是完整的 Sub-Store、也不是仪表盘。凡是上游已经做好的，这里直接复用而非重写 —— 仪表盘就是上游的，以内嵌形态提供。

### 真正有价值的部分

| | |
|---|---|
| **配置版本** | 每个配置都是带校验和的不可变版本。`list`、`validate`、`activate`、`rollback` —— 且**绝不原地修改**生效中的配置。 |
| **失败即安全** | 订阅更新、转换、校验、重载中任何一步失败，都会保留上一份可用配置。这是整套设计围绕的不变量。 |
| **诚实的能力状态** | TUN、nftables、策略路由以**五值**之一报告 —— `Supported`、`Unsupported`、`Unavailable`、`Misconfigured`、`Unknown` —— 并附上探测方式与实际观察到的结果。绝不简化成布尔值。 |
| **两个界面** | 自研管理界面负责生命周期与配置；上游仪表盘负责代理组、延迟与实时流量。 |
| **单二进制** | `proxyctl` 既是客户端也是守护端。没有第二个要装的东西，也不存在两者版本不一致。 |

---

## 安装

### 一键安装

```bash
curl -fsSL https://raw.githubusercontent.com/ryuyb/proxyctl/main/scripts/install.sh | sudo bash
```

它会安装二进制、systemd unit、配置文件，以及服务运行所用的非特权用户 `proxy-agent`。它只 **enable** 而不 start —— 在别人的服务器上擅自拉起一个代理内核，不是安装脚本该做的决定。

<details>
<summary>脚本做了什么，以及刻意不做什么</summary>

**安装到 `/usr` 之外的 prefix：**

```bash
curl -fsSL <url> | sudo bash -s -- --prefix /usr/local
```

注意 systemd 只在 `/etc/systemd/system` 与 `/usr/lib/systemd/system` 下查找 unit，因此非默认 prefix 需要自己建软链。脚本在 `systemctl enable` 失败时会说明这一点。

**指定版本，或使用镜像：**

```bash
./scripts/install.sh --version 0.1.0
PROXYCTL_BASE_URL=https://mirror.example/proxyctl ./scripts/install.sh
./scripts/install.sh --dry-run      # 只打印将要做什么
```

**它不安装 Mihomo 内核。** 这是刻意的决定：

* 内核是独立程序、独立许可，由安装脚本去抓取会把「对应源码」义务转移到本脚本上；
* 内核由 agent 做校验和验证，选择哪个版本属于运维的判断；
* **没有内核 agent 照样能用** —— `proxyctl doctor` 与 Web 界面都能起来，「还没装内核」是一个合法状态，而非装坏了。

用 `proxyctl mihomo update <版本号>` 安装内核，例如
`proxyctl mihomo update v1.19.30`。

**关于校验和。** `.sha256` 与产物来自同一处，因此它**不是独立证据** —— 它能发现下载截断或产物名不匹配，但发现不了被替换。脚本在校验时会说明这一点。
</details>

### 从源码构建

```bash
git clone https://github.com/ryuyb/proxyctl
cd proxyctl
cargo build --release -p proxyctl     # 产物在 target/release/proxyctl
sudo install -m 0755 target/release/proxyctl /usr/bin/proxyctl
sudo install -d -o proxy-agent -g proxy-agent -m 0755 /usr/lib/proxy-agent
sudo install -m 0644 packaging/systemd/proxy-agent.service /usr/lib/systemd/system/
```

或直接用仓库里的 `scripts/install.sh`，它会把上面这些连同其余步骤都做完。

### 环境要求

Linux + systemd，架构为 `x86_64` 或 `aarch64`。实测过的是 Debian/Ubuntu 与 PVE LXC；其他发行版应当可用，但**未经验证**。

预编译二进制无运行时依赖。从源码构建需要 Rust 1.85 或更高。

---

## 快速开始

```bash
# 1. 安装内核。版本号是必填的 —— 没有 "latest" 简写，
#    因为 agent 会校验产物的 checksum，且选择哪个版本是一个决定，而非默认值。
#    这条命令会按需把 agent 拉起来。
proxyctl mihomo update v1.19.30

# 2. 存一份配置并激活它。内核需要有东西可加载，这一步会创建第一个版本。
proxyctl config add /etc/proxy-agent/mihomo.yaml

# 3. 看看这个环境实际能做什么
proxyctl doctor

# 4. 启动内核
proxyctl start
```

`config add` 会用与 `config validate` 相同的三层验证检查文档，然后把它存成不可变版本并把 active 指针指向它。它**不会** reload，所以在内核还不存在时也能用 —— 这是首次 `start` 能有东西可加载的唯一途径。要切换**正在运行**的配置，用 `config activate`，它会 reload，并在内核拒绝时回滚。

不需要 `sudo`，也不需要加入任何组：本机任何用户都能直接使用 agent。见[谁能用](#谁能用)。

### 不需要启动

**没有"启动 agent"这一步。** socket unit 已 enable，systemd 开机就创建 socket，并在第一次有东西连上来时拉起服务。直接跑命令就行：

```bash
proxyctl status        # 按需拉起 agent
```

这就是为什么所有命令都不需要 `sudo`。而直接启动服务**是**需要的 —— polkit 把 `manage-units` 当作 `auth_admin`，SSH 会话没有可以弹提示的地方，所以 `systemctl start` 会报 "Access denied … requires interactive authentication"。socket unit 的存在就是为了让你不必碰到这件事。

它还让 agent 能自愈：崩溃时 systemd 会重启它；socket 文件由 socket unit 而非服务持有，所以服务停了 socket 也还在。

**不要运行 `sudo proxyctl agent run`。** 它看起来能跑 —— socket 建出来了，agent 也打印了监听地址 —— 但结果比不启动更糟：

**不要运行 `sudo proxyctl agent run`。** 它看起来能跑 —— socket 建出来了，agent 也打印了监听地址 —— 但结果比不启动更糟：

* 进程挂在你的终端里，`Ctrl-C` 之后会留下一个没有监听者的 socket 文件；
* `systemctl` 看不到它，管不了它，也重启不了它；
* 状态目录和配置目录会落在你当时所在的目录，而不是打包好的位置。

unit 存在的理由就是这个：它指定了用户、用正确的权限创建运行目录、并且只授予 `CAP_NET_ADMIN`。

### 谁能用

agent socket 的权限是 `0666`：**本机任何用户都能连上它**，而且 agent 自己做调用方认证，不把文件权限当作边界。这是刻意的 —— 目标是客户端开箱可用，不需要把谁加进某个专用组。

实际含义：

* 单用户服务器、或者容器里，用起来就真的这么简单：装完、起服务、直接用自己的账号跑 `proxyctl`；
* 如果机器上有多个互不信任的本地用户，那么他们当中任何一个都能通过这个 socket 管理内核。如果你在意这点，就收紧它 —— 要么改窄 socket 权限，要么在 `config.toml` 里配对端校验：

```toml
[agent]
socket_allowed_uid = 1000   # 只有这个 uid 能连
# socket_allowed_gid = 1000 # 或者按组，或者两个都配
```

内核自己的 socket **并没有被保护**，这点在你依赖它之前值得知道：Mihomo 给自己 controller socket 硬编码了 `chmod 0666`，且在 unix socket 上**不校验** secret，所以机器上任何东西都能访问它：

```bash
curl -X PUT --unix-socket /run/proxy-agent/mihomo.sock -d '{}' http://localhost/configs
```

这会替换内核正在运行的配置。Agent 无法收紧它 —— 模式是上游自己设的。在 agent socket 放开之前，`0750` 的运行目录挡住了这条路；现在目录可穿越，就没有东西挡了。单用户主机或容器里这不是运维需要关心的差别，所以这里是接受该风险的。如果你在意，把 `external-controller-unix` 指到一个自己独占的 `0750` 目录里，这样内核的 socket 不可达而 agent 的仍然开放。

要从另一台机器访问 Web 界面，在配置里设置监听：

```toml
# /etc/proxy-agent/config.toml
[api]
bind = "0.0.0.0:9090"
```

**TCP 上必须有 token，且没有 loopback 豁免** —— 见[安全](#安全)。请先签发 token，否则 agent 会拒绝启动。

---

## 命令行

```bash
proxyctl status                 # 内核当前状态
proxyctl doctor                 # 运行环境与能力
proxyctl start | stop | restart | reload

proxyctl mihomo update v1.19.30 # 拉取、校验并安装内核发行版
proxyctl mihomo version         # 当前已安装的版本

proxyctl config list            # 全部配置版本
proxyctl config add FILE        # 存为版本并激活
proxyctl config validate FILE   # 预检、语法、语义
proxyctl config activate ID     # 切换正在运行的内核，失败可回滚
proxyctl config rollback ID     # 回退到曾经可用的版本

proxyctl subscription list
proxyctl subscription add NAME URL   # 登记;此时不会拉取
proxyctl subscription update NAME    # 拉取、转换并激活

proxyctl connections            # 实时连接（管理员可见进程信息）
proxyctl logs -f                # 跟随内核日志
proxyctl jobs                   # 近期任务
proxyctl audit                  # 审计记录
proxyctl tui                    # 终端界面
```

所有命令都支持 `--json` 以便脚本处理，以及 `--socket PATH` / `--token TOKEN` 用于连接其他 agent。

### 退出码

这是**契约**，因为脚本会依赖它：

| 码 | 含义 |
|---|---|
| `0` | 成功 |
| `1` | 失败 |
| `2` | 用法错误 |
| `3` | 未找到 |
| `4` | 与当前状态冲突 |
| `5` | 无权限 |
| `6` | 依赖不可达（内核、订阅源） |
| `7` | 尚未实现 |

注意：`config validate` 对**被拒绝**的配置会返回非零，尽管请求本身是 `200`。**`200` 也可能是坏消息。**

---

## Web 界面

两个界面，由 agent 在同一 origin 提供。

**自研管理界面**（`/`）覆盖本项目自己负责的部分：生命周期、配置版本与回滚、订阅、能力、医生、近期任务、审计记录、实时连接。它需要会话，而会话需要 token：

```bash
proxyctl token issue --principal admin
```

**仪表盘**（`/ui`）是 [metacubexd](https://github.com/MetaCubeX/metacubexd) —— 上游自己的项目，以构建产物形态内嵌。它负责代理组切换、逐节点延迟测试、流量图表、规则查看 —— 这些是本项目**刻意没有重建**的部分。

它的数据来自 `/clash-api`，一个到内核的同源反代。正是因为有了它，浏览器才永远拿不到内核 secret，内核的控制口也才能待在别的进程碰不到的 unix socket 上。

两者都内嵌在二进制里。没有它们的 checkout 照样能构建，只是会提供一个说明如何获取的占位页。

---

## 配置

只有一个文件，且完全可以省略 —— 空文件是合法的，会得到全部默认值。

```text
位置   /etc/proxy-agent/config.toml
权限   必须 0600，强制执行。否则加载器拒绝启动：这个文件可能存有内核 secret。
参考   /usr/share/doc/proxy-agent/config.toml.example
```

取值优先级从高到低：**命令行参数** → **环境变量**（`PROXYCTL_*`）→ **本文件** → **内置默认值**。想看实际生效的值以及每个值来自哪里：

```bash
proxyctl agent run --print-config
```

**所有路径都可配置**，没有任何地方硬编码打包时的默认值。三个目录来自 `[paths]`，socket 来自 `[agent] socket`。

---

## 订阅

订阅先登记、后拉取，两步分开是刻意的：登记只记录"从哪里拉"，所以即使提供商或转换器当时不可达也能成功。

```bash
proxyctl subscription add airport https://provider.example/sub?token=...
proxyctl subscription update airport      # 拉取、转换、校验、激活
```

按名字添加是幂等的 —— 名字**就是**标识符 —— 所以对已存在的名字再 add 是替换拉取地址，而不是建第二个。

**URL 永远不会被回显**，`add` 和 `list` 都不会。订阅 URL 本质上就是凭证：它通常内嵌 token，拿到它就等于拿到订阅。两条命令都只报名字，而后续命令寻址需要的也正是名字。

`--schedule SECONDS` 会自动刷新；不加则只在运行 `update` 时拉取。

---

## 安全

简版：**认证是唯一的授权环节** —— 没有角色，通过认证就能做任何事。本地 socket 信任任何能连上它的人，TCP 必须带 token，浏览器拿到的是 cookie 而非凭证。

* **Unix socket（agent）。** 权限 `0666`，位于一个可穿越的目录。agent 自己做调用方认证；能连上 socket 就等于拥有操作员身份。对端凭证（`SO_PEERCRED`）是**额外可选**的检查，默认关闭 —— 因为 LXC 的 uid 映射会让正确的对端看起来不对。这是[谁能用](#谁能用)里说明的刻意取舍。
* **Unix socket（内核）。** `0666`，因为 Mihomo 硬编码该值，且在 unix socket 上不校验 secret。因此它**不被保护**：本机任何用户都能访问它并替换运行中的配置。这是放开 agent socket 的代价，这里直说而不是暗示它安全。
* **TCP。** 必须有 token，且**没有 loopback 豁免** —— 在同样运行着不可信软件的机器上，loopback 不是信任边界。配置监听时至少要存在一个 token，否则 agent 拒绝启动。
* **浏览器**用 token 换取 `HttpOnly; SameSite=Strict` 的 cookie。**刻意没有 CSRF token**：那要求它可被脚本读取，而这正是 cookie 方案要避免的问题。
* **内核 secret 永远不会到达页面。** 由反代注入。上游自己的 all-in-one server 是把控制 token 放进浏览器的；这里不这么做。
* **内核控制口**默认绝不绑定到公网接口。
* **没有角色。** 任何通过认证的调用方都能做所有事。唯一会拒绝的是 `/clash-api` 上的 **HTTP 方法**（只放行 `GET`/`HEAD`/`OPTIONS`），因为它防的是页面替换内核运行配置 —— 那不是对「谁在问」的限制。

完整推理在 [`docs/adr/`](docs/adr/) 与 [`AGENTS.md`](AGENTS.md) 的安全章节。

---

## 工作原理

```text
crates/
├── domain/          纯规则 —— 无 I/O、无运行时、无数据库
├── application/     用例与 Port
├── infrastructure/  适配器：SQLite、mihomo、systemd、Clash API 反代
├── interfaces/      HTTP API 与两个内嵌界面
├── cli/             proxyctl：客户端命令、TUI、守护端角色
└── bootstrap/       组装根
```

六个 crate、一个二进制。依赖只向内指，且有架构测试强制 —— domain 完全不知道 Linux、HTTP 或 SQLite 的存在，这正是它能脱离这些被测试的原因。

**状态存放位置：**

```text
/etc/proxy-agent/config.toml        配置              (0600)
/var/lib/proxy-agent/configs/       不可变版本
/var/lib/proxy-agent/database.sqlite
/run/proxy-agent/agent.sock         agent        (0666，本机任何用户)
/run/proxy-agent/mihomo.sock        内核
```

**现阶段已有的文档** —— 记录推理过程、实测数据，以及**尚未解决的问题**（而不是含糊地假设掉）：

| | |
|---|---|
| [`docs/design/`](docs/design/) | 逐功能设计记录，含被实测推翻的假设 |
| [`docs/adr/`](docs/adr/) | 架构决策及其后果 |
| [`docs/research/`](docs/research/) | Phase 0：上游行为、许可、安全模型 |
| [`open-questions.md`](docs/research/open-questions.md) | 未决问题及其证据 —— 不是愿望清单 |

---

## 项目状态

**预发布，活跃开发中。** 已在真实内核上验证可用：内核生命周期、配置版本与回滚、订阅、医生、REST API、CLI、TUI、两个 Web 界面，以及带一键安装脚本的 systemd 打包。

尚未完成：

* `.deb` 包 —— 目前安装是脚本而非软件包；
* 通过 Sub-Store 的订阅转换（Port 已存在，当前接的是原生转换器）；
* 任何多节点能力。

已知未决项及其证据见 [`open-questions.md`](docs/research/open-questions.md)。测试与门禁：

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm --dir frontend/admin check     # 前端是另一套工具链
```

---

## 许可

agent 采用 **MIT 或 Apache-2.0 双许可**，任选其一 —— 见 [`LICENSE-MIT`](LICENSE-MIT)
与 [`LICENSE-APACHE`](LICENSE-APACHE)。

Mihomo 内核是 GPL-3.0，且是**独立程序**：本项目不分发它，只在运维要求时安装。
内嵌的仪表盘是 MIT，并带有上游自己的字体与图形署名要求 —— 见
[`docs/research/13-licenses.md`](docs/research/13-licenses.md)。
