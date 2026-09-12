# R10 — PVE LXC 能力矩阵与 doctor 探测

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：上游文档研究 + 容器实测（**非 PVE**）
> 关键结论一句话：**TUN 的成立条件是「可用且可打开的 `/dev/net/tun`」∩「`CAP_NET_ADMIN`」，二者缺一不可；透明代理在此之上还要加「可写 sysctl（`ip_forward` / `route_localnet`）」，因此 unprivileged LXC 完全可以跑 TUN（甚至比 Docker 更容易），而 PVE 的默认 seccomp/AppArmor/device-cgroup 三层过滤决定了这一组合不会自动成立，必须由 doctor 逐项探测而不是假设。**

---

## 1. 结论摘要（TL;DR）

| # | 结论 | 证据等级 |
|---|------|----------|
| 1 | **`LXC ≠ TUN 可用`**。TUN 需要同时满足两件事：进程有 `CAP_NET_ADMIN`，且 `/dev/net/tun` 存在、可 `open(O_RDWR)` 且能通过 `ioctl(TUNSETIFF)` 注册网卡。只满足其中一个都不够。 | `[实测-容器]` + `[上游文档]` |
| 2 | **`CAP_MKNOD` 单独存在并不等于能建 TUN。** 实测默认容器有 `CAP_MKNOD`、`mknod /dev/net/tun` 成功，但 `TUNSETIFF` 返回 `EPERM`——因为 device cgroup（cgroup v2 下由 `BPF_PROG_TYPE_CGROUP_DEVICE` 实现）拦住了设备访问。**「设备节点存在」≠「设备可用」**，这是 `Misconfigured` 状态最主要来源。 | `[实测-容器]` + `[上游文档]` |
| 3 | **unprivileged LXC 跑 TUN 是可行的，且是推荐做法。** 所需最小组合是：bind-mount `/dev/net/tun` 进容器 + `lxc.cgroup2.devices.allow: c 10:200 rwm` + 容器内进程持有 `CAP_NET_ADMIN`。实测把这两者组合起来（`--cap-add=NET_ADMIN --device /dev/net/tun`）后，`TUNSETIFF`、`ip tuntap add`、`ip link add` **全部成功**。 | `[实测-容器]` + `[上游文档]` |
| 4 | **透明代理（TProxy / redirect）只需要 `CAP_NET_ADMIN`，不需要 `SYS_ADMIN`。** 实测仅给 `NET_ADMIN` 即可成功创建 `nft ... tproxy to :7894`、`nft ... redirect to :7894`、`iptables -j TPROXY`、`iptables -j REDIRECT`、`ip rule add fwmark`、`ip route add ... table 100`。 | `[实测-容器]` |
| 5 | **TProxy 的隐藏门槛是 sysctl 可写，不是 netfilter 权限。** Docker 默认把 `/proc/sys` 挂成只读，即使 `--cap-add=NET_ADMIN --cap-add=SYS_ADMIN` 也写不动 `route_localnet`。PVE LXC 默认不这样挂载（`lxc.mount.entry` 可控），所以这是 **Docker 特有伪影，不应推广成「LXC 也不行」**；但它恰好说明 doctor 必须把「sysctl 可写」单列为一个独立探测项。 | `[实测-容器]` |
| 6 | **systemd 可用性与 PVE LXC 的 `nesting` 强相关。** PVE 文档明确指出 `nesting` 是 systemd 隔离服务所需要的；未开 `nesting` 时 `systemd-networkd` / 服务隔离可能异常。容器内 init 不是 systemd 时，`systemctl` 系列命令不可用，Mihomo 进程管理必须回退到自管子进程 + `Restart=always` 等价逻辑或外部 supervisor。 | `[上游文档]` + `[实测-容器]` |
| 7 | **降级必须是常态而非异常。** 「HTTP/SOCKS/Mixed 可用 + TUN 不可用 + TProxy 不可用」是完全合法的生产状态；Agent 不得因为 TUN 探测失败而拒绝启动或拒绝 reload。 | 架构要求（AGENTS.md） |

---

## 2. LXC/容器权限模型要点（PVE 官方文档）

### 2.1 `pct.conf` 的 `features` 选项

来源：[Proxmox VE 文档 `pct.conf(5)`](https://pve.proxmox.com/pve-docs/pct.conf.5.html)（版本 9.2.10）。

官方语法：

```text
features: [force_rw_sys=<1|0>] [,fuse=<1|0>] [,keyctl=<1|0>] [,mknod=<1|0>] [,mount=<fstype;fstype;...>] [,nesting=<1|0>]
```

逐项原文要点（引号内为文档原文语义的转述，关键处保留英文）：

| 子选项 | 默认 | 官方说明 | 与 Mihomo 的关系 |
|--------|------|----------|------------------|
| `nesting` | `0` | "Allow nesting. Best used with unprivileged containers with additional id mapping. Note that this will expose procfs and sysfs contents of the host to the guest. **This is also required by systemd to isolate services.**" | 影响容器内 systemd 是否能正常工作；opens procfs/sysfs 暴露面 |
| `keyctl` | `0` | "For unprivileged containers only: Allow the use of the `keyctl()` system call. This is required to use docker inside a container. ... Essentially, you can choose between running systemd-networkd or docker." | 与 Mihomo 无直接关系；但说明 unprivileged 下 seccomp 默认拦系统调用 |
| `fuse` | `0` | "Allow using *fuse* file systems in a container. Note that interactions between fuse and the freezer cgroup can potentially cause I/O deadlocks." | 与 Mihomo 无直接关系 |
| `mknod` | `0` | "Allow unprivileged containers to use `mknod()` to add certain device nodes. **This requires a kernel with seccomp trap to user space support (5.3 or newer). This is experimental.**" | **注意**：这是「允许容器自己造设备节点」，不是「允许访问该设备」。造出节点后仍需 device cgroup 放行 |
| `mount` | 空 | "Allow mounting file systems of specific types. ... Note that this can have negative effects on the container's security." | 与 Mihomo 无直接关系 |
| `force_rw_sys` | `0` | "Mount /sys in unprivileged containers as rw instead of mixed. This can break networking under newer (>= v245) systemd-network use." | 不建议为 Mihomo 打开 |

**结论**：`features` 里**没有任何一项**是「TUN 开关」。指望开 `nesting` 或 `mknod` 就得到 TUN 是错误假设。

### 2.2 设备透传的两条路径（容易混淆）

来源同上（`pct.conf(5)`）与 [Linux Container wiki](https://pve.proxmox.com/wiki/Linux_Container)。

**(a) PVE 抽象层 `dev[n]`**（推荐给普通用户）：

```text
dev[n]: [[path=]<Path>] [,deny-write=<1|0>] [,gid=<integer>] [,mode=<Octal access mode>] [,uid=<integer>]
```

原文："Device to pass through to the container"。有 `mode` / `uid` / `gid` / `deny-write` 子选项。这是 PVE 提供的、比手写 `lxc.*` 更安全的封装。

**(b) LXC 低层 `lxc.*`**。PVE 文档明确允许直接写低层配置：

> "It is also possible to add low-level LXC-style configuration directly, for example: `lxc.init_cmd: /sbin/my_own_init`. Those settings are directly passed to the LXC low-level tools."

### 2.3 cgroup v2 与 `lxc.cgroup2.devices.allow`

来源：[LXC `lxc.container.conf(5)` 上游源码文档](https://raw.githubusercontent.com/lxc/lxc/main/doc/lxc.container.conf.sgml.in)（`raw.githubusercontent.com` 直连可用）。

关键原文要点：

- 纯 cgroup v2 下必须用 `lxc.cgroup2.` 前缀：
  > "the `lxc.cgroup2.` key prefix must be used. The `lxc.cgroup.` key prefix, which was used for legacy and hybrid hierarchy configurations, **is no longer supported**."
- device controller 在 unified hierarchy 下的实现方式：
  > "In the unified cgroup hierarchy, the device controller is implemented via an eBPF program of type `BPF_PROG_TYPE_CGROUP_DEVICE` attached to a cgroup"
- 放行规则由 `lxc.cgroup2.devices.allow` / `lxc.cgroup2.devices.deny` 指定。
- allowlist/denylist 语义：`lxc.cgroup2.devices.deny = a` 表示默认封禁全部设备，之后必须用 allow 规则逐个放行；`lxc.cgroup2.devices.allow = a` 反之。
- 重要副作用：**"Specifying any of the aforementioned two rules will cause all previous rules to be cleared, i.e. the device list will be reset."**（写错顺序会把之前所有规则清空——doctor 的配置校验应检查这一点。）

cgroup 版本差异（[PVE Linux Container wiki](https://pve.proxmox.com/wiki/Linux_Container)）：

> "The current version of *cgroups* is *cgroupv2*. The v1 version of the cgroup subsystem was deprecated with the release of **Proxmox VE 7.0** and **removed entirely with Proxmox VE 9.0**."

→ 因此 PVE 8/9 上必须使用 `lxc.cgroup2.devices.allow`，`lxc.cgroup.devices.allow` **在 PVE 9 上无效**，在 PVE 8 上属于已废弃路径。

### 2.4 `lxc.mount.entry`

来源：LXC `lxc.container.conf(5)` 上游源码文档。

原文要点：

> "Specify a mount point corresponding to a line in the fstab format. Moreover lxc supports mount propagation, such as rshared or rprivate, and adds three additional mount options. `optional` don't fail if mount does not work. `create=dir` or `create=file` to create dir (or file) when the point will be mounted. `relative` source path is taken to be relative to the mounted container root."

→ `create=file` 正是让 LXC 在容器内自动创建 `/dev/net/tun` 这个挂载点文件的关键选项，从而**绕开对 `mknod` 的依赖**。

### 2.5 `lxc.cap.drop` / `lxc.cap.keep`

来源同上。原文要点：

> `lxc.cap.drop`： "Specify the capability to be dropped in the container. A single line defining several capabilities with a space separation is allowed. The format is the lower case of the capability definition without the `CAP_` prefix, eg. `CAP_SYS_MODULE` should be specified as `sys_module`."
>
> `lxc.cap.keep`： "Specify the capability to be kept in the container. **All other capabilities will be dropped.** ... A value of `none` alone can be used to drop all capabilities."

→ 若部署方用 `lxc.cap.drop` 收紧容器（安全加固常见做法），**很可能把 `net_admin` 一起丢掉**，这正是 doctor 必须实测 capability 而不是读取配置的原因。PVE 自身也会 drop 一批 capability（见 2.7）。

### 2.6 AppArmor / seccomp

来源：[PVE Linux Container wiki — Security Considerations](https://pve.proxmox.com/wiki/Linux_Container)。

原文要点：

- "LXC uses many security features like **AppArmor, CGroups and kernel namespaces**."
- "AppArmor profiles are used to restrict access to possibly dangerous actions. **Some system calls, i.e. mount, are prohibited from execution.**"
- 追踪方式：`dmesg | grep apparmor`
- 关闭方式（**文档明确不推荐**）：
  > "Although it is not recommended, AppArmor can be disabled for a container. ... `lxc.apparmor.profile = unconfined`"
  > "Please note that this is not recommended for production use."
- seccomp：文档提到 "some syscalls (user space requests to the Linux kernel) **are not allowed** within containers"，以及 LXC 层的 `lxc.seccomp.profile` / `lxc.no_new_privs`。

**对 Mihomo 的含义**：`mount` 被 AppArmor 禁止不影响 Mihomo（Mihomo 不 mount）；但 `unshare`、`keyctl`、部分 `ioctl` 可能被 seccomp 影响。实测容器中 `Seccomp: 2`（filter mode）且 `unshare -Urn` 被拒，说明 seccomp 确实在起作用。

### 2.7 privileged vs unprivileged

来源：[PVE Linux Container wiki](https://pve.proxmox.com/wiki/Linux_Container)。

- unprivileged（**PVE 创建新容器的默认值**）："Unprivileged containers use a new kernel feature called user namespaces. The root UID 0 inside the container is mapped to an unprivileged user outside the container."
- privileged："Security in containers is achieved by using mandatory access control *AppArmor* restrictions, *seccomp* filters and Linux kernel namespaces. The LXC team considers this kind of container as unsafe ... That's why privileged containers should only be used in trusted environments."
- `pct.conf(5)` 对 `unprivileged` 的定义："Makes the container run as unprivileged user. For creation, the default is **1**. For restore, the default is the value from the backup. **(Should not be modified manually.)**"

→ **不要建议用户手工改 `unprivileged`**；只能在创建时选择。

### 2.8 rootfs 与 `/dev` 的差异

- PVE 支持三类 mount point：storage backed、bind mount、device mount（[wiki](https://pve.proxmox.com/wiki/Linux_Container)）。
- bind mount 在 unprivileged 下 "you might run into permission problems caused by the user mapping and cannot use ACLs"。
- bind mount 源路径 "must not contain any symlinks"（安全限制）。
- **`/dev` 是 tmpfs，不是 rootfs 的一部分**，所以 `/dev/net/tun` 不会因为容器模板里有就自动存在——这是 `dev[n]` / `lxc.mount.entry` 存在的理由。

---

## 3. TUN / TProxy / nftables 的前置条件（逐项）

### 3.1 TUN（内核侧）

来源：[Linux kernel `Documentation/networking/tuntap.rst`](https://raw.githubusercontent.com/torvalds/linux/master/Documentation/networking/tuntap.rst)。

原文要点：

> "In order to use the driver a program has to **open `/dev/net/tun` and issue a corresponding `ioctl()`** to register a network device with the kernel."
>
> "Create device node: `mkdir /dev/net` (if it doesn't exist already); `mknod /dev/net/tun c 10 200`"
>
> "**There's no harm in allowing the device to be accessible by non-root users, since `CAP_NET_ADMIN` is required for creating network devices** or for connecting to network devices which aren't owned by the user in question."
>
> "Driver module autoloading: Make sure that 'Kernel module loader' - module auto-loading support is enabled in your kernel. **The kernel should load it on first access.**"
>
> "When the program closes the file descriptor, the network device and all corresponding routes will disappear."

**由此得出三个硬性事实**：

1. 设备号固定为 **char major 10, minor 200**（`c 10:200`），这是 `lxc.cgroup2.devices.allow` 里必须写的值。
2. `open()` 本身**不需要特权**；特权在 `TUNSETIFF` ioctl 那一步（`CAP_NET_ADMIN`）。
3. `tun` 模块按需自动加载，**通常不需要 `modprobe`，也不需要 `CAP_SYS_MODULE`**。但若宿主内核把 `tun` 编译为模块且 `/lib/modules` 在容器内不可见（实测容器即如此），仍能自动加载——因为模块加载发生在**宿主内核**上下文。（`[推测]`：宿主内核若 `CONFIG_TUN=n`，则无解。）

### 3.2 TUN（Mihomo 侧）

来源：[Mihomo 官方 TUN 文档](https://wiki.metacubex.one/config/inbound/tun/)（直连可用）。

| 配置项 | 官方说明 | 特权含义 |
|--------|----------|----------|
| `enable` | 启用 tun | — |
| `stack` | `system` / `gvisor` / `mixed`，默认 `gvisor` | `gvisor` 是**用户态协议栈**，理论上不依赖内核 netfilter；但**创建 TUN 网卡本身仍需要 `/dev/net/tun` + `CAP_NET_ADMIN`** |
| `device` | 指定 tun 网卡名 | — |
| `auto-route` | "自动设置全局路由，可以自动将全局流量路由进入 tun 网卡" | **需要写路由表 → `CAP_NET_ADMIN`** |
| `auto-redirect` | "**仅支持 Linux**，自动配置 iptables/nftables 以重定向 TCP 连接，需要 `auto-route` 已启用" | **需要 netfilter 写入权限 → `CAP_NET_ADMIN`** |
| `auto-detect-interface` | 自动选择出口网卡 | 读取为主 |
| `dns-hijack` | 劫持匹配连接进内部 dns 模块 | 依赖 auto-route / redirect |
| `strict-route` | Linux 下"让不支持的网络无法到达 / 将所有连接路由到 tun" | 路由 + netfilter |
| `gso` | "启用通用分段卸载，**仅支持 Linux**" | — |
| `route-address-set` / `route-exclude-address-set` | "**仅支持 Linux，且需要 nftables** 以及 `auto-route` 和 `auto-redirect` 已启用" | **需要 nftables 可用** |
| `include-uid` / `exclude-uid` 等 | "UID 规则仅在 Linux 下被支持,并且需要 `auto-route`" | 路由 + owner match |
| `include-mac-address` | "仅支持 Linux，且需要启用 `auto-route` 和 `auto-redirect`" | netfilter + 路由 |
| `iproute2-table-index` | 默认 `2022` | doctor 可用它检查路由表是否已建立 |
| `iproute2-rule-index` | 默认 `9000` | 同上 |

> ⚠️ **注意**：Mihomo TUN 文档中**没有**任何一句说明需要 `CAP_SYS_ADMIN`。实测也确认 `SYS_ADMIN` 对 netfilter 规则创建**不是必需的**。所以「TUN 需要 SYS_ADMIN」是**常见误解**，本任务明确排除。

### 3.3 Mihomo 官方 systemd 单元（权威 capability 集合）

来源：[Mihomo 官方「创建运行服务」文档](https://wiki.metacubex.one/startup/service/)。

官方给出的 `/etc/systemd/system/mihomo.service` 关键行：

```ini
[Service]
Type=simple
LimitNPROC=500
LimitNOFILE=1000000
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE CAP_SYS_TIME CAP_SYS_PTRACE CAP_DAC_READ_SEARCH CAP_DAC_OVERRIDE
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE CAP_SYS_TIME CAP_SYS_PTRACE CAP_DAC_READ_SEARCH CAP_DAC_OVERRIDE
Restart=always
ExecStartPre=/usr/bin/sleep 1s
ExecStart=/usr/local/bin/mihomo -d /etc/mihomo
ExecReload=/bin/kill -HUP $MAINPID
```

**这对 Agent 有直接价值**：

- 官方把 `AmbientCapabilities` 写出来，证明 Mihomo 在**非 root 运行**时也需要这些能力。容器内若 Agent 以非 root 运行 Mihomo，必须确保 ambient capabilities 能被设置（容器内 `CAP_SETPCAP` + 不丢 ambient raise 权限）。
- `CAP_SYS_PTRACE`、`CAP_DAC_READ_SEARCH`、`CAP_SYS_TIME` 是官方配置里出现的，但**不是 TUN/TProxy 的必需项**（`SYS_TIME` 供 NTP、`PTRACE`/`DAC_READ_SEARCH` 供某些探测）。Agent 若用 systemd 托管，应**复制官方集合**而不是自己发明一个更小的集合，否则会出现「功能莫名不可用」。

### 3.4 最小 capability 集合（由实测 + 官方文档归纳）

| 目标功能 | 必需 capability | 必需设备/文件 | 必需 sysctl | 必需内核特性 |
|----------|-----------------|---------------|-------------|--------------|
| HTTP/SOCKS/Mixed 代理入口 | 无（>`1024` 端口）；`CAP_NET_BIND_SERVICE`（`<1024` 端口） | 无 | 无 | 无 |
| TUN 网卡建立 | `CAP_NET_ADMIN` | `/dev/net/tun`（c 10:200）可 `open(O_RDWR)` 且 device cgroup 放行 | 无（仅建卡时） | `CONFIG_TUN` |
| `auto-route` | `CAP_NET_ADMIN` | — | 无（写路由表即可） | 路由子系统 |
| TProxy（nftables 或 iptables） | `CAP_NET_ADMIN` | — | `net.ipv4.ip_forward=1`、`net.ipv4.conf.<iface>.route_localnet=1`（见下） | `nf_tproxy` / `xt_TPROXY` |
| redirect（nat） | `CAP_NET_ADMIN` | — | 通常无需 forward（本机重定向） | `nf_nat` |
| 内建 DNS 劫持 | 随 TUN/TProxy 而定 | — | — | — |
| NTP | `CAP_SYS_TIME` | — | — | — |
| 内核模块加载 | 容器内**不需要**（宿主加载） | — | — | — |

> **`route_localnet` 说明**：TProxy 场景下，本机发起的、目标是 `127.0.0.0/8` 的流量默认被内核丢弃，需要 `route_localnet=1`。这是 Linux 网络常识，本次实测只能证明「该 sysctl 在容器内可读、在 Docker 下不可写」，**无法证明 PVE LXC 下一定可写**。标记 `[未验证]`（见第 9 节）。

### 3.5 `CAP_NET_ADMIN` 官方语义

来源：[`capabilities(7)`](https://man7.org/linux/man-pages/man7/capabilities.7.html)。

> **CAP_NET_ADMIN** — Perform various network-related operations:
> - interface configuration;
> - administration of IP firewall, masquerading, and accounting;
> - **modify routing tables**;
> - **bind to any address for transparent proxying**;
> - set type-of-service (TOS);
> ...

→ 一句话覆盖了 TUN（interface configuration）、TProxy（transparent proxying）、nftables/iptables（IP firewall）、auto-route（routing tables）。**`CAP_NET_ADMIN` 是网络数据面的总钥匙**，这也是为什么最小权限组合的核心就是它。

> **CAP_NET_RAW** — "Use RAW and PACKET sockets; bind to any address for transparent proxying."
>
> **CAP_MKNOD** — "Create special files using `mknod(2)`."

---

## 4. 容器实测记录（OrbStack，明确局限）

### 4.1 环境与局限性声明

| 项目 | 值 |
|------|-----|
| 宿主 | macOS (Darwin, arm64) |
| 容器运行时 | Docker via OrbStack（`docker info` 确认 daemon 可用） |
| 被测镜像 | `postgres:16-alpine`（Alpine 3.23.3，aarch64）再 `apk add iproute2 iptables nftables libcap python3` |
| 内核 | `7.0.14-orbstack-00380-ga7e0a2dc9535`（OrbStack Linux VM 内核，**不是 PVE 内核**） |
| 证据分级 | `[实测-容器]` |

> ⚠️ **证据强度声明（重要）**
>
> **OrbStack 的 Linux VM/容器 ≠ PVE LXC。** 两者在以下方面不可类比：
> - OrbStack 是 **Docker**（runc + 自有 VM），默认 `--cap-drop=ALL` 后按白名单加回，并使用 seccomp 默认 profile；PVE 是 **LXC**，capability/seccomp/AppArmor/device-cgroup 的默认策略完全不同。
> - **`/proc/sys` 只读是 Docker/OrbStack 的挂载决定**（实测 mountinfo 显示 `/proc/sys ro`），**不是 LXC 行为**。因此「容器内 sysctl 写不动」这一条**不能推广到 PVE LXC**。
> - OrbStack 内核的 `tun` 模块、netfilter 模块集合与 PVE 宿主内核（Debian-based）不同。
> - 本节所有结果仅用于**验证「capability ↔ 功能」的因果关系**，不用于断言 PVE 上的具体表现。
>
> 2026-09-12 记录：Docker Hub 与国内镜像源在该环境下不可达，无法拉取 `debian:12` 等新镜像；本节证据来自**本地已有 Alpine 基础镜像 + `apk` 在线安装工具链**，工具链安装正常。

### 4.2 四种模式实测结果对照

| 探测项 | ① 默认（无 NET_ADMIN） | ② `--privileged` | ③ `--cap-add=NET_ADMIN` | ④ `--cap-add=NET_ADMIN --device /dev/net/tun` |
|--------|----------------------|------------------|------------------------|-----------------------------------------------|
| `CapEff` | `0xa80425fb` | `0x1ffffffffff` | `0xa80435fb` | `0xa80435fb` |
| `CAP_NET_ADMIN` | ✗ | ✓ | ✓ | ✓ |
| `CAP_MKNOD` | ✓ | ✓ | ✓ | ✓ |
| `CAP_SYS_ADMIN` | ✗ | ✓ | ✗ | ✗ |
| `/dev/net/tun` 存在 | ✗ | ✓ (`crw-rw-rw-`) | ✗ | ✓ (`crw-rw-rw-`) |
| `open(O_RDWR)` | ✗ `ENOENT(2)` | ✓ | ✗ `ENOENT(2)` | ✓ |
| `mknod` 成功 | ✓（但设备仍不可用） | n/a（已存在） | — | — |
| **`TUNSETIFF`** | **✗ `EPERM(1)`** | ✓ | — | **✓ `r10c1`** |
| `ip tuntap add` | ✗ `EPERM` | ✓ | — | ✓ |
| `ip link add dummy` | ✗ `EPERM` | ✓ | ✓ | ✓ |
| `nft list ruleset` | ✗ `EPERM` | ✓ | ✓（空 ruleset） | ✓ |
| `nft add tproxy 规则` | ✗ | ✓ | **✓** | **✓** |
| `nft add redirect 规则` | ✗ | ✓ | ✓ | ✓ |
| `iptables -j REDIRECT` | ✗ `Permission denied` | ✓ | ✓ | ✓ |
| `iptables -j TPROXY` | ✗ | ✓ | **✓** | **✓** |
| `ip rule add fwmark 1 lookup 100` | ✗ `EPERM` | ✓ | ✓ | ✓ |
| `ip route add ... table 100` | ✗ | ✓ | ✓ | ✓ |
| 写 `/proc/sys/net/ipv4/ip_forward` | ✗ 只读 | ✗ 只读 | ✗ 只读 | ✗ 只读 |
| `unshare -Urn` | ✗ | ✗ | ✗ | ✗ |
| `/proc/1/comm` | `sh` | `sh` | `sh` | `sh` |
| `systemctl` | 不存在 | 不存在 | 不存在 | 不存在 |

### 4.3 关键原始输出（摘录）

**① 默认模式 —— 关键失败点**

```text
--- capabilities (/proc/self/status)
CapEff:	00000000a80425fb
Seccomp:	2
Seccomp_filters:	1

--- /dev/net/tun presence
ls: /dev/net/tun: No such file or directory

--- mknod /dev/net/tun test
MKNOD-OK
crw-r--r--    1 root     root       10, 200 /dev/net/tun

--- open after mknod attempt
OPEN-RDWR-OK                      <-- open() 成功！

--- TUNSETIFF probe
TUNSETIFF-FAIL 1 Operation not permitted

--- nft list ruleset
Operation not permitted (you must be root)

--- iptables -t nat -L -n
iptables v1.8.11 (nf_tables): Could not fetch rule set generation id: Permission denied (you must be root)

--- unshare -Urn
unshare: unshare failed: Operation not permitted

--- write test sysctl
can't create /proc/sys/net/ipv4/ip_forward: Read-only file system

--- /proc/sys mount opts
374 514 0:100 /sys /proc/sys ro,nosuid,nodev,noexec,relatime - proc proc rw
```

> **这是本任务最有价值的单条记录**：`CAP_MKNOD` 在、`mknod` 成功、**`open(O_RDWR)` 也成功**，但 `TUNSETIFF` 返回 `EPERM`。这精确证明了 TUN 的门槛在 **ioctl 的 `CAP_NET_ADMIN` 检查**，而不只是「文件存不存在」。→ **doctor 绝不能把「`/dev/net/tun` 存在」当成 TUN 可用。**

**③/④ `CAP_NET_ADMIN` 足以做透明代理**

```text
=== nft add tproxy rule test
TPROXY-RULE-OK
table inet r10t {
	chain pr {
		type filter hook prerouting priority mangle; policy accept;
		meta l4proto tcp tproxy to :7894
	}
}

=== iptables TPROXY rule (mangle)
IPT-TPROXY-OK
-N R10M
-A R10M -p tcp -j TPROXY --on-port 7894 --on-ip 0.0.0.0 --tproxy-mark 0x1/0xffffffff

=== iptables REDIRECT rule (nat)
IPT-REDIRECT-OK
-N R10
-A R10 -p tcp -m tcp --dport 80 -j REDIRECT --to-ports 7894

=== nft ip rule fwmark (auto-route style)
IPRULE-OK
32765:	from all fwmark 0x1 lookup 100

=== ip route add table 100
RT-OK
local default dev lo scope host
```

→ `SYS_ADMIN` 在模式 ③ 中是 **False**，但以上全部成功。**结论：TProxy/redirect/auto-route 不需要 `SYS_ADMIN`。**

**④ `NET_ADMIN` + tun 设备 —— TUN 成立**

```text
=== /dev/net/tun
crw-rw-rw-    1 root     root       10, 200
=== open RDWR
OPEN-RDWR-OK
=== TUNSETIFF
TUNSETIFF-OK r10c1
=== ip tuntap add
IPTUNTAP-ADD-OK
4: r10probe: <POINTOPOINT,MULTICAST,NOARP> mtu 1500 qdisc noop state DOWN
    link/none
(deleted)
```

→ **两个条件同时满足时 TUN 完全可用。** 这直接支撑「unprivileged LXC + bind mount `/dev/net/tun` + 保留 `CAP_NET_ADMIN`」的组合建议。

**sysctl 只读是 Docker 特有**

```text
### --cap-add=NET_ADMIN --cap-add=SYS_ADMIN
=== write route_localnet all
can't create /proc/sys/net/ipv4/conf/all/route_localnet: Read-only file system
```

→ 即使叠加 `SYS_ADMIN` 仍然只读。原因在 mountinfo 里：`/proc/sys` 被挂成 `ro`。**这是挂载策略问题，不是 capability 问题。** 反过来说：**在 PVE LXC 下，只要 `/proc/sys` 可写（默认通常是 rw），`CAP_NET_ADMIN` 就足够写这些 sysctl。** 该推论标记 `[推测]`（依赖 PVE 默认挂载行为，未实测）。

### 4.4 容器实测**无法**验证的项目

- 容器内无 systemd（`/proc/1/comm = sh`，`systemctl` 不存在）→ 无法验证 PVE LXC 中 systemd 托管 Mihomo 的行为。
- 内核模块视图不可见（`/proc/modules` 为空、`/lib/modules` 不存在）→ 无法验证 `tun` / `nf_tproxy` 的模块加载路径。
- 非 LXC 环境 → 无法验证 `lxc.cgroup2.devices.allow`、`lxc.mount.entry`、AppArmor、PVE seccomp 的真实效果。

---

## 5. PVE LXC Capability Matrix

**图例**：`S` = Supported｜`U` = Unavailable（环境能力缺失）｜`X` = Unsupported（技术路径不支持）｜`M` = Misconfigured（前置条件存在但配置错误）｜`?` = Unknown（需实测）

### 5.1 主矩阵

| 环境类型 | HTTP/SOCKS/Mixed proxy | TUN | TProxy (nftables) | redirect (iptables/nftables) | systemd | 内建 DNS 劫持 | auto-route |
|----------|------------------------|-----|-------------------|------------------------------|---------|---------------|------------|
| **privileged LXC（默认 PVE 配置）** | **S** — 只需可用端口；`<1024` 需 `CAP_NET_BIND_SERVICE` | **S** — `/dev/net/tun` 存在且 `CAP_NET_ADMIN` 默认保留 | **S** — 需 nft + `CAP_NET_ADMIN` + `ip_forward`/`route_localnet` | **S** — 同左 | **S** — 文档明示 systemd 可用（`nesting` 建议开启以隔离服务） | **S**（TUN/TProxy 成立时） | **S** |
| **unprivileged LXC（PVE 创建默认）** | **S** — 不需要任何网络特权 | **M→S** — **默认 U**（无 `/dev/net/tun` 且无 `CAP_NET_ADMIN`）；按第 7 节加 bind mount + `cgroup2.devices.allow` + `net_admin` 后为 **S** | **M** — 需显式保留 `CAP_NET_ADMIN` + sysctl 可写 + nft 可用；三者缺一即 **M** | **M** — 同 TProxy；iptables/nft 二进制缺失时需先安装 | **M** — 依赖 `nesting=1`；文档称 systemd 隔离服务需要 nesting；未开时可能 **M/U** | **M** | **M** |
| **VM（QEMU/KVM 上的 Debian/Ubuntu）** | **S** | **S** — root 或 ambient cap 即可 | **S** | **S** | **S** | **S** | **S** |
| **bare metal（Debian/Ubuntu）** | **S** | **S** | **S** | **S** | **S** | **S** | **S** |

### 5.2 每格所需前提与证据

| 格 | 前提 | 证据 |
|----|------|------|
| privileged LXC / HTTP | 端口 `>1024`（或 `CAP_NET_BIND_SERVICE`） | `[上游文档]` capabilities(7) |
| privileged LXC / TUN | `/dev/net/tun`（PVE 默认 privilege 容器内可见）+ `CAP_NET_ADMIN`（privileged 默认保留）+ 宿主 `CONFIG_TUN` | `[实测-容器]` 模式② + `[上游文档]` kernel tuntap.rst |
| privileged LXC / systemd | systemd ≥ 220（PVE 文档 Note 明确）；建议 `nesting=1` 以隔离服务 | `[上游文档]` pct chapter + pct.conf(5) |
| unprivileged LXC / TUN | ① `lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file 0 0` ② `lxc.cgroup2.devices.allow: c 10:200 rwm` ③ 容器内进程保留 `CAP_NET_ADMIN`（`lxc.cap.keep` 或非 drop） | `[实测-容器]` 模式④ + `[上游文档]` LXC conf / pct.conf(5) |
| unprivileged LXC / TProxy | TUN 前提 + nft/iptables 二进制 + `net.ipv4.ip_forward=1` + `net.ipv4.conf.<iface>.route_localnet=1` 可写 + `auto-route` 已启用 | `[实测-容器]`（nft/iptables 部分）+ `[上游文档]` mihomo tun 文档 + `[未验证]`（sysctl 可写） |
| unprivileged LXC / systemd | `features: nesting=1`；容器内 systemd ≥ 220 | `[上游文档]` pct.conf(5) |
| VM / bare metal | root 或 `AmbientCapabilities`（见官方 unit） | `[上游文档]` mihomo service 文档 |

### 5.3 重要：矩阵中**没有**的依赖

- **TUN 不需要 `SYS_ADMIN`**（实测 ③/④ 明确排除）。
- **TProxy 不需要 `SYS_ADMIN`**（同上）。
- **不需要 `CAP_SYS_MODULE`**（`tun` 由宿主内核按需加载，容器不加载模块）。
- **不需要 `CAP_DAC_OVERRIDE`** 只要 `/dev/net/tun` 权限位允许（内核文档明示 0666 无害）。
- **不需要 `features: fuse` / `keyctl` / `mount=`**。

---

## 6. doctor 探测项清单（可直接实现）

### 6.1 设计原则

1. **无副作用**：默认路径**绝不**修改路由表、netfilter 规则、sysctl。
2. **需要写操作才能判定的项**，走**显式 dry-run + 立即回滚**，且回滚失败必须报错而非静默。
3. 每个探测项输出**五值状态**之一 + 人类可读原因 + 修复建议。
4. 探测顺序：**先便宜后昂贵**，并且**失败短路**（`/dev/net/tun` 不存在时不必再试 `TUNSETIFF`——但要区分「不存在」和「存在但打不开」）。
5. **绝不**因为某项失败而返回整体失败；整体结果是各项状态的集合。

### 6.2 探测项表

| ID | 探测项 | 探测方式（无副作用优先） | 判定逻辑 | 失败降级建议 |
|----|--------|--------------------------|----------|--------------|
| `CAP-01` | 有效 capability 集合 | 读 `/proc/self/status` 的 `CapEff`；解析位掩码（bit 12 = `NET_ADMIN`，13 = `NET_RAW`，21 = `SYS_ADMIN`，27 = `MKNOD`，10 = `NET_BIND_SERVICE`，25 = `SYS_TIME`）。可选：`capsh --print` 若存在 | `NET_ADMIN=1` → 该能力 `Supported`；`=0` → `Unavailable` | 报告「TUN/透明代理需要 `CAP_NET_ADMIN`」；给出 `lxc.cap.keep` / `--cap-add` / systemd `AmbientCapabilities` 修复指引 |
| `CAP-02` | 是否为 privileged 容器 | 比较 `CapEff` 是否接近全集（如 `0x1ffffffffff`）；辅助：读 `/proc/1/status` 的 `CapBnd` | 仅用于**分档展示**，不用于功能判定 | — |
| `ENV-01` | 容器环境识别 | 读 `/proc/1/environ` 中 `container=lxc`；`/run/systemd/container`；`systemd-detect-virt`（若存在）；`/proc/self/cgroup` 是否含 `lxc` | 输出 `PVE LXC` / `Docker` / `VM` / `BareMetal` / `Unknown` | 影响后续建议文案；`Unknown` 不阻塞 |
| `ENV-02` | 是否 unprivileged LXC | 读 `/proc/self/uid_map`：若 `0` 映射到非 0 宿主 uid（如 `0 100000 65536`）→ unprivileged | 用于选择修复建议模板 | — |
| `ENV-03` | init 系统 | `cat /proc/1/comm`；`command -v systemctl`；`systemctl is-system-running`（有超时） | `/proc/1/comm == systemd` 且 `systemctl` 可执行且能连上 → `Supported`；二进制在但连不上 → `Misconfigured`；二进制不存在 → `Unsupported` | 降级为「Agent 自管子进程」模式；日志采 `systemctl`→文件重定向 |
| `TUN-01` | `/dev/net/tun` 存在性与属性 | `stat("/dev/net/tun")`；校验 `st_rdev == makedev(10,200)` 且是字符设备 | 不存在 → `Unavailable`（附「需 bind mount 或 `dev[n]` 透传」）；存在但 major/minor 不是 10:200 → `Misconfigured` | 提示第 7.2 节的 `lxc.mount.entry`/`lxc.cgroup2.devices.allow` 两行 |
| `TUN-02` | 设备可否打开 | `open("/dev/net/tun", O_RDWR)` 后**立即 close**（**不**发 `TUNSETIFF`，不建网卡，无流量） | `ENOENT` → `Unavailable`；`EACCES/EPERM` → `Misconfigured`（device cgroup 或权限位问题）；成功 → 继续 `TUN-03` | `Misconfigured` 时明确提示「device cgroup 未放行 `c 10:200 rwm`」或 SELinux/AppArmor 拦截 |
| `TUN-03` | 能否注册 TUN 网卡 | **dry-run 方式**：`open` → `ioctl(TUNSETIFF, "probe0", IFF_TUN\|IFF_NO_PI)` → 成功后**立刻 close**（内核在 close 时自动删除设备与路由，见内核文档）；或用 `ip tuntap add dev <随机名> mode tun` + `ip tuntap del` | 成功 → `Supported`；`EPERM` → `Unavailable`（缺 `CAP_NET_ADMIN`）或 `Misconfigured`（cap 在但 LSM 拦截，需结合 `CAP-01` 区分）；`ENODEV/ENOSYS` → `Unsupported`（内核无 `CONFIG_TUN`） | **不阻塞** HTTP/SOCKS；在 UI 中把 TUN 标为不可用并给出修复链接 |
| `TUN-04` | 内核模块 | `/proc/modules` 中查 `tun`；`/sys/module/tun` 是否存在。**不要**在容器内 `modprobe` | 有 → `Supported`；无但 `TUN-03` 成功 → `Supported`（内建或按需加载成功）；无且 `TUN-03` 失败 → `Unknown` | 建议宿主 `modprobe tun` 并持久化 |
| `NET-01` | netfilter 后端可用性 | `nft list ruleset`（**只读**） | 成功 → nftables `Supported`；`EPERM` → `Unavailable`；`ENOENT`（无 nft）→ `Unsupported`（未安装） | 未安装时提示安装 `nftables`；无权限时提示 `CAP_NET_ADMIN` |
| `NET-02` | iptables 可用性 | `iptables -t nat -L -n`（**只读**）；注意区分 `legacy` vs `nf_tables` 后端（`iptables --version`） | 成功 → `Supported`；`EPERM` → `Unavailable`；`ENOENT` → `Unsupported` | 同 `NET-01` |
| `NET-03` | netfilter 写入能力 | **可选、显式 opt-in**：建一个带随机名的空 table（`nft add table inet <rand>`）后立刻 `nft delete table inet <rand>`；或 `iptables -t mangle -N <rand>` + `-F` + `-X` | 成功并**验证删除成功** → `Supported`；失败 → `Unavailable` | 若用户禁用 write-probe，则用 `NET-01/02` 的可读性 + `CAP-01` 推断，并标 `Unknown` |
| `NET-04` | TProxy 内核支持 | 在 `NET-03` 允许的前提下，于临时 table 中 `add chain ... type filter hook prerouting priority mangle` + `add rule ... tproxy to :1`，随后删表 | 规则被接受 → `Supported`；`ENOENT`/`EOPNOTSUPP` → `Unsupported`（`nf_tproxy` 缺失或 nft 版本过旧） | 降级为 redirect（nat）模式或纯 TUN |
| `SYS-01` | `ip_forward` | 读 `/proc/sys/net/ipv4/ip_forward` 与 `/proc/sys/net/ipv6/conf/all/forwarding` | `=1` → `Supported`；`=0` → `Misconfigured`（需置 1） | 提示 `lxc.sysctl` / 宿主 sysctl；**不要**由 Agent 静默修改 |
| `SYS-02` | sysctl 可写性 | **无副作用探测**：检查 `/proc/sys` 的 mountinfo 是否含 `ro`；或尝试写**回原值**（`echo $(cat X) > X`） | 可写 → `Supported`；`EROFS` → `Unavailable`（只读挂载）；`EACCES/EPERM` → `Misconfigured` | 提示 `/proc/sys` 只读常见于 Docker（`--read-only`/默认）；PVE LXC 通常可写；仅作诊断信息 |
| `SYS-03` | `route_localnet` | 读 `/proc/sys/net/ipv4/conf/all/route_localnet` 与 `.../<iface>/route_localnet` | `=1` → `Supported`；`=0` 且启用 TProxy → `Misconfigured` | 提示置 1；TProxy 会静默失效，必须显式告警 |
| `SYS-04` | `rp_filter` | 读 `/proc/sys/net/ipv4/conf/all/rp_filter` | `=1` 且启用 TProxy → `Misconfigured`（严格反向路径过滤会丢 TProxy 回包） | 提示设为 `2`（loose）或 `0` |
| `ROUTE-01` | 路由表可读 | `ip route show table <iproute2-table-index>`（默认 2022） | 表存在且有 mihomo 写入的路由 → `Supported`（auto-route 生效）；表不存在 → 说明 auto-route 未启用（**不是错误**） | 用于展示「auto-route 是否已生效」 |
| `ROUTE-02` | 策略路由规则 | `ip rule show`，查找 `fwmark` 到表 2022 的规则 | 存在 → `Supported` | — |
| `DNS-01` | `dns-hijack` 前提 | 结合 `TUN-03` + `NET-03`/`SYS-01`；无独立探测 | 全部满足 → `Supported`；否则 `Unavailable` | 明确说明 DNS 劫持依赖 TUN 或透明代理 |
| `DNS-02` | 解析器配置 | 读 `/etc/resolv.conf`；`command -v resolvectl` | 存在 systemd-resolved → 记录；纯文件 → 记录 | 影响分流/DNS 建议，不阻塞 |
| `PKG-01` | 必需二进制 | `command -v nft iptables ip6tables ip` | 缺一即 `Unsupported` 并列出缺失项 | 给出对应发行版安装命令 |
| `INIT-01` | systemd 可托管性 | `systemctl is-system-running`（带 5s 超时） | `running`/`degraded` → `Supported`；`offline`/超时 → `Unavailable` | 降级到自管进程 + 日志文件 |

### 6.3 五值判定规则汇总（供实现直接映射）

| 状态 | 判定规则 | 典型例子 |
|------|----------|----------|
| `Supported` | 探测命令/系统调用成功，且后续依赖项也满足 | `TUNSETIFF` 成功 + `CAP_NET_ADMIN` 在 |
| `Unavailable` | 运行时/环境不具备该能力，但**技术路径本身合法** | 无 `CAP_NET_ADMIN`；无 `/dev/net/tun`；`EPERM` |
| `Unsupported` | 当前平台/内核/软件根本不提供该机制 | 非 Linux 平台；内核 `CONFIG_TUN=n`；未安装 nftables |
| `Misconfigured` | **前置条件存在但配置不对** | 有 `/dev/net/tun` 但 `open` 返回 `EACCES`；能建 tun 但 `ip_forward=0`；`iptables` 无权限但 `CAP_NET_ADMIN` 在（LSM 拦截）；`route_localnet=0` 却启用了 TProxy |
| `Unknown` | 探测本身无法执行或结果不可解释 | 只读 `/proc/sys` 导致写探测跳过；`NET-03` 被用户禁用；探测超时 |

> **`Misconfigured` 是最有价值的状态**：它把「可用但没配好」和「根本不可用」区分开，直接决定 UI 该显示「去修配置」还是「此功能不适用」。

### 6.4 给 Agent 的探测纪律（必须写进实现）

1. **`TUN-01` 通过 ≠ TUN 可用。** 必须走完 `TUN-02` → `TUN-03`。实测已证明「设备存在 + `open` 成功 + `TUNSETIFF` `EPERM`」是真实存在的组合。
2. **写探测必须成对回滚。** `TUN-03` 依赖「close fd ⇒ 设备自动消失」这一内核语义（内核文档保证），这比 `ip tuntap add/del` 更干净，**优先用 ioctl 方式**。
3. **`nft` 临时表名要随机化**，避免与用户规则冲突；删除后要**回读校验**。
4. **任何写探测前先持久化原状态**（sysctl 值、规则集 hash），探测后验证恢复。
5. **探测结果要缓存 + 可失效**：容器配置（`pct set`）变更后必须能重新探测；提供 `proxyctl doctor --refresh`。
6. **绝不**在探测中执行 `modprobe`、`iptables -F`、`sysctl -w`、路由修改。

---

## 7. 三种目标场景的最小权限组合

> 以下配置行中的 `lxc.cgroup2.*` / `lxc.mount.entry` / `lxc.sysctl.*` 形式来自 LXC 上游文档（`[上游文档]`）；`c 10:200` 来自内核 `tuntap.rst`；**具体可用性必须在目标 PVE 上实测**（`[未验证]`，见第 9 节）。配置文件位置：`/etc/pve/lxc/<CTID>.conf`，或用 `pct set <CTID> ...`。

### 7.1 场景 A：只跑 HTTP/SOCKS/Mixed 代理

**推荐：unprivileged LXC。**

`/etc/pve/lxc/<CTID>.conf` 增补（通常**什么都不用加**）：

```ini
# 场景 A 不需要任何额外 lxc.* 行。
# 仅当需要监听 <1024 端口时，才需要 CAP_NET_BIND_SERVICE：
lxc.cap.keep: net_bind_service
```

要点：

- HTTP/SOCKS/Mixed 监听端口默认 `7890/7891/7893` 等（`>1024`），**无需任何 capability**。
- 不需要 `/dev/net/tun`，不需要 `CAP_NET_ADMIN`，不需要 netfilter，不需要 sysctl 改动。
- **这是唯一在「零特权」下就能完整工作的场景，必须是最稳的默认档。**
- 若站点要求 `<1024` 端口，优先改用 `>1024` + 上游反代，而非给容器加 capability。

### 7.2 场景 B：要跑 TUN

**推荐：unprivileged LXC + 显式 tun 透传。**（privileged 也能跑，但没必要为了 TUN 把整个容器提权。）

```ini
# --- 1) 把宿主的 /dev/net/tun 绑进容器，并让 LXC 自动创建挂载点文件 ---
lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file 0 0

# --- 2) 在 cgroup v2 device controller 上放行 char 10:200（tun）读写 ---
#     注意：PVE 9 已移除 cgroup v1，lxc.cgroup.* 不再可用，必须用 cgroup2
lxc.cgroup2.devices.allow: c 10:200 rwm

# --- 3) 确保容器内持有 CAP_NET_ADMIN ---
#     若 PVE 默认已保留则可不写；若做了加固（lxc.cap.drop）务必显式 keep
lxc.cap.keep: net_admin
```

要点与坑：

- **坑 1：只做 bind mount 不够。** device cgroup（cgroup v2 下是 `BPF_PROG_TYPE_CGROUP_DEVICE`）会拦住设备访问。两条都要。实测模式④证明了「`NET_ADMIN` + 设备可用」才是充分条件。
- **坑 2：不要依赖 `mknod`。** measured 证明 `CAP_MKNOD` 在、`mknod` 成功，但访问仍被 device cgroup 拒绝。而 `create=file` 让 LXC 在挂载时创建挂载点，**根本不需要 `mknod`**，因此也**不需要** `features: mknod=1`（该选项 PVE 文档标注为 experimental）。
- **坑 3：`lxc.cgroup2.devices.deny = a` 会清空之前所有规则。** 若部署模板里有 deny-all，必须把 `allow: c 10:200 rwm` 写在**它之后**，且不要把其他 allow 规则弄丢。
- **坑 4：unprivileged 下 `lxc.mount.entry` 的源路径不能含符号链接**（PVE 文档安全限制）。
- **坑 5：只开 TUN 不开 auto-route 时，Mihomo 只是多了一张网卡**，流量不会自动进来。`auto-route` 需要 `CAP_NET_ADMIN`（同一 capability，无需追加）。
- **坑 6：`stack: gvisor` 降低但**不消除**特权要求**——建卡这一步照样要 `/dev/net/tun` + `CAP_NET_ADMIN`。
- 若必须用 `systemd` 托管 Mihomo 且容器是 unprivileged，建议同时 `features: nesting=1`（PVE 文档：systemd 隔离服务需要它）。注意文档也警告它会向 guest 暴露宿主 procfs/sysfs。

### 7.3 场景 C：要跑 nftables 透明代理（TProxy 或 redirect）

**推荐：unprivileged LXC 也完全可行；capability 需求与场景 B 完全相同，额外门槛在 sysctl 与 netfilter 用户态工具。**

在 7.2 的基础上追加：

```ini
# --- 宿主侧/容器侧 sysctl（LXC 支持 lxc.sysctl.<name> 设置内核参数） ---
lxc.sysctl.net.ipv4.ip_forward: 1
lxc.sysctl.net.ipv4.conf.all.route_localnet: 1
# 若担心严格反向路径过滤丢回包，可放宽（按需，非必须）：
# lxc.sysctl.net.ipv4.conf.all.rp_filter: 2

# --- capability 与 B 相同：net_admin 即可 ---
lxc.cap.keep: net_admin
```

容器内需安装：`nftables`（或 `iptables`）+ `iproute2`。

Mihomo 侧配置要点：

```yaml
tun:
  enable: true
  auto-route: true
  auto-redirect: true     # 仅 Linux；会自动写 iptables/nftables（需 auto-route: true）
  auto-detect-interface: true
  dns-hijack:
    - any:53
    - tcp://any:53
```

或使用纯 TProxy 监听：

```yaml
listeners:
  - name: tproxy-in
    type: tproxy
    port: 7894
    listen: 0.0.0.0
    udp: true
```

要点与坑：

- **坑 1：`route_localnet` 必须为 1。** TProxy 场景下本机发往 `127.0.0.0/8` 的流量默认被丢弃。这是最容易被忽略、且失败时**无明确报错**的一项——doctor 必须显式检查。
- **坑 2：`ip_forward` 必须为 1。** 尤其当容器作为网关/透明网关时。
- **坑 3：`lxc.sysctl.*` 对非 namespaced sysctl 会改宿主全局值。** LXC 文档明确警告："Note that not all sysctls are namespaced. Changing Non-namespaced sysctls will cause the system-wide setting to be modified." → `ip_forward` 是 **per-netns** 的（安全），但仍需注意别误设全局项。
- **坑 4：Docker 里 `/proc/sys` 只读不代表 LXC 也如此。** 实测的 `EROFS` 是 Docker 挂载策略（`/proc/sys ro`），**不要**据此给 PVE 用户下「透明代理不可行」的结论。反过来，doctor 检测到 `EROFS` 时也只应报 `Unavailable` 并提示挂载方式，而不是宣称平台不支持。
- **坑 5：`AutoRedirect` 自动改写 netfilter 规则**，会与用户已有的 nft/iptables 规则**互相影响**。Agent 在启用前应快照现有规则集（用于回滚），并在 doctor 中提示冲突风险。
- **坑 6：`CAP_NET_ADMIN` 是唯一必需的 capability**，实测已排除 `SYS_ADMIN`。不要为了「保险」给容器加 `SYS_ADMIN`——那是显著扩大攻击面。

### 7.4 三种场景的 privilege 选择建议（一句话版）

```text
场景 A（HTTP/SOCKS）      → unprivileged，零额外 lxc.* 配置         ← 必须是最稳默认档
场景 B（TUN）             → unprivileged + tun 两行 + cap.keep net_admin
场景 C（透明代理）        → 同场景 B + sysctl 两行 + 安装 nftables/iptables
场景 B/C 的 privileged 替代 → 仅在用户明确接受风险时；不要作为默认推荐
```

---

## 8. 降级状态设计（五值状态如何产生）

### 8.1 能力状态不是布尔的，是因为失败原因不同

| 观测 | 正确状态 | 为什么不能是 `false` |
|------|----------|---------------------|
| 宿主内核没有 `CONFIG_TUN` | `Unsupported` | 用户做什么都没用，UI 应隐藏该功能 |
| 无 `/dev/net/tun` 设备节点 | `Unavailable` | 用户加一行 `lxc.mount.entry` 就能解决，UI 应给指引 |
| `/dev/net/tun` 在但 `open` 返回 `EACCES` | `Misconfigured` | 文件在、权限/device-cgroup 不对，是**配置问题** |
| 设备可打开但 `TUNSETIFF` 返回 `EPERM` | `Unavailable`（缺 `CAP_NET_ADMIN`） | capability 问题，**这正是实测模式①的真实情形** |
| capability 在、`TUNSETIFF` 被 LSM 拦截 | `Misconfigured` | 需要看 `dmesg \| grep apparmor` |
| TUN 全通，但 `ip_forward=0` | TUN=`Supported`，TProxy=`Misconfigured` | **同一环境里不同能力状态不同**——这正是「不能用一个 bool 描述环境」的核心证据 |
| 探测本身跑不了（只读 `/proc/sys`） | `Unknown` | 诚实表达不确定性，而不是猜 |

### 8.2 合法降级态示例（必须被产品正常接受）

```text
Basic Proxy        ✓ Supported      (HTTP/SOCKS/Mixed 端口可用)
TUN                ✗ Unavailable    (无 CAP_NET_ADMIN)
nftables           ✓ Supported      (nft 可读可写)
TProxy             ✗ Misconfigured  (ip_forward=0 / route_localnet=0)
systemd            ~ Unavailable    (容器内无 systemd，Agent 自管进程)
```

→ 这个状态**完全可用**：用户通过 `7890` 端口用代理，一切正常。Agent **必须**在这种状态下正常 start/reload/更新订阅，绝不因为 TUN 不可用而拒绝操作。

### 8.3 状态传播规则（对架构的约束）

1. 能力状态存在 **System Domain**，通过 Capability Port 暴露给 Application。
2. Application 的 use case（`RunDoctor`、`StartMihomo`、`ActivateConfig`）**读取**能力状态用于**决策与提示**，但**不得**把能力可用性写成前置硬条件——除非该功能**语义上**必须有它（例如「开启 TUN」obviously 需要 TUN）。
3. `ActivateConfig` 必须能对「配置里 `tun.enable: true` 但 TUN `Unavailable`」给出**明确的前置校验失败**（这是配置校验，不是环境崩溃），并且**保留旧配置**。
4. 能力状态变化应产生 `SystemCapabilityChanged` 事件（AGENTS.md 已列出该事件）。

---

## 9. 真实 PVE 补测清单（未完成项）

> 以下每一项都需要在**真实 PVE 主机**（建议 PVE 8.x 与 9.x 各一台，或至少 9.x）上执行。标记 `[未验证]`。
> 建议准备 4 个 CT：`privileged`、`unprivileged`（默认）、`unprivileged+nesting`、`unprivileged+tun透传`。

### 9.1 配置类验证

| # | 待验证项 | 执行方式 | 期望/待观察 |
|---|----------|----------|-------------|
| P1 | `lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file 0 0` 是否让 unprivileged 容器出现 `/dev/net/tun` | 写入 `/etc/pve/lxc/<CTID>.conf`，重启 CT，容器内 `ls -l /dev/net/tun` | 期望 `crw-rw-rw- ... 10, 200` |
| P2 | `lxc.cgroup2.devices.allow: c 10:200 rwm` 是否真的放行设备访问 | 容器内 `open(O_RDWR)` + `TUNSETIFF` | 期望成功；若失败读取 `dmesg \| grep -i -E 'apparmor\|audit'` |
| P3 | 只做 P1 不做 P2 会怎样 | 移除 P2 重启 | 期望 `open` 或 `TUNSETIFF` 失败 → 证明两条都必需 |
| P4 | 只做 P2 不做 P1 会怎样 | 移除 P1 | 期望 `/dev/net/tun` 不存在 → `Unavailable` |
| P5 | `features: mknod=1` 是否是替代方案 | 开 mknod，容器内 `mknod` + `TUNSETIFF` | 期望：能建节点但仍需 P2；确认 PVE 文档"experimental"的实际表现 |
| P6 | `pct set <CTID> -dev0 /dev/net/tun` 是否等价且更安全 | 用 `dev[n]` 而非手写 `lxc.*` | 比较两种写法的实际 `/dev` 结果与 cgroup 规则 |
| P7 | PVE 默认 `lxc.cap.drop` 列表是什么 | 在 CT 内读 `/proc/self/status` + 检查 `/usr/share/lxc/config/*.common.conf` | 确认 `net_admin` 是否默认被 drop（决定是否必须 `lxc.cap.keep`） |
| P8 | `lxc.cap.drop` 与 `lxc.cap.keep` 的执行顺序/优先级 | 同时写两者，观察实际 `CapEff` | 确定正确的加固写法 |
| P9 | `lxc.sysctl.net.ipv4.ip_forward: 1` 是否被 PVE/LXC 接受 | 写入配置重启，容器内读该值 | 期望 `1`；确认是否影响宿主全局 |
| P10 | unprivileged LXC 内 `/proc/sys` 是否可写 | 容器内 `echo $(cat X) > X` | 期望可写（与 Docker 的 `EROFS` 相反）→ 决定 `SYS-02` 的判定 |
| P11 | `route_localnet` 在 PVE LXC 中的默认值与可写性 | 读 + 写回原值 | 决定 TProxy 是否需要显式配置 |
| P12 | PVE 的 AppArmor profile 是否拦截 `TUNSETIFF` / nft | `dmesg \| grep apparmor`；查看 `/etc/apparmor.d/lxc/` | 期望不拦截；若拦截记录确切的拒绝规则 |

### 9.2 功能类验证

| # | 待验证项 | 执行方式 | 期望/待观察 |
|---|----------|----------|-------------|
| P13 | unprivileged LXC + tun 透传下，Mihomo 能否成功启动 TUN 入站 | 用真实 mihomo 二进制 + `tun.enable: true` | 期望成功建卡（**不做真实隧道流量**，仅验证启动与网卡出现） |
| P14 | `auto-route: true` 能否写入路由表 2022 | 启动后 `ip route show table 2022`、`ip rule show` | 期望出现 mihomo 的路由与 fwmark 规则 |
| P15 | `auto-redirect: true` 能否写 netfilter | 启动后 `nft list ruleset`（或 `iptables-save`） | 期望出现 mihomo 的规则；记录规则集快照用于回滚 |
| P16 | `dns-hijack` 是否生效 | 启动后捕获 53 端口相关规则 | 期望出现劫持规则（不验证实际解析） |
| P17 | 纯 TProxy 监听（`type: tproxy`）在 unprivileged LXC 下是否可建立并接收连接 | 启动监听，本机 `curl --resolve` 打一个测试端口（**不打真实代理流量**） | 期望监听成功 |
| P18 | redirect（nat）模式的最小配置是否成立 | `iptables -t nat -A ... -j REDIRECT` + mihomo redirect listener | 期望连通性验证通过 |
| P19 | `nf_tproxy` / `xt_TPROXY` 是否在 PVE 宿主内核可用 | 在 CT 内建临时 nft tproxy 规则后删除 | 期望成功；若失败记录内核配置 |
| P20 | PVE 宿主内核 `CONFIG_TUN` 与 `tun` 模块 | 宿主 `modprobe tun`、`lsmod \| grep tun` | 期望存在或可按需加载 |

### 9.3 systemd / 生命周期验证

| # | 待验证项 | 执行方式 | 期望/待观察 |
|---|----------|----------|-------------|
| P21 | unprivileged LXC 内 systemd 是否正常工作 | `systemctl is-system-running` | 期望 `running`/`degraded` |
| P22 | `nesting=1` 对 systemd 隔离服务的实际影响 | 对比开/关 nesting 时 `systemctl` 是否可用 | 验证 PVE 文档"systemd requires nesting to isolate services" |
| P23 | 用官方 systemd unit（含 `AmbientCapabilities`）在 LXC 内托管 mihomo 是否成功 | 部署官方 unit | 期望 ambient caps 生效；确认容器内能否设置 ambient caps |
| P24 | 容器内非 root 运行 mihomo + ambient caps 是否仍能建 TUN | 建非特权用户，`setcap`/unit 方式给 caps | 期望可行；这是最安全的生产形态 |
| P25 | PVE 重启/CT 重启后能力是否保持 | 重启后重跑 doctor | 期望一致 |
| P26 | `pct set` 改配置后是否需重启 CT 才生效 | 改 `lxc.*` 后 `pct set` 并重启 | 确认生效时机（影响 `doctor --refresh` 语义） |

### 9.4 反向验证（避免过度乐观）

| # | 待验证项 | 目的 |
|---|----------|------|
| P27 | 在**未透传 tun** 的 unprivileged LXC 上确认 TUN 确实不可用，且 HTTP/SOCKS 仍然可用 | 验证降级态真实存在且产品可接受 |
| P28 | 在有 `/dev/net/tun` 但无 `CAP_NET_ADMIN` 的容器上确认得到 `Unavailable` 而非崩溃 | 验证实测模式①的判定逻辑在 PVE 上可复现 |
| P29 | 确认 `lxc.cgroup2.devices.deny = a` 之后 `allow` 规则顺序问题的真实表现 | 验证 LXC 文档"clears all previous rules"的后果 |
| P30 | 确认 AppArmor `unconfined` **不是**必需（即默认 profile 下 TUN 可用） | 避免向用户推荐降低安全性的方案 |

---

## 10. 对 Agent 架构的影响（System Domain / Capability Port）

### 10.1 Domain 模型（`domain/system`）

AGENTS.md 已规定 `system/` 拥有 `Platform`、`Architecture`、`InitSystem`、`ContainerEnvironment`、`CapabilityStatus`、`NetworkCapabilities`。基于本次调研，建议细化：

```rust
// domain/system —— 纯值对象，无 I/O

/// 五值状态。注意：不是 bool，且必须能携带"为什么"。
pub enum CapabilityStatus {
    Supported,
    Unsupported,     // 平台/内核根本不提供
    Unavailable,     // 环境未授权（缺设备/缺 capability）
    Misconfigured,   // 前置条件在，但配置不对
    Unknown,         // 无法判定
}

/// 探测证据，让 UI/CLI 能解释状态来源
pub struct CapabilityEvidence {
    pub status: CapabilityStatus,
    pub detail: String,          // 人类可读原因
    pub probe_id: ProbeId,       // 如 TUN-03
    pub remediation: Option<String>, // 修复建议（如具体的 lxc.* 行）
}

pub struct NetworkCapabilities {
    pub tun_device: CapabilityEvidence,      // TUN-01/02/03 合成
    pub tun_net_admin: CapabilityEvidence,   // CAP-01 的 NET_ADMIN 位
    pub nftables: CapabilityEvidence,        // 读 + 可选写
    pub iptables: CapabilityEvidence,
    pub tproxy: CapabilityEvidence,          // NET-04
    pub redirect: CapabilityEvidence,
    pub auto_route: CapabilityEvidence,      // ROUTE-01/02 + NET_ADMIN
    pub ip_forward: CapabilityEvidence,      // SYS-01
    pub route_localnet: CapabilityEvidence,  // SYS-03
    pub sysctl_writable: CapabilityEvidence, // SYS-02
    pub dns_hijack: CapabilityEvidence,      // 派生
    pub systemd: CapabilityEvidence,         // ENV-03
    pub container: ContainerEnvironment,     // Lxc{privileged:bool} | Docker | None | Unknown
}
```

**关键设计约束**：

- `CapabilityStatus` 与 `CapabilityEvidence` 属于 Domain（纯数据 + 纯规则），**不能**包含探测实现。
- 「能力 A 是否足以支撑功能 B」这类**纯规则**可放 Domain（例如 `fn tun_usable(&self) -> CapabilityStatus`，由 `tun_device` ∩ `tun_net_admin` 推导）。这正好把实测发现的「两条件交集」固化成领域不变量。
- 任何 `std::process::Command`、`File::open`、`ioctl` 都**不得**出现在 Domain。

### 10.2 建议的 Port（Application 定义）

保持小而 capability-oriented（AGENTS.md 明确反对 giant `SystemManager`）：

```rust
// application/ports —— 只声明"需要什么信息"

#[async_trait]
pub trait CapabilityProbe: Send + Sync {
    /// 探测单个能力项；实现内部保证无副作用或自回滚。
    async fn probe(&self, id: ProbeId) -> Result<CapabilityEvidence>;

    /// 探测全部项，返回快照。
    async fn probe_all(&self, opts: ProbeOptions) -> Result<CapabilitySnapshot>;
}

#[derive(Default)]
pub struct ProbeOptions {
    /// 是否允许有副作用的写探测（临时 table / sysctl 写回）。
    /// 默认 false —— 无副作用优先。
    pub allow_write_probes: bool,
}

#[async_trait]
pub trait NetfilterManager: Send + Sync {
    /// 读取当前规则集快照（只读，用于 doctor 与回滚）。
    async fn snapshot(&self) -> Result<RulesetSnapshot>;
    /// 应用 mihomo 所需规则；失败必须能恢复快照。
    async fn apply(&self, plan: RulePlan) -> Result<()>;
    async fn rollback(&self, snapshot: RulesetSnapshot) -> Result<()>;
}

#[async_trait]
pub trait SysctlAccess: Send + Sync {
    async fn read(&self, key: SysctlKey) -> Result<String>;
    /// 写入；调用方负责保存并恢复原值。
    async fn write(&self, key: SysctlKey, value: &str) -> Result<()>;
}
```

### 10.3 Application use case：`RunDoctor`

```text
RunDoctor
  ├─ 读取缓存的 CapabilitySnapshot（或按 opts 重新探测）
  ├─ 组合派生状态（TUN = tun_device ∩ tun_net_admin；TProxy = tproxy_kernel ∩ net_admin ∩ ip_forward ∩ route_localnet）
  ├─ 生成 DoctorReport { system, mihomo, network, runtime, verdicts }
  └─ 每种 Verdict 附 remediation（可执行的 lxc.* / sysctl / 包安装指引）
```

**必须遵守**：

- `RunDoctor` **不修改**环境（除非显式 `--fix`，且 `--fix` 属于独立 use case，需 ADR）。
- `RunDoctor` 的输出 DTO 与 Domain 模型分离（AGENTS.md：不得直接暴露 Domain 实体）。

### 10.4 进程管理：systemd 不可用时的降级

实测证明容器内极可能没有可用 systemd。因此：

- `ProcessManager` Port 必须有**至少两个**适配器：
  1. `SystemdProcessManager`（bare metal / VM / 开了 nesting 且 systemd 正常的 LXC）
  2. `SupervisedChildProcessManager`（容器内自管子进程 + 信号 + 日志重定向 + 重启退避）
- 选择逻辑放 Infrastructure/bootstrap，由 `CapabilitySnapshot.systemd` 决定。
- **不能**让 Application 依赖 systemd 存在。

### 10.5 与「失败不得影响其他功能」不变量的对应

| 不变量 | 本次调研给出的具体约束 |
|--------|------------------------|
| 能力不可用不得导致其他功能失败 | TUN/TProxy 探测失败**绝不**阻塞 `StartMihomo`/`ReloadMihomo`/订阅更新 |
| 配置校验前置 | `tun.enable: true` 但 TUN 不可用 → **ActivateConfig 前置失败**（保留旧配置），而不是启动后崩 |
| 回滚优先 | 启用 `auto-redirect` 前**必须**快照 netfilter 规则集；失败恢复快照 |
| 显式能力优于假设 | 所有 `LXC == X` 的推断必须由 `CapabilityProbe` 输出替代 |

### 10.6 需要在后续 ADR 中决策的点

1. `--fix`（自动修复 sysctl / 安装包 / 写 cgroup 规则）是否进入 MVP？（建议**不进入**，只给指引。）
2. `ProbeOptions::allow_write_probes` 默认值与用户可见开关。
3. DoctorReport 的 JSON schema（`proxyctl doctor --json`）。
4. 能力快照的持久化位置与失效策略（`/var/lib/proxy-agent/state/`）。

---

## 11. 证据与来源

### 11.1 上游文档（权威）

| 来源 | URL | 关键贡献 |
|------|-----|----------|
| Proxmox VE — `pct.conf(5)` 手册 | https://pve.proxmox.com/pve-docs/pct.conf.5.html | `features` 全子选项原文；`dev[n]`；`unprivileged`；`nesting` 与 systemd；`mount`/`keyctl`/`mknod` 语义；允许直接写 `lxc.*` |
| Proxmox VE — Proxmox Container Toolkit（pct 章节） | https://pve.proxmox.com/pve-docs/chapter-pct.html | cgroup v1 在 PVE 7.0 废弃、**PVE 9.0 移除**；`LXC` 基础架构；OCI 支持 |
| Proxmox VE Wiki — Linux Container | https://pve.proxmox.com/wiki/Linux_Container | unprivileged/privileged 安全模型；AppArmor（`lxc.apparmor.profile = unconfined` 不推荐）；seccomp；cgroup v2；systemd ≥ 220；bind/device mount point；symlink 限制 |
| LXC 上游 — `lxc.container.conf(5)` | https://raw.githubusercontent.com/lxc/lxc/main/doc/lxc.container.conf.sgml.in | `lxc.cgroup2.*` 必须（`lxc.cgroup.*` **不再支持**）；device controller 的 eBPF 实现；allow/deny=list 语义与"清空之前规则"；`lxc.mount.entry` + `create=file`；`lxc.cap.drop` / `lxc.cap.keep`；`lxc.sysctl.*` 与"非 namespaced sysctl 改全局"警告；`lxc.idmap`；seccomp profile |
| Linux kernel — `Documentation/networking/tuntap.rst` | https://raw.githubusercontent.com/torvalds/linux/master/Documentation/networking/tuntap.rst | `/dev/net/tun` 必须 `open` + `ioctl(TUNSETIFF)`；`mknod /dev/net/tun c 10 200`；**"CAP_NET_ADMIN is required for creating network devices"**；模块可按需自动加载；close fd ⇒ 设备与路由消失 |
| Linux man-pages — `capabilities(7)` | https://man7.org/linux/man-pages/man7/capabilities.7.html | `CAP_NET_ADMIN` 覆盖 interface config / IP firewall / routing tables / transparent proxying；`CAP_NET_RAW`；`CAP_MKNOD`；`CAP_SYS_ADMIN` 的实际范围（不含上述网络项） |
| Mihomo 官方文档 — Tun | https://wiki.metacubex.one/config/inbound/tun/ | `auto-redirect` 仅 Linux 且依赖 `auto-route`；`gso` 仅 Linux；`route-address-set` 需 nftables；`iproute2-table-index` 默认 2022；**全文未要求 `SYS_ADMIN`** |
| Mihomo 官方文档 — tproxy listener | https://wiki.metacubex.one/config/inbound/listeners/tproxy/ | `type: tproxy` 监听配置结构 |
| Mihomo 官方文档 — 创建运行服务（systemd） | https://wiki.metacubex.one/startup/service/ | 官方 systemd unit，含 `CapabilityBoundingSet` / `AmbientCapabilities` 的权威 capability 集合 |

### 11.2 实测记录（`[实测-容器]`，OrbStack，非 PVE）

- 环境：macOS arm64；OrbStack Docker；内核 `7.0.14-orbstack-00380-ga7e0a2dc9535`；镜像 Alpine 3.23.3 aarch64（`postgres:16-alpine` + `apk add iproute2 iptables nftables libcap python3`）。
- 四种模式：默认 / `--privileged` / `--cap-add=NET_ADMIN` / `--cap-add=NET_ADMIN --device /dev/net/tun`。
- 所有写探测均自建自删（`ip tuntap add`→`del`、`ip link add`→`del`、`nft add table`→`delete table`、`iptables -N`→`-F`→`-X`），`sysctl` 写探测回写原值；未建立任何真实隧道流量。
- 所有容器以 `--rm` 运行；探测镜像与临时目录已清理。

### 11.3 未被采用为唯一依据的来源

调研过程中遇到的社区文章（如第三方 LXC/TUN 教程）**未**作为结论依据。本文件中所有配置行均可追溯至 11.1 的上游文档，或 11.2 的实测输出。

---

## 12. 未验证假设与开放问题

### 12.1 明确标注的假设

| # | 内容 | 标记 |
|---|------|------|
| A1 | PVE LXC 默认 `/proc/sys` 为可写（与 Docker 的 `ro` 挂载相反） | `[推测]` 依据：Docker 的只读来自其挂载策略；LXC 默认不这样挂载。**必须由 P9/P10 验证** |
| A2 | PVE 默认 `lxc.cap.drop` 不包含 `net_admin`（因此 unprivileged 容器应默认具备 `CAP_NET_ADMIN`） | `[推测]` 依据：PVE 设计意图是「privileged 容器保留全部 cap」。**必须由 P7 验证** |
| A3 | `lxc.mount.entry` 的 bind mount 能让 unprivileged 容器**不依赖 mknod** 获得 `/dev/net/tun` | `[推测]` 依据：LXC 文档的 `create=file` + bind mount 语义。**必须由 P1/P5 验证** |
| A4 | PVE 宿主内核已启用 `nf_tproxy` / `xt_TPROXY` | `[推测]` 依据：Debian 内核默认启用。**必须由 P19 验证** |
| A5 | PVE 宿主内核 `tun` 可用（内建或模块） | `[推测]`。**必须由 P20 验证** |
| A6 | AppArmor 默认 profile 不拦截 `TUNSETIFF` 与 nft 操作 | `[推测]`。**必须由 P12 验证** |
| A7 | OrbStack 内核的 netfilter/`tun` 特性集合与 Debian 内核足够接近，可用于验证 capability 因果关系 | `[推测]` 用于**因果**推论；不作为 PVE 行为的证据 |

### 12.2 开放问题

1. **`lxc.cap.keep` 与 PVE 默认 drop 列表的交互**：PVE 是否为每个 CT 生成 `lxc.cap.drop`？若用户写 `lxc.cap.keep: net_admin`，是"只保留这一项"还是"在前述基础上保留"？→ 影响 7.2 的推荐写法（P8）。
2. **`dev[n]` 与手写 `lxc.mount.entry` 的优劣**：`pct set -dev0` 是否自动处理 `cgroup2.devices.allow`？若是，应优先推荐 `dev[n]`（更 PVE-native、更少踩坑）。→ P6。
3. **`auto-redirect` 的规则冲突管理**：Mihomo 会自动改写 netfilter，但文档未描述其清理行为。Agent 需要知道：Mihomo 停止后规则是否残留？若残留，回滚策略是什么？
4. **unprivileged LXC 中 ambient capabilities 能否设置**：官方 systemd unit 依赖 `AmbientCapabilities`。在 user namespace 内 `PR_CAP_AMBIENT_RAISE` 是否可用（需要 `CAP_SETPCAP` 且不被 seccomp 拦）？→ P23/P24，直接决定「非 root 运行 Mihomo」是否可行。
5. **`nesting=1` 的代价**：PVE 文档警告它向 guest 暴露宿主 procfs/sysfs。是否存在比 `nesting` 更小的代价来让 systemd 正常？→ P22。
6. **`route_localnet` 的作用域**：应设 `all` 还是具体出口接口？Mihomo `auto-redirect` 自身是否会设置？若它会设置，doctor 的 `SYS-03` 判定应在 Mihomo 启动前后分别采样。
7. **内核 cgroup v2 device controller 在 PVE 上的具体表现**：LXC 文档说它基于 `BPF_PROG_TYPE_CGROUP_DEVICE`。若宿主内核或 systemd 版本不支持该 BPF 程序类型，device 规则可能静默失效。→ 与 P2 合并验证。
8. **多实例 / 多 CT 场景**：同一宿主上多个 CT 各自跑 Mihomo + TUN，路由表 2022 与 fwmark 规则是否互相干扰？（netns 隔离下应无干扰，但需确认。）→ 新增补测项。
9. **`iproute2-table-index` 冲突**：若宿主或其他 CT 已用 table 2022，Mihomo 的行为？是否可配置规避？
10. **OCI 容器的差异**：PVE 9 引入 OCI 应用容器（technology preview）。其 capability/device 模型与系统容器是否一致？→ 后续研究。

### 12.3 对 Phase 0 其他研究项的输入

- **R11（Network Stack）**：本文件第 3 节与第 6 节可直接作为 nftables/iptables/TProxy 探测项的输入；`route_localnet` / `rp_filter` 的交叉影响需 R11 补充。
- **R9（Linux runtime）/ 安全研究**：第 7 节的「最小权限组合」与第 10.6 节的 `--fix` 决策应与安全研究项对齐（提权面评估）。
- **架构**：第 10 节建议的 `CapabilityProbe` / `NetfilterManager` / `SysctlAccess` Ports 若被采纳，需要 ADR（涉及 Port 定义与进程监督模型）。

---

*本文件为 Phase 0 Architecture Discovery 子任务 R10 的交付物。所有 PVE 特定结论在真机验证前均应视为待验证；请优先执行第 9 节清单。*
