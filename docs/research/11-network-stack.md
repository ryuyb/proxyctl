# R11 — 网络栈与透明代理范围

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：上游官方文档 + 上游源码（mihomo `Alpha` / sing-tun `meta`）+ Debian 12 容器实测（Docker/OrbStack，**非 PVE 证据**）
> 关键结论一句话：**Mihomo 在 Linux 上有两条互不相同的透明化路径 —— TUN（`auto-route`/`auto-redirect` 由 Mihomo 进程自己用 netlink 写路由与 nftables，只需 `CAP_NET_ADMIN` + `/dev/net/tun`）与 TProxy/redirect（Mihomo 只提供监听端口，规则必须由外部防火墙写入，或者退化到 Mihomo 自带的 legacy `iptables: enable` 自动改全局 iptables）；MVP 应把 Mixed 端口与「TUN 可选启用 + 前置检测 + 失败降级」列为 Supported，把一切「Agent 自动写防火墙规则」列为 Detection Only / Later，把改写宿主网络、无 `CAP_NET_ADMIN` 强行接管、自动改系统 DNS 列为 Unsupported。**

---

## 1. 结论摘要（TL;DR）

| # | 结论 | 证据 |
|---|---|---|
| C1 | **TUN 与 TProxy 是两条独立技术路径，前置条件不同。** TUN 由 Mihomo 进程内通过 netlink 直接建 TUN 设备、装路由与 rule，不需要 `ip` 命令，也不需要外部防火墙二进制；TProxy/redirect 入站（`tproxy-port` / `redir-port` / `listeners.type=tproxy\|redir`）本身**不装任何规则**，必须由外部 nftables/iptables 规则把流量送进来 | [上游文档] [Mihomo TUN](https://wiki.metacubex.one/config/inbound/tun/)、[代理端口](https://wiki.metacubex.one/config/inbound/port/)；[上游源码] `listener/sing_tun/tun_linux.go`、`listener/tproxy/tproxy.go`、`listener/redir/tcp_linux.go` |
| C2 | **TUN 的最小前提**：`/dev/net/tun`（char 10:200）+ `CAP_NET_ADMIN`；`stack: gvisor` 在用户态自带 TCP/IP 协议栈，`system`/`mixed` 则把 TCP（mixed 还有 UDP）交给内核协议栈处理，因此对内核 netfilter/路由状态的依赖更强。**不需要** nftables/iptables 二进制、不需要 `ip` 命令 | [上游源码] `listener/sing_tun/*`；[实测-容器] 无 `CAP_NET_ADMIN` 时 `TUNSETIFF` 直接 `EPERM`，有 `CAP_NET_ADMIN` 时 `ip tuntap add dev r11tun0 mode tun` 成功 |
| C3 | **`auto-redirect` 是 Mihomo 自研的「类 redirect」实现，不是 TPROXY。** 它默认用进程内 nftables 客户端（`github.com/metacubex/nftables`，netlink）创建 `table inet mihomo`：TCP 用 nftables `redirect`（NAT REDIRECT）打到 sing-tun 内部 redirect server，UDP/ICMP 走 mark + 策略路由进 TUN，DNS 用 DNAT 劫持到内置 DNS。**硬依赖 `auto-route`**，否则启动报错 `` `auto-route` is required by `auto-redirect` ``。内核不支持 nftables 时自动回退到调用外部 `iptables`/`ip6tables`（`exec.LookPath`），或用 `DISABLE_NFTABLES=1` 强制回退 | [上游源码] `listener/sing_tun/server.go:424-447`、`metacubex/sing-tun@meta` `redirect_nftables.go` / `redirect_linux.go` / `redirect.go` |
| C4 | **内核模块名（官方）**：nftables 路径需要 `NFT_TPROXY`（模块 `nft_tproxy`）+ `NFT_SOCKET`（`nft_socket`），二者 select `NF_TPROXY_IPV4`/`NF_TPROXY_IPV6`（`nf_tproxy_ipv4`/`nf_tproxy_ipv6`）；iptables 路径需要 `NETFILTER_XT_TARGET_TPROXY`（`xt_TPROXY`）+ `NETFILTER_XT_MATCH_SOCKET`（`xt_socket`）。**Mihomo 全树没有任何 `modprobe` 调用**，模块只能由内核 autoload（`request_module`）或运维预加载 | [上游文档] [kernel `Documentation/networking/tproxy.rst`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/networking/tproxy.rst) §3、[kernel `net/netfilter/Kconfig`](https://raw.githubusercontent.com/torvalds/linux/master/net/netfilter/Kconfig) `NFT_TPROXY` / `NETFILTER_XT_TARGET_TPROXY`；[上游源码] mihomo `Alpha` 与 sing-tun `meta` 全树 `grep -rn modprobe` 0 命中 |
| C5 | **策略路由是 TProxy 的必要条件**：`ip rule add fwmark 1 lookup 100` + `ip route add local 0.0.0.0/0 dev lo table 100`，配合监听 socket 上的 `IP_TRANSPARENT`；否则「非本地地址」的包无法被本地进程接收 | [上游文档] kernel `tproxy.rst` §1-2；[上游源码] `listener/tproxy/udp_linux.go`（`setsockopt(IP_TRANSPARENT)`）、`listener/tproxy/tproxy_iptables.go`（`ip -f inet rule add fwmark 0x2d0 lookup 0x2d0`） |
| C6 | **`route_localnet` 不是 TProxy 的必需项**，它只在「把 127/8 当合法路由地址」的场景（经典 REDIRECT-to-127.0.0.1）需要；内核默认 `FALSE`，且它本身是一个**安全降级开关**（允许 127/8 参与路由）。mihomo/sing-tun 源码**从不设置** `route_localnet` | [上游文档] kernel [`ip-sysctl.rst` `route_localnet`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/networking/ip-sysctl.rst)；[上游源码] mihomo/sing-tun 全树 `grep route_localnet` 0 命中；[实测-容器] 默认值 `0` |
| C7 | **TUN + `dns-hijack` 会改动系统 DNS 状态**：sing-tun 在 TUN 起来后若 `resolvectl` 存在，会执行 `resolvectl domain <tun> ~.` / `resolvectl default-route <tun> true` / `resolvectl dns <tun> <dns>`，关闭时 `resolvectl revert <tun>`。也就是说 **Mihomo 会通过 systemd-resolved 间接改写全局 DNS 分流**，进程被 `SIGKILL` 时 revert 不会执行 → 可能残留 | [上游源码] sing-tun `tun_linux.go:1038-1077` |
| C8 | **容器实测结论（OrbStack kernel 7.0.14-orbstack aarch64 / Debian 12）**：无 `CAP_NET_ADMIN` 时 nft 建表、`iptables` 建链、`ip rule`、`ip route`、`TUNSETIFF` **全部 `EPERM`**；`--privileged`（`CapEff=0x1ffffffffff`）时 nftables `tproxy to`、iptables(-nft/-legacy) `TPROXY`、`REDIRECT`、fwmark 策略路由、TUN 设备**全部创建成功并可清理**。`/proc/net/ip_tables_targets` 中已存在 `TPROXY`，说明该内核把 xt_TPROXY 编入 | [实测-容器] 见 §6 |
| C9 | **Mihomo 自带的 legacy iptables 自动化是「反面教材」，不应照抄**：`iptables: {enable: true}` + `tproxy-port` 会让 Mihomo 直接 `exec` 一串 `iptables`/`ip`/`sysctl` 命令，往**全局** `PREROUTING`/`OUTPUT` 追加跳转，硬编码 `172.17.0.0/16` 放行，且 setup 失败时 `os.Exit(2)`（整个 Mihomo 退出）；与 `tun.enable` 互斥 | [上游源码] `hub/executor/executor.go:463-534`、`listener/tproxy/tproxy_iptables.go` |
| C10 | **MVP 边界**：Mixed/HTTP/SOCKS 端口 + TUN 可选启用（含前提检测与降级）= `MVP Supported`；nftables/iptables/`/dev/net/tun`/`CAP_NET_ADMIN`/策略路由/`resolvectl`/规则残留的**只读检测** = `MVP Detection Only`；TProxy/redirect 规则自动生成与回滚 = `Later`；宿主网络改写、无权限强上、自动改 `/etc/resolv.conf`、Docker/K8s 自动接管 = `Unsupported` | 本文 §7 |

---

## 2. TUN 入站的前置条件（字段级）

字段清单以官方 wiki [Tun](https://wiki.metacubex.one/config/inbound/tun/) 为准，并与 [上游源码] `listener/config/tun.go`（YAML tag）和 `listener/inbound/tun.go`（`inbound:` tag）逐条对齐。**源码中存在但 wiki 未列出的字段已单独标注**。

### 2.1 核心字段

| 字段 | 官方语义（摘要） | Linux 前提 / 备注 | 证据 |
|---|---|---|---|
| `enable` | 启用 TUN | 需要 `/dev/net/tun` + `CAP_NET_ADMIN` | [上游文档] |
| `stack` | `system` / `gvisor` / `mixed`，默认 `gvisor`（wiki 建议 `mixed`） | `system`/`mixed` 的 TCP 走内核协议栈（占用系统资源最少、行为最接近原生）；`gvisor` 纯用户态，隔离性最好；Linux 上若开了主机防火墙可能需要放行 TUN 网卡出站（wiki 给出 `iptables -A OUTPUT -o <tun> -j ACCEPT` 的示例） | [上游文档]；[上游源码] `constant/tun.go`（默认枚举 0 = gVisor） |
| `device` | TUN 网卡名 | Linux 任意名；macOS 必须 `utun*` | [上游文档] |
| `auto-route` | 自动设置全局路由，把全局流量导入 TUN | 进程内 netlink 实现，**不需要 `ip` 命令**；需要 `CAP_NET_ADMIN`；默认路由表索引 `2022`、rule 起始索引 `9000`（见下） | [上游文档]；[上游源码] `listener/sing_tun/tun_linux.go`、`sing-tun/tun.go:69-71` |
| `auto-redirect` | **仅 Linux**，自动配置 iptables/nftables 以重定向 TCP 连接，**需要 `auto-route` 已启用** | 见 §3.3。默认 mark `auto-redirect-input-mark=0x2023` / `auto-redirect-output-mark=0x2024`，回退 rule index `32768` | [上游文档]；[上游源码] `listener/sing_tun/server.go`、`sing-tun/redirect.go:13-14` |
| `auto-detect-interface` | 自动选择出口接口（多出口设备建议手工指定） | 依赖接口监控（netlink route 事件）来跟随出口变化；`auto-redirect` 模式下另有 fallback rule index（32768）兜底 | [上游文档]；[上游源码] `sing-tun/tun_linux.go` rules() |
| `dns-hijack` | 把匹配的连接导入内置 DNS 模块，不写协议默认 `udp://`；示例 `any:53` / `tcp://any:53` | 解析时把 `any` 替换为 `0.0.0.0`；`<tun 地址>+1:53` 会被自动加入劫持目标。**副作用：触发 `resolvectl` 改写 systemd-resolved**（见 C7） | [上游文档]；[上游源码] `listener/sing_tun/server.go:276-300`、`listener/sing_tun/dns.go` |
| `strict-route` | 开启 `auto-route` 时执行严格路由 | Linux 上：让不支持的网络不可达 + 把所有连接路由到 TUN；实现为「对未启用协议族插入 `FR_ACT_UNREACHABLE` 规则」+ nftables reject 规则 | [上游文档]；[上游源码] `sing-tun/tun_linux.go:773-790`、`redirect_nftables_rules.go` `nftablesCreateUnreachable()` |
| `mtu` | 最大传输单元 | 源码默认值 `9000` | [上游源码] `listener/sing_tun/server.go:194` |
| `gso` / `gso-max-size` | 通用分段卸载 / 数据块最大长度 | **仅 Linux**；依赖 TUN offload 能力，探测失败会降级并告警 | [上游文档]；[上游源码] `sing-tun/tun_offload_linux.go` |
| `inet4-address` / `inet6-address` | 指定 TUN 地址 | IPv6 生效还需顶层 `ipv6: true`；启动时若系统无 IPv6 会禁用（可用 `SKIP_SYSTEM_IPV6_CHECK=1` 强制）；**`inet4-address` 决定 `auto-route` 是否启用 IPv4** | [上游文档]；[上游源码] `redirect_linux.go`（`enableIPv4/enableIPv6`） |
| `iproute2-table-index` | `auto-route` 生成的路由表索引，默认 `2022` | 源码中若为 `0` 会**随机挑一个空闲表号**；不是「不建表」 | [上游文档]；[上游源码] `sing-tun/tun.go:69`、`tun_linux.go:385-395` |
| `iproute2-rule-index` | `auto-route` 生成的 rule 起始索引，默认 `9000` | 实际会按 family/mark/uid 顺序递增占用多个 priority；`auto-redirect` 模式下还会用 `auto-redirect-iproute2-fallback-rule-index`（默认 `32768`）插到 `main`/`default` 之后 | [上游文档]；[上游源码] `sing-tun/tun_linux.go:496-600` |
| `route-address` / `route-exclude-address` | 自定义进 TUN 的网段 / 排除网段 | 与 `auto-route` 配合；排除项通过 IPSet 差集实现 | [上游文档]；[上游源码] `sing-tun/tun_rules.go BuildAutoRouteRanges()` |
| `route-address-set` / `route-exclude-address-set` | 用 rule-set 的 CIDR 动态生成防火墙集合 | **仅 Linux 且需要 nftables** + `auto-route` + `auto-redirect`；会切到 `AutoRedirectMarkMode`；官方明确「与任意配置中的 `routing-mark` 冲突」 | [上游文档]；[上游源码] `listener/sing_tun/server.go:449-471` |
| `include-interface` / `exclude-interface` | 限制/排除被路由的接口 | 二者互斥，不可同时配置；影响 nftables 规则与 output chain 是否生成（`lo` 在 include 里才生成 output 链） | [上游文档]；[上游源码] `redirect_nftables.go:54` |
| `include-uid` / `include-uid-range` / `exclude-uid` / `exclude-uid-range` | 按 UID 限制 | **仅 Linux 且需要 `auto-route`**；实现为 netlink rule 的 uidrange 与 nftables `MetaKeySKUID` 集合 | [上游文档]；[上游源码] `sing-tun/tun_linux.go`、`redirect_nftables_rules.go` |
| `include-mac-address` / `exclude-mac-address` | 按来源 MAC 限制局域网设备 | **仅 Linux**，需要 `auto-route` + `auto-redirect` | [上游文档] |
| `endpoint-independent-nat` | 端点无关 NAT | 性能略降，非必要不开 | [上游文档] |
| `udp-timeout` | UDP NAT 过期时间，默认 `300` 秒 | — | [上游文档] |
| `include-android-user` / `include-package` / `exclude-package` | Android 用户/包名 | Android-only | [上游文档] |
| `inet4-route-address` / `inet6-route-address` / `inet4-route-exclude-address` / `inet6-route-exclude-address` | 旧写法，即将废弃 | 仍被源码解析 | [上游文档]；[上游源码] `listener/config/tun.go` |

### 2.2 源码存在但 wiki 未列出的字段 `[上游源码]`

| 字段 | 含义 | 备注 |
|---|---|---|
| `auto-redirect-input-mark` | `auto-redirect` 输入 mark，默认 `0x2023` | 用于 UDP/ICMP 的 mark 路由与防回环 |
| `auto-redirect-output-mark` | `auto-redirect` 输出 mark，默认 `0x2024` | — |
| `auto-redirect-iproute2-fallback-rule-index` | 回退 rule 索引，默认 `32768` | 注释写明「在系统默认规则（32766 main / 32767 default）之后」 |
| `loopback-address` | TUN 环回地址 | 触发 `inet4_local_redirect_address_set` 与额外的 route-hook 链 |
| `exclude-src-port` / `exclude-src-port-range` / `exclude-dst-port` / `exclude-dst-port-range` | 按端口排除 | 实现为 rule 的 sport/dport 范围 + `goto` |
| `icmp-timeout` / `disable-icmp-forwarding` | ICMP 相关 | — |
| `file-descriptor` | 外部传入已打开的 TUN fd | 由宿主/上层托管设备时使用 |
| `recvmsgx` / `sendmsgx` | **darwin 专用** | — |
| `processors-per-channel` | gvisor 内部参数，官方注明「非公开选项，不要写进文档」 | — |

### 2.3 TUN 的设备与权限前提（Linux）

```text
1) 设备节点：/dev/net/tun  (char major 10, minor 200)
   - 普通发行版：由内核/udev 或 systemd-tmpfiles 提供
   - PVE 非特权 LXC（官方 wiki 做法）：
       lxc.cgroup2.devices.allow: c 10:200 rwm
       lxc.mount.entry: /dev/net dev/net none bind,create=dir
       宿主上 chown 100000:100000 /dev/net/tun   # 非特权容器 UID 映射
     并且创建容器时建议 features: nesting=1
2) capability：CAP_NET_ADMIN
   - 创建 TUN 设备（TUNSETIFF）、写路由表、写 rule、写 nftables 都需要它
   - CAP_NET_RAW：mihomo 在部分原始包/ICMP 路径上可能使用（sing-tun 源码中 TCP/UDP listener 走的是普通 socket + IP_TRANSPARENT，未见到显式要求 CAP_NET_RAW 的代码路径）[推测]
3) 无外部二进制依赖：sing-tun 在 Linux 上走 netlink（netlink.RuleList / RouteAdd / nftables），
   不调用 ip / nft / iptables；唯一的例外是 resolvectl（DNS）与 iptables 回退路径
```

[上游文档] [Proxmox VE Wiki: OpenVPN in LXC](https://pve.proxmox.com/wiki/OpenVPN_in_LXC)（非特权容器 TUN 的标准写法）、[Proxmox VE Wiki: Linux Container](https://pve.proxmox.com/wiki/Linux_Container)（`features: nesting=1`、非特权容器默认开启）；
[上游源码] `sing-tun/tun_linux.go`（netlink）、`sing-tun/tun_linux.go:1042`（`exec.LookPath("resolvectl")`）；
[实测-容器] 见 §6：非特权下 `mknod /dev/net/tun c 10 200` 可以成功（Docker 默认含 `CAP_MKNOD`），但 `ip tuntap add` 报 `ioctl(TUNSETIFF): Operation not permitted`。

---

## 3. TProxy / redirect 的实现方式与前置条件

### 3.1 三种「透明代理」机制必须区分清楚

| 机制 | 谁写防火墙规则 | 传输层能力 | 依赖 |
|---|---|---|---|
| **A. TProxy 入站**（`tproxy-port` / `listeners.type=tproxy`） | **外部**（运维/脚本/本 Agent） | TCP + UDP | `xt_TPROXY` 或 `nft_tproxy` + `xt_socket`/`nft_socket` + 策略路由 + `IP_TRANSPARENT` |
| **B. redirect 入站**（`redir-port` / `listeners.type=redir`） | **外部** | **仅 TCP** | NAT `REDIRECT` 规则 + `SO_ORIGINAL_DST`（内核 conntrack） |
| **C. TUN + `auto-redirect`** | **Mihomo 自己**（进程内 nftables，可回退 iptables） | TCP（redirect）+ UDP/ICMP（mark→TUN）+ DNS（DNAT） | 仅 `CAP_NET_ADMIN`；内核 nf_tables（或回退到 iptables 二进制） |
| D. legacy `iptables: {enable: true}` + `tproxy-port` | **Mihomo 自己**（exec `iptables`/`ip`/`sysctl`） | TCP + UDP | 外部 `iptables`/`ip` 二进制，写**全局**规则 |

[上游文档] [Mihomo TProxy listener](https://wiki.metacubex.one/config/inbound/listeners/tproxy/)、[REDIRECT listener](https://wiki.metacubex.one/config/inbound/listeners/redirect/)、[代理端口](https://wiki.metacubex.one/config/inbound/port/)（「redirect 端口仅限 Linux(Android) 以及 macOS 适用，tproxy 端口仅限 Linux(Android) 适用」「redirect 透明代理端口，仅能代理 TCP 流量；tproxy 透明代理端口，可代理 TCP 与 UDP 流量」）；
[上游源码] `listener/redir/tcp_linux.go`（`SO_ORIGINAL_DST = 80` / `IP6T_SO_ORIGINAL_DST = 80`，仅 TCP）、`listener/tproxy/udp_linux.go`（`syscall.SetsockoptInt(fd, syscall.SOL_IP, syscall.IP_TRANSPARENT, 1)`）。

> **重要语义澄清**：官方 wiki 的 TProxy 页面**只有监听配置，没有任何规则示例**；「透明代理规则」在 Mihomo 侧要么由用户自备，要么走 §3.4 的 legacy 自动化。因此本节的规则示例来自 **kernel 官方文档**（nft/iptables 各一版）与 **Mihomo 自己的源码**（iptables 版），不来自 Mihomo wiki。

### 3.2 官方规则原文

**（1）kernel 官方 nftables 版**（[Documentation/networking/tproxy.rst](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/networking/tproxy.rst)）：

```sh
# 1. 让「目的地址属于本机 socket」的包被 mark 并放行（socket match）
nft add table filter
nft add chain filter divert "{ type filter hook prerouting priority -150; }"
nft add rule filter divert meta l4proto tcp socket transparent 1 meta mark set 1 accept

# 2. 策略路由：把带 mark 的包投递给本地
ip rule add fwmark 1 lookup 100
ip route add local 0.0.0.0/0 dev lo table 100

# 3. TPROXY 目标（注意：只写规则，不改包）
nft add rule filter divert tcp dport 80 tproxy to :50080 meta mark set 1 accept
```

同文档给出的 iptables 等价版：

```sh
iptables -t mangle -N DIVERT
iptables -t mangle -A PREROUTING -p tcp -m socket --transparent -j DIVERT
iptables -t mangle -A DIVERT -j MARK --set-mark 1
iptables -t mangle -A DIVERT -j ACCEPT

iptables -t mangle -A PREROUTING -p tcp --dport 80 -j TPROXY \
  --tproxy-mark 0x1/0x1 --on-port 50080
```

文档同时强调：**代理进程必须对监听 socket 开启 `(SOL_IP, IP_TRANSPARENT)`**，否则非本地地址 bind 失败。

**（2）Mihomo 自带的 iptables 版**（[上游源码] `listener/tproxy/tproxy_iptables.go`，节选、`PROXY_FWMARK = PROXY_ROUTE_TABLE = 0x2d0`）：

```sh
ip -f inet rule add fwmark 0x2d0 lookup 0x2d0
ip -f inet route add local default dev <inbound-interface> table 0x2d0

iptables -t mangle -N mihomo_divert
iptables -t mangle -A mihomo_divert -j MARK --set-mark 0x2d0
iptables -t mangle -A mihomo_divert -j ACCEPT

iptables -t mangle -N mihomo_prerouting
iptables -t mangle -A mihomo_prerouting -s 172.17.0.0/16 -j RETURN
iptables -t mangle -A mihomo_prerouting -m addrtype --dst-type LOCAL -j RETURN
# ... addLocalnetworkToChain(): 0.0.0.0/8, 10/8, 127/8, 169.254/16, 172.16/12, 192.168/16 ... -j RETURN
iptables -t mangle -A mihomo_prerouting -p tcp -m socket -j mihomo_divert
iptables -t mangle -A mihomo_prerouting -p udp -m socket -j mihomo_divert
iptables -t mangle -A mihomo_prerouting -p tcp -j TPROXY --on-port <tproxy-port> --tproxy-mark 0x2d0/0x2d0
iptables -t mangle -A mihomo_prerouting -p udp -j TPROXY --on-port <tproxy-port> --tproxy-mark 0x2d0/0x2d0
iptables -t mangle -A PREROUTING -j mihomo_prerouting          # ← 追加到全局链

iptables -t nat -I PREROUTING ! -s 172.17.0.0/16 ! -d 127.0.0.0/8 -p tcp --dport 53 -j REDIRECT --to <dns-port>
iptables -t nat -I PREROUTING ! -s 172.17.0.0/16 ! -d 127.0.0.0/8 -p udp --dport 53 -j REDIRECT --to <dns-port>
```

**（3）实测可用的最小核对清单**（[实测-容器]，已在 `--privileged` Debian 12 容器验证语法与内核支持）：

```sh
nft add table inet r11test
nft 'add chain inet r11test prerouting { type filter hook prerouting priority mangle; policy accept; }'
nft 'add rule inet r11test prerouting meta l4proto tcp tproxy to :17892'
nft 'add rule inet r11test prerouting meta l4proto udp tproxy to :17892'

iptables -t mangle -N R11TEST
iptables -t mangle -A R11TEST -p tcp -j TPROXY --on-port 17892 --tproxy-mark 0x1/0xffffffff
# 实测：nft 允许 `tproxy to :PORT` 与 `tproxy ip to :PORT` 两种写法；iptables-nft 回显为
#   -A R11TEST -p tcp -j TPROXY --on-port 17892 --on-ip 0.0.0.0 --tproxy-mark 0x1/0xffffffff
```

> **[实测-容器] 一个容易踩的语法坑**：在 `inet` family 里做 DNS DNAT 时必须显式写协议族：
> `nft 'add rule inet r11mihomo prerouting tcp dport 53 dnat to 198.18.0.2:53'`
> → `Error: ip or ip6 must be specified with address for inet tables.`
> 正确写法是 `dnat ip to 198.18.0.2:53`。Mihomo 自己生成 nftables 时是构造 netlink 表达式并显式带 `Family`，所以不受此限制（[上游源码] `redirect_nftables_rules.go` `nftablesCreateDNSHijackRulesForFamily`）。

### 3.3 `auto-redirect` 的实现（上游源码级）

[上游源码] `metacubex/sing-tun@meta` + mihomo `listener/sing_tun/server.go`：

```text
NewAutoRedirect(...)          # 仅 Linux：supportRedirect = true
├─ useNFTables = (GOOS != android) && !DISABLE_NFTABLES
├─ initializeNFTables(): nft.ListTablesOfFamily(IPv4)   ← 探测内核是否支持 nf_tables
│    失败 → useNFTables=false，回退 exec.LookPath("iptables") / ("ip6tables")
├─ Start():
│    若未提供 customRedirectPort：起一个内部 redirect server（REDIR listener），
│    端口由内核分配（IPv6 启用时 bind ::，否则 0.0.0.0）
│    nftables 路径：先 cleanupNFTables() 再 setupNFTables()（幂等重建）
└─ Close(): cleanupNFTables() / cleanupIPTables()
```

nftables 结构与语义（[上游源码] `redirect_nftables.go`、`redirect_nftables_rules.go`）：

```text
table inet mihomo                      # TableName 固定为 "mihomo"
├─ set inet4_local_address_set / inet6_local_address_set        # 本机所有地址 + lo（接口变化时增量更新）
├─ set inet4_route_address_set / inet6_route_address_set        # route-address-set（flags interval）
├─ set inet4_route_exclude_address_set / inet6_route_exclude_address_set
├─ set inet4_local_redirect_address_set / inet6_local_redirect_address_set
├─ chain output           (hook output,  priority mangle, type nat)
├─ chain output_route     (hook output,  priority mangle, type route)   # 仅 loopback-address / mark mode
├─ chain output_udp_icmp  (hook output,  priority mangle, type route)
├─ chain prerouting       (hook prerouting, priority dstnat+1, type nat)
├─ chain prerouting_filter(hook prerouting, priority dstnat+1, type filter)
└─ chain prerouting_udp_icmp (hook prerouting, priority dstnat+2, type filter)
```

关键行为：

1. **TCP**：`meta l4proto tcp … redirect to :<redirectPort>`（NAT REDIRECT，不是 TPROXY）——因为 redirect 只能改 TCP，UDP 无法保留原目的地址（kernel 文档亦如此说明）。
2. **UDP / ICMP**：不 redirect，而是打 mark（`0x2023`/`0x2024`）后由 `auto-route` 的 rule 把它们送进 TUN，由 gvisor/system 协议栈处理。
3. **DNS 劫持**：在 `prerouting`（type nat）里匹配 `dport 53` 且目的地址命中 `inet4_local_address_set`（本机地址）时 DNAT 到内置 DNS（`<tun 地址>+1:53` 或 `DNSServers`）。
4. **防回环**：`iifname <tun>` return；output 方向 `oifname lo` return；mark 命中即 return。
5. **`strict-route`**：对未启用协议族插入 reject 规则（`nftablesCreateUnreachable`）。
6. **OpenWRT 特殊处理**：`redirect_nftables_rules_openwrt.go` 会在检测到 `fw4` 时注入兼容配置（[上游源码] `exec.LookPath("fw4")`）。
7. **清理**：`cleanupNFTables()` → `nft.DelTable(inet mihomo)`，即**整表删除**，不做逐条回收——这是 Mihomo 自己的「幂等重建」策略，也说明 `table inet mihomo` 是 Mihomo 的独占命名空间。

**MVP 相关性**：`auto-redirect` 已经能覆盖「Linux 上落单机全流量透明代理」的大部分需求，而且**不需要 Agent 写任何防火墙规则**（规则由 Mihomo 进程自己维护、退出即删表）。因此对 Agent 来说，推荐姿态是「检测 + 由用户配置开启」，而不是自己造一套规则。

### 3.4 legacy 自动 iptables（`iptables:` 顶层配置）——风险清单

[上游源码] `config/config.go:347` `RawIPTables`：

```yaml
iptables:
  enable: true
  inbound-interface: lo        # 默认 lo
  bypass: ["192.168.0.0/16"]   # 默认空
  dns-redirect: false
tproxy-port: 7894
```

[上游源码] `hub/executor/executor.go:463-534` 的行为：

| 行为 | 后果 |
|---|---|
| `tun.enable == true` 时报错 `when tun is enabled, iptables cannot be set automatically` | 与 TUN 互斥 |
| `tproxy-port == 0` 时报错 | 必须显式给端口 |
| `dns-redirect: true` 时要求 DNS 已启用且 `dns.listen` 可解析 | 需要 DNS server |
| setup 失败 → `log.Errorln(...)` + **`os.Exit(2)`** | **整个 Mihomo 进程退出**，与「能力不可用必须降级」的项目不变量直接冲突 |
| `dialer.DefaultRoutingMark.CompareAndSwap(0, 2158)`（`0x86E`） | 改动全局 dialer 行为 |
| 硬编码 `172.17.0.0/16`（docker0）RETURN | 假设了 Docker 默认网段 |
| 往全局 `PREROUTING`/`OUTPUT` **追加/插入**跳转 | 与其他防火墙管理器（ufw/firewalld/docker/kubernetes/k3s）共存风险高 |
| `Shutdown()` 与每次配置更新都会 `CleanupTProxyIPTables()` | 正常退出能清理；`SIGKILL`/断电/崩溃不能 |

> 结论：**Agent 不能复用这条路径**。它是 Mihomo 为「单机一次性接管」设计的，缺少 dry-run、快照、回滚与冲突检测。

---

## 4. DNS 拦截与端口冲突

### 4.1 `dns-hijack` 的语义（源码级）

[上游源码] `listener/sing_tun/server.go:276-300`：

```go
for _, d := range options.DNSHijack {
    if _, after, ok := strings.Cut(d, "://"); ok { d = after }   // 协议前缀仅用于书写，实际全导入 DNS 模块
    d = strings.Replace(d, "any", "0.0.0.0", 1)
    addrPort, _ := netip.ParseAddrPort(d)
    dnsAdds = append(dnsAdds, addrPort)
}
// 另外总是追加 <inet4-address 的第一个地址 + 1>:53 与 <inet6-address + 1>:53
```

[上游源码] `listener/sing_tun/dns.go`：

```go
func (h *ListenerHandler) ShouldHijackDns(targetAddr netip.AddrPort) bool {
    for _, addrPort := range h.DnsAddrPorts {
        if addrPort == targetAddr || (addrPort.Addr().IsUnspecified() && targetAddr.Port() == 53) {
            return true
        }
    }
    return false
}
```

⇒ 写 `any:53` 时 `addrPort.Addr()` 是 `0.0.0.0`（unspecified），于是**任何目的地址、目的端口 53 的流量都会被劫持**进内置 DNS；`tcp://any:53` 与 `any:53` 在解析后等价（协议前缀被丢弃），因此想同时覆盖 TCP/UDP 需要**两条都写**（wiki 示例即如此）。

### 4.2 `fake-ip` / `respect-rules`

| 项 | 事实 | 证据 |
|---|---|---|
| `enhanced-mode` | `fake-ip` / `redir-host`，**默认 `redir-host`** | [上游文档] [DNS 配置](https://wiki.metacubex.one/config/dns/) |
| `fake-ip-range` | 默认示例 `198.18.0.1/16`；**TUN 的默认 IPv4 地址也参考该值** | [上游文档] |
| `fake-ip-filter` / `fake-ip-filter-mode` | `blacklist`/`whitelist`/`rule`（`rule` 模式下语法与路由 rules 一致，支持 GEOSITE/RULE-SET） | [上游文档] |
| `respect-rules` | DNS 连接遵守路由规则；**必须同时配置 `proxy-server-nameserver`**，否则配置解析直接报错（源码强制校验）；wiki 明确「强烈不建议和 `prefer-h3` 一起使用」 | [上游源码] `config/config.go:1419-1421`；[上游文档] |
| `listen` | DNS server 监听（支持 udp/tcp）；还有 `listen-routing-mark` | [上游源码] `config/config.go` `DNS` 结构 |
| `ipv6: false` | 回应空 AAAA 解析 | [上游文档] |

### 4.3 端口冲突（53 vs systemd-resolved vs Docker）

| 场景 | 冲突 | 处理建议 | 证据 |
|---|---|---|---|
| `dns.listen: 0.0.0.0:53` | systemd-resolved 的 stub listener 默认监听 **127.0.0.53:53 与 127.0.0.54:53（TCP+UDP）**，`bind(0.0.0.0:53)` 会 `EADDRINUSE` | 三选一：① DNS 用非 53 端口（如 `0.0.0.0:1053`）由 TUN `dns-hijack` 接管；② `DNSStubListener=no`（需重启 systemd-resolved，属于**系统级修改**，应由用户显式执行）；③ 只监听 `127.0.0.1:53` 并接受全局不可见 | [上游文档] [systemd `resolved.conf`](https://www.freedesktop.org/software/systemd/man/latest/resolved.conf.html)：`DNSStubListener=`「If "yes" (the default), the stub listens for both UDP and TCP requests」、地址 `127.0.0.53`/`127.0.0.54` 端口 53 |
| `/etc/resolv.conf` 指向 stub | TUN 模式下若 `resolvectl` 缺失（容器）或未生效，系统解析仍走 127.0.0.53，可能绕过 Mihomo DNS | doctor 应检测 `resolvectl` 是否存在 + `/etc/resolv.conf` 是否指向 stub；不可自动改写 | [上游源码] sing-tun `tun_linux.go:1042`（`LookPath` 失败即静默跳过）；[推测] |
| `dns-redirect: true`（legacy iptables） | 用 `REDIRECT --to <dns-port>` 抢 53 | 不推荐在 MVP 使用 | [上游源码] `tproxy_iptables.go:75-78` |
| Docker 内置 DNS (127.0.0.11) | 容器内 `resolv.conf` 常指向 127.0.0.11，TUN + `dns-hijack any:53` 在 `iifname <tun>` 之外的路径上会先命中 Docker 的 DNAT | 不承诺在 Docker 容器内做全透明接管 | [推测] |
| 已存在其他 53 监听（dnsmasq/AdGuardHome/Pi-hole） | `bind` 失败 → Mihomo DNS 起不来 | doctor 只读检测 `ss -lntup`，提示冲突方 | [推测] |

### 4.4 TUN 对系统 DNS 的副作用（必须记录）

[上游源码] sing-tun `tun_linux.go:1038-1077`：

```go
func (t *NativeTun) setSearchDomainForSystemdResolved() {
    if t.options.EXP_DisableDNSHijack { return }
    ctlPath, err := exec.LookPath("resolvectl"); if err != nil { return }   // 不存在就静默跳过
    ...
    go func() {
        _ = shell.Exec(ctlPath, "domain", t.options.Name, "~.").Run()
        _ = shell.Exec(ctlPath, "default-route", t.options.Name, "true").Run()
        _ = shell.Exec(ctlPath, "dns", t.options.Name, dnsServer...).Run()
    }()
}
func (t *NativeTun) unsetSearchDomainForSystemdResolved() {   // Close 时
    _ = shell.Exec(ctlPath, "revert", t.options.Name).Run()
}
```

⇒ **TUN + `dns-hijack` 会写入 systemd-resolved 的 per-link DNS 配置**（默认路由 + `~.` 搜索域 + DNS 服务器）。这是 Mihomo 侧的系统级副作用，Agent 的 doctor 必须能观测到（`resolvectl status <tun>`），并在 Mihomo 异常退出后提供**只读诊断**（是否残留 `~.` 默认路由）。`resolvectl revert` 是唯一的回退，且只在优雅关闭时执行。

---

## 5. 策略路由要点

### 5.1 fwmark + `ip rule` + local route（TPROXY 的骨架）

```sh
ip rule add fwmark 1 lookup 100
ip route add local 0.0.0.0/0 dev lo table 100
```

- 作用：把「被 mark 的包」从正常转发路径改写为「投递给本机 socket」，从而让代理进程能收到目的地址并非本机的连接。
- 内核文档对「非本地地址 socket」的要求：代理需要在监听/发送 socket 上开 `IP_TRANSPARENT`（`setsockopt(fd, SOL_IP, IP_TRANSPARENT, 1)`）。
- [实测-容器] 在 `--privileged` Debian 12 中：`ip rule add fwmark 0x2d0 lookup 0x2d0` 成功，自动落到 **preference 32765**（在 `main` 32766 之前）；`ip route add local default dev lo table 0x2d0` 成功，表内显示 `local default dev lo scope host`；反之无 `CAP_NET_ADMIN` 时全部 `RTNETLINK answers: Operation not permitted`。

### 5.2 `route_localnet` 的用途与风险

| 项 | 内容 |
|---|---|
| 官方定义 | kernel `ip-sysctl.rst`：`route_localnet - BOOLEAN. Do not consider loopback addresses as martian source or destination while routing. This enables the use of 127/8 for local routing purposes. default FALSE` |
| 何时需要 | 当你用 NAT `REDIRECT`/`DNAT` 把流量改成 **127.0.0.1**（或从 127/8 发包）时，路由层默认会把 127/8 当 martian 丢弃 |
| TProxy 是否需要 | **不需要**。TPROXY 不改包，配合 `ip route add local … dev lo` 即可 |
| Mihomo 是否设置 | **从不设置**（mihomo 与 sing-tun 全树 0 命中） |
| 风险 | 一旦打开，127/8 变成可路由地址：① 可能成为绕过本机访问控制/SSRF 的通道；② 与其他依赖「127/8 不可路由」假设的安全策略冲突；③ 属**全局 sysctl**（per-netns），影响面大于单条规则 |
| 建议 | MVP：**只读检测 + 在规则模板里默认不使用**；若 Later 要生成 `REDIRECT`-to-localhost 规则，必须先显式警告并支持一键恢复 |

### 5.3 `auto-route` 生成的 rule 结构（上游源码级，供 doctor 解释现状）

```text
priority 9000 + n  (iproute2-rule-index 起)
├─ [auto-redirect mark mode] mark == outputMark(0x2024) → goto 9002   # 经代理出去的流量不再被劫持
├─ [auto-redirect mark mode] mark == inputMark(0x2023)  → lookup 2022 # 劫持进来的流量
├─ [uid/port 排除] sport/dport 匹配 → goto nopPriority(ruleStart+10)
├─ [uidrange] skipuid / 用户范围
├─ [strict-route] 未启用协议族插入 FR_ACT_UNREACHABLE
├─ dst == <tun 地址> → table 2022
├─ iif lo / oif <tun> …
└─ priority 32768: lookup 2022   (auto-redirect fallback，位于 main 32766 / default 32767 之后)
```

[上游源码] `sing-tun/tun_linux.go:496-800`、`sing-tun/tun.go:69-71`。

对 Agent 的含义：
- `2032/9000/32768` 这些数字是「Mihomo 的领地」，doctor 不应把它们当作异常；
- 若系统上已存在 `fwmark` 规则或 2022 表，说明可能残留了上一次未清理的 TUN 会话 —— 这是**只读诊断项**；
- `route-address-set` 参与者与 `routing-mark`（顶层 `routing-mark`）**官方明确冲突**，配置校验应拦截。

---

## 6. 容器实测记录（明确局限）

### 6.1 环境与方法

| 项 | 值 |
|---|---|
| 宿主 | macOS (Darwin, arm64)，OrbStack |
| 容器内核 | `Linux 7.0.14-orbstack-00380-ga7e0a2dc9535 #1 SMP PREEMPT … aarch64`（实测自容器内 `uname -a`） |
| 镜像 | `agentscope/copaw:latest`（本地已有，`Debian GNU/Linux 12 (bookworm)`）→ `apt-get install nftables iptables iproute2 kmod` → `docker commit r11-net:local` |
| 工具版本 | `nftables v1.0.6 (Lester Gooch #5)`、`iptables v1.8.9 (nf_tables)`、`iproute2-6.1.0` |
| 两次运行 | ① 默认（非特权）`docker run --rm`；② `docker run --rm --privileged` |
| 网络 | 未使用 `--network host`；未做任何真实流量代理实验（安全红线） |
| 清理 | 容器全部 `--rm`；自建 nft table / iptables chain / `ip rule` / 路由表 / TUN 设备在脚本内逐个删除并复查；`/tmp/r11-net/` 已删除 |

> **为什么这里可以放心测试**：容器运行在独立 network namespace 中，容器内的路由表、nftables、iptables、TUN 设备对宿主 macOS 不可见；`--privileged` 仅在容器 netns 内放大 capability。全程没有 `iptables -F`、没有触碰容器内既有规则（只建了自己命名的 `R11TEST`/`R11LEG`/`R11NFT`/`R11NAT` 链与 `r11test`/`r11mihomo` 表）。`sysctl -w net.ipv4.conf.all.route_localnet=1` 的写入也是 netns 局部的，且容器随 `--rm` 销毁。

### 6.2 非特权容器（默认 Docker capabilities）

`CapEff = 00000000a80425fb`（**不含 `CAP_NET_ADMIN`**，bit12 未置位；含 `CAP_NET_RAW`/`CAP_MKNOD`）。

| 操作 | 结果 | 原文 |
|---|---|---|
| `nft`, `iptables`, `ip6tables`, `ip`, `ss`, `capsh`, `lsmod` | 存在（安装后） | `nftables v1.0.6`、`iptables v1.8.9 (nf_tables)` |
| `update-alternatives --display iptables` | 默认指向 **iptables-nft** | `link best version is /usr/sbin/iptables-nft`（legacy priority 10 < nft priority 20） |
| `/dev/net/tun` | 不存在；`mknod /dev/net/tun c 10 200` **成功** | `crw-r--r-- 1 root root 10, 200` |
| `ip tuntap add dev r11tun0 mode tun` | 失败 | `ioctl(TUNSETIFF): Operation not permitted` |
| `nft add table inet r11test` | 失败 | `Error: Could not process rule: Operation not permitted` |
| `nft list tables` / `list ruleset` | 失败 | `Operation not permitted (you must be root)` + `netlink: Error: cache initialization failed` |
| `iptables -t mangle -N R11TEST` | 失败（exit 4） | `iptables v1.8.9 (nf_tables): Could not fetch rule set generation id: Permission denied (you must be root)` |
| `iptables-legacy -t mangle -N R11LEG` | 失败（exit 3） | `iptables v1.8.9 (legacy): can't initialize iptables table 'mangle': Permission denied (you must be root)` |
| `ip rule add fwmark 1 lookup 100` | 失败 | `RTNETLINK answers: Operation not permitted` |
| `ip route add local default dev lo table 100` | 失败 | `RTNETLINK answers: Operation not permitted` |
| `sysctl -w net.ipv4.conf.all.route_localnet=1` | 失败 | `sysctl: permission denied on key "net.ipv4.conf.all.route_localnet"` |
| `read /proc/net/ip_tables_targets` | **成功** | 内含 `TPROXY` ×2、`REDIRECT`、`MARK`、`CONNMARK`、`SET`、`MASQUERADE` … |
| `read /proc/net/ip_tables_matches` | **成功** | 内含 `socket`、`addrtype`…（`socket` 出现 4 次） |
| `/proc/modules` | 空（0 行） | `lsmod` 只有表头 ⇒ OrbStack 内核**无 loadable modules**，全部内建 |
| `/sys/module` | 81 项；相关项仅 `nf_conntrack`、`xt_recent`、`8021q` | 没有 `nf_tables`/`nft_tproxy`/`nf_tproxy_ipv4`/`tun` 目录（内建模块不出现在 `/sys/module`） |
| `cat /proc/1/cgroup` | `0::/`；`/.dockerenv` 存在；`systemd-detect-virt` = `docker` | 容器身份可判定 |

### 6.3 特权容器（`--privileged`，`CapEff = 000001ffffffffff`）

| 操作 | 结果 | 原文/备注 |
|---|---|---|
| nft `add table inet r11test` + `chain {type filter hook prerouting priority mangle}` | **成功** | — |
| nft `tproxy to :17892`（TCP）/ `tproxy ip to :17892` / UDP 版 | **成功** | 回显：`meta l4proto tcp tproxy to :17892` |
| iptables-nft `-t mangle -N` + `-m socket -j MARK` + `-j TPROXY --on-port --tproxy-mark` | **成功** | `-A R11TEST -p tcp -j TPROXY --on-port 17892 --on-ip 0.0.0.0 --tproxy-mark 0x1/0xffffffff` |
| iptables-legacy 同样三项 | **成功** | `-A R11LEG -p tcp -j TPROXY --on-port 17892 --on-ip 0.0.0.0 --tproxy-mark 0x2d0/0x2d0` |
| iptables-nft `-t nat -N` + `-j REDIRECT --to-ports 17893` | **成功** | redirect 模式最小集 |
| nft mihomo 风格子集：`table inet r11mihomo` + `chain prerouting {type nat hook prerouting priority dstnat + 1}` + `chain output {type nat hook output priority mangle}` + `chain output_route {type route hook output priority mangle}` + `meta l4proto tcp redirect to :17893` + `iifname "r11tun0" return` + `oifname "lo" return` + `set inet4_local_address_set {type ipv4_addr; flags interval}` + `ip daddr @set return` + `meta mark set 0x2023 ct mark set meta mark` | **全部成功** | 证明 §3.3 描述的结构在本内核上可落地 |
| nft `tcp dport 53 dnat to 198.18.0.2:53`（inet family，未写 `ip`） | **失败** | `Error: ip or ip6 must be specified with address for inet tables.` |
| nft 在 `type nat` 链里加 `tproxy` | 语法接受（exit 0） | 语义是否按预期工作**未验证**（tproxy 需要早于 conntrack 的 hook；无真实流量测试） |
| `sysctl -w net.ipv4.conf.all.route_localnet=1` | **成功** | `net.ipv4.conf.all.route_localnet = 1`（默认 0） |
| `ip rule add fwmark 0x2d0 lookup 0x2d0` | **成功** | `32765: from all fwmark 0x2d0 lookup 720` |
| `ip -f inet route add local default dev lo table 0x2d0` | **成功** | `local default dev lo scope host` |
| `ip tuntap add dev r11tun0 mode tun` | **成功** | `r11tun0 DOWN <POINTOPOINT,MULTICAST,NOARP>`；`ip tuntap del` 也成功 |
| `/dev/net/tun` | **已存在**（`crw-rw-rw- 10,200`） | `--privileged` 会挂载宿主 `/dev`，因此不需要 mknod |

**清理与残留复查**：

| 项 | 结果 |
|---|---|
| `nft delete table inet r11test` / `inet r11mihomo` | exit 0，`nft list tables` 中消失 |
| `iptables -t mangle -F/-X R11TEST` 等 | exit 0 |
| **残留 1**：`nft list tables` 仍显示 `table ip mangle`、`table ip nat` | 这是 **iptables-nft 兼容层表**：只要用 `iptables` 操作过 `mangle`/`nat`，即使把自己的链删光，空的 `ip mangle`/`ip nat` 表仍留在 nftables 里（`iptables-save` 显示基链全 ACCEPT、无规则）。**这是「可回滚」设计必须处理的残留语义** |
| **残留 2**：`ip rule`/路由表/TUN | 已删除并复查（`ip rule show` 恢复为 0/32766/32767 三行；表 0x2d0 已 flush） |
| 容器 | 全部 `--rm` 销毁，netns 随之消失 |

### 6.4 局限（**这不是 PVE 证据**）

1. **容器证据 ≠ PVE LXC 证据**。Docker 默认 seccomp/AppArmor/capability 集合与 PVE 的 `lxc.cap.drop`/非特权 UID 映射/AppArmor profile 不同。PVE 非特权 LXC 中 `CAP_NET_ADMIN` 是否存在，必须在真机 `doctor` 上实测。
2. **没有真机发行版**：只测了 Debian 12；Ubuntu 24.04 / Debian 13 的 `iptables` 默认 alternative、nftables 版本差异未验证。
3. **没有 systemd**：容器内无 systemd-resolved、无 `resolvectl`，因此 §4.4 的 DNS 副作用只来自源码，未实测。
4. **内核模块 autoload 未验证**：OrbStack 内核 `/proc/modules` 为空、`/sys/module` 无 nf_tables 条目（全部内建），无法观察「第一次执行 nft/iptables 时内核 `request_module` 拉模块」的行为，也无从验证「模块不存在时」的错误形态。
5. **没有真实流量验证**：安全红线要求不做真实代理流量实验，因此「规则装上后 TCP/UDP 是否真的按预期被劫持/回环是否干净」**未验证**，只验证了规则可创建。
6. **未在 Docker/K8s/其他防火墙共存场景测试**：`DOCKER-USER` 链、kube-proxy 的 iptables 规则、ufw/firewalld 的 nft 表均未涉及。

### 6.5 可复现命令

```bash
# 准备镜像（宿主已有可联网的 Debian 基础镜像时）
docker run -d --name r11-build --entrypoint sh <debian12-image> -c 'sleep 7200'
docker exec r11-build sh -c 'apt-get update && apt-get install -y --no-install-recommends nftables iptables iproute2 kmod'
docker commit r11-build r11-net:local && docker rm -f r11-build

# 两次探测
docker run --rm --entrypoint sh r11-net:local /probe.sh default
docker run --rm --privileged --entrypoint sh r11-net:local /probe.sh privileged
```

（探测脚本见本次调研使用的 `probe.sh` / `probe3.sh`；脚本内所有写操作都在 `R11*` 命名空间并自带清理。）

---

## 7. 四分类范围（MVP Supported / Detection Only / Later / Unsupported）

### 7.1 MVP Supported（必须实现且可用）

| 能力 | 为什么是 Supported | 风险 / 约束 |
|---|---|---|
| HTTP / SOCKS / Mixed 端口（`port` / `socks-port` / `mixed-port`） | 无任何网络特权前提，是产品的「保底可用」路径；也是 AGENTS.md 中「降级状态」的基线 | 端口冲突需可检测（`ss -lnt`）；默认只监听 127.0.0.1，不默认 `0.0.0.0` |
| Mihomo 生命周期 / 配置版本 / reload / 回滚 / 订阅 | 与网络能力解耦 | 见 R09/R10 |
| **TUN 可选启用**（用户在配置中显式 `tun.enable: true`） | 官方能力成熟，且**规则由 Mihomo 自己维护、退出即清**，对 Agent 而言是「零规则写入」的最优透明代理路径 | ① 启用前必须要求 doctor 通过 `tun` 前提检测；② TUN 失败必须降级为 Mixed 端口而不是让 Agent 失败；③ 提醒用户 `dns-hijack` 会调 `resolvectl` |
| 端口/监听探测与占用冲突识别 | 只读、低风险，直接影响可用性 | 误报要可解释 |
| Mihomo 配置模板生成（含 tun 段、DNS 段） | Agent 的核心职责是「生成配置 + 校验」 | 生成的字段必须来自官方文档（本文 §2/§4） |

### 7.2 MVP Detection Only（只做 doctor 检测与提示，不自动改网络）

| 检测项 | 观测方法（建议） | 输出状态 |
|---|---|---|
| `/dev/net/tun` 是否存在且可打开 | `stat` + `open(O_RDWR)`（不 `TUNSETIFF`） | `Supported` / `Unavailable` |
| `CAP_NET_ADMIN`（当前进程 + 目标 Mihomo 进程） | `capget` 或读 `/proc/<pid>/status` `CapEff` 的 bit 12；systemd 场景读 `AmbientCapabilities` | `Supported` / `Unsupported` |
| `nf_tables` 是否可用 | `nft list tables`（只读）或 netlink 探测；错误串区分「内核不支持」与「权限不足」 | `Supported` / `Unsupported` / `Misconfigured` |
| nftables 二进制与版本 | `nft --version` | `Unknown`（缺失时） |
| iptables 存在性与**后端** | `iptables --version`（回显 `(nf_tables)` / `(legacy)`）+ `update-alternatives --display iptables` + `iptables-legacy -L -n` 与 `iptables -L -n` 对比 | `Supported` / `Misconfigured` |
| 内核 TPROXY 支持 | 只读 `/proc/net/ip_tables_targets`（含 `TPROXY`）、`/proc/net/ip_tables_matches`（含 `socket`）；nftables 侧只能靠内核版本/试加规则 | `Supported` / `Unknown` |
| 策略路由现状 | `ip rule show`、`ip route show table all`、检查是否已有 `fwmark` rule / `2022` 表 / `32768` rule（**Mihomo 残留信号**） | `Supported` / `Misconfigured` |
| `route_localnet` | `sysctl -n net.ipv4.conf.all.route_localnet` | 仅提示，不修改 |
| `resolvectl` 与 systemd-resolved | `command -v resolvectl`、`resolvectl status`、`/etc/resolv.conf` 内容 | `Supported` / `Unavailable` |
| DNS 端口冲突 | `ss -lntup` 找 53 监听者 | `Misconfigured` |
| LXC/PVE 环境识别 | `systemd-detect-virt`、`/proc/1/environ`、`/proc/self/cgroup`、`/sys/fs/cgroup` | 影响提示文案 |
| 当前防火墙管理器 | ufw / firewalld / docker / kube-proxy 的存在与规则数量（只读） | `Misconfigured`（提示共存风险） |
| 规则残留检测 | 只读扫描：`table inet mihomo`、`ip mangle`/`ip nat` 中的 mihomo 链、`mihomo_*` 链、2022 表、`0x2d0` fwmark rule | `Misconfigured` |

> 设计原则：**MVP 中所有网络写操作都不属于 Agent**。Agent 只回答「能不能、为什么不能、怎么修」。

### 7.3 Later（暂缓，等有 PVE 真机 + 回滚机制后再做）

| 能力 | 前置条件（必须先完成） |
|---|---|
| TProxy / redirect **规则生成**（nftables + iptables 双模板） | DryRun 输出 diff、快照、watchdog、死手开关、冲突检测（§8 全部） |
| `auto-redirect` 的受控开启/关闭 | 需要真机验证与 Docker/其他防火墙共存 |
| 规则自动回滚与「定时自杀」 | 独立于被破坏路径的回滚通道（本地 console / systemd-run 预排程） |
| systemd 两档 unit（proxy-only / tun-enabled）的自动切换 | 见 R09 |
| 无 systemd 环境的 `direct-process` fallback | 见 R09 |
| 多网卡/多出口自动选择建议 | 需要真实拓扑 |
| LXC 宿主侧配置片段生成（`pct set` / `lxc.*` 行） | 只生成文本，由用户执行（不要 Agent 直接改宿主） |

### 7.4 Unsupported（明确不做）

| 不做的事 | 理由 |
|---|---|
| 修改 **PVE 宿主** 或任何宿主级网络（`/etc/pve/lxc/*.conf` 自动写入、宿主 `iptables`/`nft`、宿主 sysctl） | 越过容器边界，破坏面不可控；与「容器内能力不足就降级」的产品哲学冲突 |
| 修改 **宿主 macOS** 的任何网络配置 | 本仓库的开发环境红线；产品本身也不面向 macOS |
| 无 `CAP_NET_ADMIN` 时「想办法」绕过（setuid helper、sudo NOPASSWD、滥用其他 cap） | 提权通道本身就是漏洞；正确行为是报告 `Unsupported` 并降级 |
| `iptables -F` / `nft flush ruleset` / 全量接管现有防火墙 | 会造成不可逆断网；也与 Docker/K8s/ufw/firewalld 不可能共存 |
| 自动改写 `/etc/resolv.conf` 或自动执行 `DNSStubListener=no` | 超出代理职责，且失败会导致全机 DNS 不可用 |
| 自动 `modprobe` 加载内核模块 | 上游 Mihomo 自己都不做（全树无 `modprobe`）；在 LXC 中通常也不允许 |
| 在 Docker/K8s/rootless 容器内承诺全透明接管（TUN/TProxy 全流量） | 命名空间与既有 DNAT/策略路由冲突，无法给出可靠保证 |
| 真实流量转发实验作为「安装即测试」 | 与 red line 一致：Agent 不做流量探测 |
| 自动设置/恢复 `route_localnet` | 全局安全开关，必须人工确认 |

---

## 8. 失败安全与回滚设计

前提假设：**一旦 Agent 写错防火墙规则，最坏后果是整机断网、SSH 失联**。因此设计的核心不是「写对」，而是「写错也能自己活回来」。

### 8.1 分层策略（按风险从低到高）

| 级别 | 行为 | MVP 是否采用 |
|---|---|---|
| L0 只读 doctor | 检测 + 提示，不写任何东西 | ✅ MVP |
| L1 DryRun 渲染 | 生成规则文本/diff 给人看，**不写内核**；可用容器/`nft -c`（check）做语法校验 | ✅ MVP（可选） |
| L2 人工确认后 Apply | 需要显式 `--yes` + 二次确认；带快照 + watchdog | Later |
| L3 全自动 Apply | 需要上面全部 + 死手开关 + 已通过真机验证 | Later（不承诺） |

### 8.2 Apply 的安全骨架（Later 实现时必须齐备）

```text
1. Preflight（全部失败即中止，不做任何写操作）
   - capability / 内核支持 / 冲突检测（已有 table inet mihomo？已有 fwmark rule？）
   - 目标端口可用性（redirect server 端口）
   - 快照：nft list ruleset > snapshot.nft
           iptables-save > snapshot.v4 / ip6tables-save > snapshot.v6
           ip rule show > rules.txt ; ip route show table all > routes.txt
           sysctl 值（route_localnet / ip_forward）> sysctl.txt
   - 快照写入 /var/lib/proxy-agent/network/<txid>/，落盘 + fsync

2. Transaction（只碰自有命名空间，绝不 flush）
   - nft：新建 table inet <agent-owned-name>（不要复用 "mihomo"），或在自己的 base chain 上挂
   - iptables：只建自己的链 + 一条 `-j <own-chain>` 跳转，不删除任何既有规则
   - 所有写操作按「记录操作日志 → 执行 → 校验回读」模式进行

3. Verify（超时窗口内）
   - 回读规则是否与预期一致（normalize 后比对）
   - 连通性健康检查（不走被改写的路径：如 controller API 本地调用 + 一条固定直连探测）
   - 超时/失败 → 回滚

4. Watchdog + 死手开关（dead-man switch）
   - 独立于 Agent 主进程：`systemd-run --on-active=120s /usr/lib/proxy-agent/net-rollback <txid>`
   - Verify 通过后 `systemctl stop` 掉该定时单元（取消自杀）
   - 这样即使 Agent 崩溃/被杀/断网，规则也会在 N 秒后自动回滚
   - 更好的形态：由「未被防火墙影响」的第二个进程/服务持有回滚脚本与快照

5. Rollback
   - 删除自有 table / 链 / rule / 路由 / TUN 设备
   - 恢复 sysctl（若改过）
   - 处理 iptables-nft 残留：空的 `ip mangle` / `ip nat` 表会留下（[实测-容器] §6.3）。
     策略：**接受**（无害、无规则）或**用快照 diff 决定是否 `nft delete table ip mangle`**
     —— 绝不无条件删除，因为其中可能承载别人的规则
```

### 8.3 与 Docker / K8s / 其他防火墙共存

| 场景 | 风险 | 策略 |
|---|---|---|
| Docker 默认 `172.17.0.0/16` + `DOCKER`/`DOCKER-USER` 链 | 劫持容器出站流量；与 `nat` 表优先级纠缠 | 默认把这些网段放入 bypass；检测到 Docker 时降级为提示 |
| kube-proxy（iptables 或 nft 模式）/ k3s | 全量接管会破坏 Service 转发 | 未验证前不支持自动接管 |
| ufw / firewalld | ufw 管理自己的链，firewalld 管理 `firewalld` nft 表；混用 backend 时互相不可见 | doctor 只读检测；规则模板避开它们的表/链名 |
| iptables-nft 与 iptables-legacy 并存 | 两套规则互不可见（[实测-容器]：nft 侧出现 `iptables-legacy tables present` 警告） | doctor 必须分别 dump 两套；Apply 前确认系统实际使用哪套 |
| LXC 内 netfilter | 取决于 `CAP_NET_ADMIN` 与宿主配置；PVE 宿主 `firewall=1` 可能另有一层 | 只使用容器 netns 内的规则，不承诺穿透宿主 |

### 8.4 对 MVP 的明确建议

1. **MVP 只做 L0 + L1**：`doctor` + `proxyctl network rules --dry-run`（打印将生成的 nft/iptables 文本）。**不 apply**。
2. 需要透明代理时，**首选让用户在 Mihomo 配置里开 TUN**（规则由 Mihomo 自管、退出即删），Agent 只负责前提检测与降级。
3. 若用户坚持 TProxy，提供文档化的**手工规则模板**（本文 §3.2），由用户自行执行。
4. 一旦进入 Later 的自动 Apply，**必须**同时实现：快照、DryRun 默认、watchdog、死手开关、只碰自有命名空间、断网自救文档（含如何在 console 上恢复）。

---

## 9. 对 Agent 架构的影响（Port 边界）

### 9.1 Port 划分建议

```text
① NetworkProbe（只读，MVP 必须）
   - 幂等、无副作用、可在任何用户下调用（权限不足时返回 Unavailable 而非报错）
   - 必须能在「没有 nft/iptables 二进制」的环境下工作（读 /proc、/sys）

   trait NetworkProbe {
       async fn inspect(&self) -> Result<NetworkReport>;      // 一次性聚合，带缓存 TTL
       fn capabilities(&self) -> &NetworkCapabilities;        // 最近一次快照
   }

② FirewallRenderer（纯函数，MVP 可做；放在 application 或 infrastructure？见 9.3）
   trait FirewallRenderer {
       fn render(&self, plan: &TransparentProxyPlan, backend: FirewallBackend) -> Result<FirewallRuleset>;
   }

③ FirewallManager（Later，写操作）
   trait FirewallManager {
       fn backend(&self) -> FirewallBackend;                  // Nft | IptablesNft | IptablesLegacy | None
       async fn snapshot(&self) -> Result<FirewallSnapshot>;   // 幂等
       async fn current_owned(&self) -> Result<Option<FirewallSnapshot>>;  // 只读：我自己的规则现在是什么
       async fn apply(&self, tx: &FirewallTransaction) -> Result<ApplyOutcome>;   // 事务性、幂等
       async fn rollback(&self, tx_id: TxId) -> Result<()>;    // 幂等、可重复调用
       async fn verify(&self, tx: &FirewallTransaction) -> Result<VerifyOutcome>;
   }

④ TunCapability / (可选) TunDeviceManager
   MVP：只需要能力检测 + 配置生成，不要 Port 去创建 TUN 设备
   Later：若必须托管设备，再引入 TunManager，且设备生命周期与 Mihomo 进程分离要有明确 owner
```

### 9.2 幂等性要求（硬性）

| 方法 | 幂等语义 |
|---|---|
| `NetworkProbe::inspect` | 重复调用结果一致（除环境变化）；不产生任何副作用；不写入任何文件 |
| `FirewallManager::snapshot` | 重复调用返回等价快照；不改变内核状态 |
| `FirewallManager::apply` | 同一个 `tx_id` 重复 apply = 结果等价（**不要**重复追加规则）；实现方式：先删自有 table/chain 再重建（如 Mihomo 的 `cleanup → setup`），或先 `current_owned()` 判断 |
| `FirewallManager::rollback` | 重复调用不报错（`ENOENT` 视为成功）；必须在「从未 apply 过」时也安全 |
| `FirewallManager::verify` | 纯读 |

### 9.3 规则内容算 Domain 还是 Infrastructure？

| 内容 | 归属 | 理由 |
|---|---|---|
| **要不要做透明代理**、哪个入口（TUN / TProxy / redirect）、哪些网段排除、DNS 如何处理、mark 值等**策略** | Application（以 value object 表达，如 `TransparentProxyPlan` / `BypassSet` / `FirewallPolicy`） | 这是业务决策，需要被校验、被 diff、被 API 暴露；与 netfilter 语法无关 |
| **具体 nft/iptables 文本与 netlink 表达式**（`meta l4proto tcp tproxy to :…`、`type nat hook prerouting priority dstnat + 1`、`iptables -t mangle -A …`） | Infrastructure（`FirewallRenderer`/adapter 内部） | 这是可替换的实现细节；换 backend（nft ↔ iptables-legacy）不应改变上层代码 |
| **Capability 状态与原因** | Domain 的 system 模型（`NetworkCapabilities` / `CapabilityStatus`）+ Infrastructure 填充 | 与现有设计文档 §21 一致：不能用 `bool`，必须是 `Supported / Unsupported / Unavailable / Misconfigured / Unknown` |
| **进程命令执行** | Infrastructure（`ProcessManager`），且**只能**由 adapter 内部调用 | AGENTS.md 明令：Application/Domain/interfaces 不得 exec |

推荐的 `NetworkCapabilities` 形状（扩展设计文档 §21）：

```rust
pub struct NetworkCapabilities {
    pub tun_device: CapabilityStatus,          // /dev/net/tun 可打开
    pub net_admin: CapabilityStatus,           // CAP_NET_ADMIN
    pub nf_tables: CapabilityStatus,           // 内核 + nft 二进制
    pub iptables_backend: IptablesBackend,     // NfTables | Legacy | None | Unknown
    pub xt_tproxy: CapabilityStatus,           // /proc/net/ip_tables_targets 含 TPROXY
    pub nft_tproxy: CapabilityStatus,          // 需试建规则或按内核版本推断 -> Unknown
    pub policy_routing: CapabilityStatus,      // ip rule/route 可写（写测试必须可回滚，MVP 只读）
    pub systemd_resolved: ResolvedState,       // Absent | Stub | Disabled | Unknown
    pub existing_rule_residue: Vec<Residue>,   // table inet mihomo / mihomo_* 链 / 2022 表 / 0x2d0 rule
    pub conflicting_managers: Vec<String>,     // docker / kube-proxy / ufw / firewalld
    pub notes: Vec<CapabilityNote>,            // 人类可读原因 + 证据
}
```

### 9.4 依赖方向约束（与 AGENTS.md 一致）

```text
interfaces (API/CLI/TUI)  →  application (RunDoctor / PlanTransparentProxy / ApplyNetworkPlan)
application               →  Port (NetworkProbe / FirewallManager / FirewallRenderer)
infrastructure            →  Port 实现 (nftables adapter / iptables adapter / /proc reader)
domain                    →  纯值对象（CapabilityStatus / NetworkCapabilities / FirewallPolicy）
```

禁止：`application → nftables`、`domain → tokio/std::process`、API handler 里拼 `iptables` 命令行。

---

## 10. 证据与来源

### 10.1 上游文档

| 主题 | 链接 |
|---|---|
| Mihomo TUN 全字段 | https://wiki.metacubex.one/config/inbound/tun/ （原始 Markdown：`MetaCubeX/Meta-Docs` `docs/config/inbound/tun.md`） |
| Mihomo 代理端口（`redir-port` / `tproxy-port`，透明代理端口说明） | https://wiki.metacubex.one/config/inbound/port/ |
| Mihomo TProxy listener | https://wiki.metacubex.one/config/inbound/listeners/tproxy/ |
| Mihomo REDIRECT listener | https://wiki.metacubex.one/config/inbound/listeners/redirect/ |
| Mihomo DNS 配置（`fake-ip` / `respect-rules` / `listen` / `enhanced-mode`） | https://wiki.metacubex.one/config/dns/ |
| Mihomo handbook DNS（DNS 劫持的目的） | `Meta-Docs` `docs/handbook/dns.md` |
| kernel TProxy 官方文档（nft/iptables 规则 + 策略路由 + 模块名） | https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/networking/tproxy.rst |
| kernel `route_localnet` / `rp_filter` | https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/networking/ip-sysctl.rst |
| kernel Kconfig（`NFT_TPROXY` / `NETFILTER_XT_TARGET_TPROXY`） | https://raw.githubusercontent.com/torvalds/linux/master/net/netfilter/Kconfig |
| systemd-resolved stub listener（127.0.0.53/54:53、`DNSStubListener=`） | https://www.freedesktop.org/software/systemd/man/latest/resolved.conf.html |
| PVE 非特权 LXC 的 TUN 设备写法 | https://pve.proxmox.com/wiki/OpenVPN_in_LXC |
| PVE Linux Container（`features: nesting=1`、非特权容器） | https://pve.proxmox.com/wiki/Linux_Container |

### 10.2 上游源码

| 文件 | 用途 |
|---|---|
| mihomo `Alpha` `listener/config/tun.go`、`listener/inbound/tun.go` | TUN 全字段（YAML tag / inbound tag） |
| mihomo `Alpha` `constant/tun.go` | stack 枚举与默认（gVisor=0） |
| mihomo `Alpha` `listener/sing_tun/server.go` | `auto-redirect` 初始化、`auto-route` 依赖校验、`dns-hijack` 解析、`tableName="mihomo"`、MTU 默认 9000、iproute2 默认值 |
| mihomo `Alpha` `listener/sing_tun/dns.go` | `ShouldHijackDns` 语义 |
| mihomo `Alpha` `listener/sing_tun/redirect_linux.go` | `supportRedirect = true`（仅 Linux） |
| mihomo `Alpha` `listener/inbound/tproxy.go` | tproxy listener 只有监听，无规则 |
| mihomo `Alpha` `listener/tproxy/tproxy_iptables.go` | 官方 iptables 规则原文、`0x2d0` mark/table、`sysctl ip_forward`、cleanup |
| mihomo `Alpha` `listener/tproxy/udp_linux.go` | `IP_TRANSPARENT` setsockopt |
| mihomo `Alpha` `listener/redir/tcp_linux.go` | `SO_ORIGINAL_DST = 80`（TCP-only redirect） |
| mihomo `Alpha` `hub/executor/executor.go:463-534` | legacy `iptables:` 自动配置流程、`os.Exit(2)`、与 TUN 互斥 |
| mihomo `Alpha` `config/config.go`（`RawIPTables` / `DNS` / `parseDNS`） | `iptables.*` 字段、`respect-rules` 强校验 |
| mihomo `Alpha` `dns/system_posix.go` | `/etc/resolv.conf` 仅读取，不写入 |
| sing-tun `meta` `redirect_linux.go` / `redirect.go` | `autoRedirect` 初始化、nftables 探测与回退、mark 默认值 |
| sing-tun `meta` `redirect_nftables.go` / `redirect_nftables_rules.go` | `table inet mihomo` 完整链/集合结构与规则生成 |
| sing-tun `meta` `redirect_iptables.go` | iptables 回退实现 |
| sing-tun `meta` `tun_linux.go` | auto-route 的 netlink rule/route 实现、`strict-route`、`resolvectl` 副作用 |
| sing-tun `meta` `tun.go` / `tun_rules.go` | `DefaultIPRoute2TableIndex=2022`、`DefaultIPRoute2RuleIndex=9000`、fallback `32768` |
| sing-tun `meta` `tun_offload_linux.go` | GSO 仅 Linux |

### 10.3 本次容器实测（原始记录）

| 记录 | 内容 |
|---|---|
| `[实测-容器]` run #1 | 非特权 Debian 12：工具/内核/权限/模块视图、全部写操作 `EPERM` |
| `[实测-容器]` run #2 | `--privileged` Debian 12：nft tproxy、iptables(nft/legacy) TPROXY、REDIRECT、策略路由、TUN 设备全部成功并可清理 |
| `[实测-容器]` run #3 | `/sys/module` 与 `/proc/net/ip_tables_*` 视图；mihomo 风格 nftables 结构子集；`inet` 表 DNAT 语法坑；iptables-nft 兼容表残留 |

（原始 shell 输出保留在本次调研的临时目录中，落盘产物只有本文档；容器已 `--rm` 销毁，`/tmp/r11-net/` 已清理。）

---

## 11. 未验证假设与开放问题

| # | 假设 / 问题 | 现状 | 需要什么才能确认 |
|---|---|---|---|
| Q1 | PVE **非特权** LXC 中 `CAP_NET_ADMIN` 是否存在 | 只能引用 PVE wiki 的 TUN 写法，未实测 | 真机 PVE + `pct` 非特权容器上跑 `doctor` |
| Q2 | PVE **特权** LXC 与 host netns 的 netfilter 边界（容器内 nft 是否会看到/影响宿主表） | 未验证 | 真机实验（需宿主侧观察，注意隔离） |
| Q3 | PVE 宿主 `firewall=1`（`net[n]: firewall=1`）与容器内 nftables 规则的相互作用 | 未验证 | 真机 + PVE 防火墙启用 |
| Q4 | LXC 中 `nf_tables`/`nft_tproxy` 模块是否存在并按需 autoload | OrbStack 内核全内建、`/proc/modules` 为空，无法观察 autoload | 真机 + `modprobe` 前后对比 |
| Q5 | `nft` 的 `tproxy` 语句在不合适的链（`type nat`）中是否"看起来成功但语义错误" | [实测-容器] 语法接受，语义未验证 | 真机 + 真实流量（**与安全红线冲突，需要单独受控环境**） |
| Q6 | Mihomo `auto-redirect` 与 Docker bridge / kube-proxy 规则共存时的实际行为 | 未验证 | 真机容器/集群环境 |
| Q7 | TUN + `dns-hijack` 后若 Mihomo 被 `SIGKILL`，systemd-resolved 是否真的残留 `~.` 路由 | 源码推断 `[推测]` | 真机 systemd 环境实测 |
| Q8 | `route_localnet` 在真实 REDIRECT-to-127.0.0.1 场景是否必需、以及最小影响面 | 只有内核文档定义 | 真机受控实验 |
| Q9 | Debian 13 / Ubuntu 24.04 上 `iptables` 默认 alternative 是否仍为 nft、nftables 版本差异 | 只测了 Debian 12（nftables 1.0.6） | 更多发行版容器矩阵 |
| Q10 | `tproxy` listener 在 `udp: true` 时对 QUIC/HTTP3 的实际处理（是否需要额外防火墙 udp 规则） | 未验证 | 真机流量测试 |
| Q11 | `fake-ip` 与 systemd-resolved 的 LLMNR/mDNS、以及 `.local` 域名的交互 | 未验证 | 真机 |
| Q12 | 上游是否仍在维护 legacy `iptables: {enable: true}` 路径（现代 wiki 已不记录 `iptables`/`tproxy-port` 的自动配置） | 源码仍在，wiki 无文档 | 关注上游变更 / 提 issue |
| Q13 | Agent 生成 nftables 规则时，如何与已有 `table inet mihomo`（Mihomo 自己的 auto-redirect）共存而不互相删除 | 未设计 | 需要一个「Mihomo 独占表名 vs Agent 独占表名」的明确约定（建议 Agent 使用不同表名） |
| Q14 | 在 systemd 沙箱（`ProtectKernelTunables=yes` 等）下，doctor 的只读检测是否仍能读取 `/proc/net/ip_tables_targets`、`/sys/module` | 只有 R09 的推断 | 真机 systemd 环境实测 |

---

*本文档仅覆盖 R11 调研范围；不包含 ADR。相关 ADR（如 `NetworkAdapter` / `FirewallManager` Port 定义、透明代理自动化边界）应在 Phase 1 单独提出。*
