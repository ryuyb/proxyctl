# R08 — ShellCrash Feature Reverse Engineering

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：源码静态阅读（未执行安装）
> 调研对象：`juewuy/ShellCrash` master 分支快照（GitHub archive tarball），仅做静态阅读与只读解包，**未执行 `install.sh`**。
> 关键结论一句话：ShellCrash 用一套 1.2 万行 POSIX sh 菜单脚本，把"Mihomo 生命周期 + 网络劫持 + 订阅"在几十种 Linux/固件形态上跑通了，它的**能力探测**和**生成前校验**值得借鉴，但它的**覆盖式自更新、手工双写的防火墙规则、无版本化的配置、失败即自我锁死**是我们必须系统性替换掉的设计。

---

## 1. 结论摘要（TL;DR）

1. **本质**：ShellCrash 不是一个"服务"，而是一个**菜单式 sh 运维脚本 + 打包 payload（tar.gz）**。它的"架构"体现在目录约定（`$CRASHDIR/{configs,yamls,jsons,ruleset,providers,ui,tools,task}`）和一批可被 `case` 分派的启动/清理脚本上，没有进程常驻的控制面。`[上游源码]`
2. **最值得 Reuse 的三个概念**：
   - **生成配置后、替换线上配置前，用内核自身做一次 `-t` 语法校验**（`clash_modify.sh` 的 `test_yaml()`），失败降级到基础配置而不是让内核崩。
   - **能力探测而非假设**：`iptables -j TPROXY -h`、`/proc/net/ip_tables_targets`、`nft add table` 试写、`modprobe tun` + 轮询 `ip route | grep utun`，逐条判断"这个内核到底支持什么"。
   - **订阅下载失败绝不覆盖当前配置**：3 次重试 + 轮换转换服务器，仍失败则 `exit 1`，旧 `config.yaml` 原样保留。
3. **最需要 Improve 的差异点**：
   - **升级/回滚**：自更新是"下载 tar.gz → 停服务 → 覆盖解压 → 重跑 init"，**无校验、无备份、无原子性、无回滚**；所谓"版本回退"是从 `api.github.com/.../tags` 挑一个 tag 重新覆盖安装，不是快照回滚。
   - **配置版本化**：全局只有一个 `yamls/config.yaml.bak`（单份），无版本号、无 checksum、无 `list/diff/activate/rollback`。
   - **网络规则**：每条规则在"加"（`fw_iptables.sh` / `fw_nftables.sh`）和"删"（`fw_stop.sh`）两处**手工各写一遍**，规则形态一变就会残留；且会向系统 `fw4` 表插入规则，而清理时只 `nft delete table inet shellcrash`，**插进 fw4 的规则不回收**。
   - **故障自救**：启动失败写 `.start_error` 标记，之后 `bfstart.sh` 见到就 `exit 1` **禁止自启**，必须人工 `crash -s start` 或 debug 菜单去删标记——是"自我锁死"而不是"自动回退到上一份好配置"。
   - **权限模型**：systemd 服务用户 `shellcrash` 实际是 **uid 0 + gid 7890**（`sed -Ei s/7890:7890/0:7890/g /etc/passwd`），它只是用独立 gid 给内核流量打"免劫持"标记，**不是权限隔离**。
   - **安全默认值**：Mihomo `external-controller` 写成 `:$db_port`（等价 `0.0.0.0:9999`），且安装/下载默认 `curl -k` / `wget --no-check-certificate`，仓库内**没有任何 sha256/GPG 校验**。
4. **应当 Ignore 的部分**：脚本自身作为发行形态、菜单作为主交互、路由器固件适配（Padavan/梅林/小米/NETGEAR）、TG Bot、内置 DDNS、`/etc/init.d/firewall` 注入式旁路由。这些是"一台设备一个固件"的产物，不是 Linux Server/PVE LXC 产品该背的复杂度。
5. **对 PVE/LXC 的直接结论**：上游 README 把 PVE 明确归入"部分可能不兼容的设备，建议用 Docker 跑"；容器分支只做了 `/proc/1/cgroup` 粗判定 + 强制 `nftables`，**没有** privileged/unprivileged、`CAP_NET_ADMIN`、`/dev/net/tun` 的实现级检测。这正是我们 `doctor` 与 `CapabilityStatus` 要补的空白。

---

## 2. 仓库与平台事实

### 2.1 仓库身份

| 项 | 值 | 证据 |
| :--- | :--- | :--- |
| 仓库 | `https://github.com/juewuy/ShellCrash` | README_CN.md:1-18、本仓库 URL `[上游文档]` |
| Stars | **13k** | `https://img.shields.io/github/stars/juewuy/ShellCrash.json` → `"message":"13k"`（2026-09-12 读取）`[上游文档]` |
| License | **GPL-3.0**（GNU General Public License v3.0） | `LICENSE.txt`（35,149 bytes，GPLv3 全文）；README_CN.md:185 "本项目采用 GNU 通用公共许可证第 3.0 版" `[上游文档]` |
| 最新 release tag | **`1.9.4-release`**，published `2026-02-15T11:44:01Z` | `https://github.com/juewuy/ShellCrash/releases.atom` 首个 entry `tag .../1.9.4`，标题 `1.9.4-release` `[上游文档]` |
| 最近提交 | **`eb0c211c176323815f67aa088d8152d6038cab21`**，`2026-08-02T02:42:00Z`（"Merge branch 'master' ... into dev"）；同日前一条 `78ad72f` "~1.9.5beta3 pkg" | `https://github.com/juewuy/ShellCrash/commits/master.atom` `[上游文档]` |
| 仓库内版本号（三处不一致） | `version` = `1.9.5beta3`（11 bytes，单行）；`bin/release_version` = 首行 `1.9.1` + 历史版本列表 `1.9.0/1.8.0/1.7.0/1.6.3/1.5.1/1.3.0/1.2.0/1.1.0`；`bin/version` = shell 变量赋值 `meta_v=v1.19.17` / `singboxr_v=1.13.0-alpha.27` / `versionsh=1.9.5beta3` / `GeoIP_v=20251205` | 三个文件的实际内容 `[上游源码]`；`bin/version` 被 `9_upgrade.sh:114-122` 下载后 `.` source 进 shell，`9_upgrade.sh:25` 用 `eval echo \$"$crashcore"_v` 取内核版本 |
| 分支 | `master` / `dev` / `rm` / `update`（`bin/*` 与 `public/`、`rules/` 被分流到独立分支下载） | `git ls-remote --heads`（经代理）返回 `refs/heads/{dev,master,rm}`；`web_get_bin.sh:7-8` 把 `bin/` 指向 `update` 分支、`public|rules` 指向 `dev` 分支 `[上游源码]` |

> 版本号出现"三处不一致"（`version` 1.9.5beta3 / `bin/release_version` 1.9.1 / 最新 tag 1.9.4）本身是一个信号：**版本元数据没有单一事实源**，这正好对应我们在 `ConfigVersion` / `MihomoVersion` 上要立的反例。

### 2.2 支持的平台（README 自述）

`[上游文档]` README_CN.md:33-39：

- **路由器设备**：各种基于 OpenWrt 或其二次开发固件（小米路由、网件等）
- **Linux 服务器**：Debian、CentOS、Armbian、Ubuntu 等标准 Linux/GNU 发行版
- **第三方固件**：Padavan（保守模式）、潘多拉、华硕/梅林
- **其他设备**：各种基于 Linux/GNU 或 Linux/busybox 的设备
- **Docker**：**"部分可能不兼容的设备（如群辉、PVE），支持 docker 环境运行"**

`[上游源码]` 运行依赖（README_CN.md:166-172）：

| 依赖组件 | 必要性 | 说明（原文） |
| :--- | :--- | :--- |
| curl / wget | 必须 | 缺少时无法节点保存、在线安装及更新 |
| iptables / nftables | 重要 | 缺少时仅能运行于纯净模式 |
| crontab | 较低 | 缺少时定时任务失效 |
| net-tools | 极低 | 缺少时无法自动检测端口占用 |
| ubus / iproute-doc | 极低 | 缺少时无法自动获取本机 Host 地址 |

### 2.3 支持的 init 系统（源码判定）

`[上游源码]` `scripts/init.sh:41-72` 是唯一的 init 分派点：

```sh
41  [ -w /usr/lib/systemd/system ] && sysdir=/usr/lib/systemd/system
42  [ -w /etc/systemd/system ] && sysdir=/etc/systemd/system
43  if [ -f /etc/rc.common -a "$(cat /proc/1/comm)" = "procd" ]; then      # OpenWrt procd
45      cp -f "$CRASHDIR"/starts/shellcrash.procd /etc/init.d/shellcrash
47  elif [ -n "$sysdir" -a "$USER" = "root" -a "$(cat /proc/1/comm)" = "systemd" ]; then  # systemd
59      mv -f "$CRASHDIR"/starts/shellcrash.service "$sysdir"/shellcrash.service
61      systemctl daemon-reload
63  elif rc-status -r >/dev/null 2>&1; then                                # OpenRC
65      mv -f "$CRASHDIR"/starts/shellcrash.openrc /etc/init.d/shellcrash
68  else                                                                   # 保守模式
70      setconfig start_old 已开启
```

此外还有三种"非 init"的自启路径：

- **s6-overlay**（Docker 镜像内）：`scripts/start.sh:55-59` 检测 `grep -q 's6' /proc/1/comm`，用 `/command/s6-svc -u /run/service/shellcrash`；s6 服务定义在 `docker/s6-rc.d/{shellcrash,bfstart,afstart,crond}`。
- **固件专用自启**：华硕/梅林用 `nvram set script_usbmount` / `jffs/.asusrouter`，Padavan 写 `/etc/storage/started_script.sh`，小米/NETGEAR 镜像化设备用 `uci set firewall.ShellCrash=include`（`scripts/init.sh:120-160`）。
- **保守模式（无 init 系统时）**：`start_old=ON`，由 cron 每分钟跑 `starts/start_legacy_wd.sh` 守护（`scripts/init.sh:68-72`、`scripts/starts/afstart.sh:44`）。

---

## 3. Feature 盘点（逐项，带源码证据）

### 3.1 安装与升级逻辑

**安装**（`install.sh`，372 行）：

- 安装源默认 `https://testingcf.jsdelivr.net/gh/juewuy/ShellCrash@master`（`install.sh:11`），README 另给 `gh.jwsc.eu.org`（作者私有源）、`raw.githubusercontent.com`、`http://t.jwsc.eu.org`（明文 HTTP 内测源）。
- 安装动作只有三步（`gettar()`，`install.sh:93-117`）：`webget /tmp/ShellCrash.tar.gz "$url/ShellCrash.tar.gz"` → `tar -zxf` 到 `$CRASHDIR` → `. "$CRASHDIR"/init.sh`。
- **安装目录可选**（`setdir()`，`install.sh:210-266`）：`/etc`、`/usr/share`、`~/.local/share`（非 root，同时 `mkdir -p ~/.config/systemd/user`）、外置存储、自定义；固件设备走 `/etc/storage`、`/data`、`/jffs`、`/tmp/mnt`。
- **覆盖安装保留配置**：`check_dir()`（`install.sh:333-367`）提示"覆盖安装时不会移除配置文件"→ 直接 `install`；选"卸载旧版本并安装"则是 `rm -rf "$CRASHDIR"`（`install.sh:347`）后重装。**没有备份步骤，也没有"保留旧版本以便回滚"的目录**。
- **版本通道**：`setversion()`（`install.sh:280-295`）在 `master` / `stable` / `dev` 之间通过 `sed "s/master/$release_type/"` 改写 URL 选择。
- **无完整性校验**：全仓库 `grep -rniE 'sha256|sha512|md5sum|gpg --|--verify|signature|checksum'`（`*.sh/*.yml/*.yaml/Dockerfile`）**命中 0 条** `[上游源码]`；相反，`curl` 一律带 `-k`、`wget` 一律带 `--no-check-certificate`（`install.sh:34,45`；`scripts/libs/web_get.sh:25,43`）。

**升级（自更新）**（`scripts/menus/9_upgrade.sh:135-161` `getscripts()`）：

```sh
138     get_bin "$TMPDIR"/ShellCrash.tar.gz ShellCrash.tar.gz
144         "$CRASHDIR"/start.sh stop 2>/dev/null
148         tar -zxf "$TMPDIR/ShellCrash.tar.gz" ${tar_para} -C "$CRASHDIR"/
153             . "$CRASHDIR"/init.sh >/dev/null
159     rm -rf "$TMPDIR"/ShellCrash.tar.gz
160     exit
```

即"停服务 → 就地覆盖解压 → 重跑 init → `exit`"。解压失败时只有提示（`error_down()`，`9_upgrade.sh:12-16`）"换个源重装"，**没有回滚**。`strings` 更新入口是菜单 9-1（`setscripts()`，`9_upgrade.sh:163-189`）。

**"回退"的真实含义**（`setserver()`，`9_upgrade.sh:1124-1273`）：菜单项 `e)` 会去 `https://api.github.com/repos/juewuy/ShellCrash/tags` 拉 tag 列表（`9_upgrade.sh:1233`），用户选一个 tag 写入 `release_type`，随后更新的仍是"下载该 tag 的 tar.gz 并覆盖"。1.9.4 release notes 也自述"优化版本回退功能，现在支持回退到近期 tags"。**结论：是"降级重装"，不是快照回滚。** `[上游源码]` + `[上游文档]`

**payload 与目录结构**：`ShellCrash.tar.gz`（payload，含 `init.sh` / `scripts` / `lang` / 模板）解包到 `$CRASHDIR`；随后 `init.sh:184-220` 建立/迁移出：

```
$CRASHDIR/
├── configs/      # ShellCrash.cfg（key=value 配置）、command.env、*.list、web_save、task/
├── yamls/        # config.yaml 与用户分段覆盖 user.yaml / proxies.yaml / proxy-groups.yaml / rules.yaml / others.yaml、config.yaml.bak
├── jsons/        # sing-box 生成物
├── ruleset/      # .mrs / .srs 规则集
├── providers/    # proxy-providers 落盘
├── ui/           # 本地 Dashboard
├── tools/        # tun.ko / ShellDDNS.sh 等
└── task/         # task.sh + task_*.list
```

**卸载**（`scripts/menus/uninstall.sh`）：停服务 → 清 cron → 询问"是否保留配置"，保留则把 `configs/ yamls/ jsons/` 暂存到 `/tmp/ShellCrash/*_bak`、`rm -rf "$CRASHDIR"/*`、再搬回（`uninstall.sh:28-37`）→ 再删 unit、`/usr/bin/crash`、`/etc/profile` 里的 alias/export、`userdel -r shellcrash`。**这是全项目唯一一处"先保住数据再删"的逻辑**，但它依赖 `/tmp` 暂存。`[上游源码]`

### 3.2 init system 适配

三种 unit 文件都在 `scripts/starts/` 下，安装时按 §2.3 分派，**不是多态，而是三份独立实现**：

**systemd**（`scripts/starts/shellcrash.service`，全文 19 行）：

```ini
[Unit]
Description=ShellCrash Core
After=network.target

[Service]
Type=simple
User=shellcrash
StandardOutput=null
ExecStartPre=/etc/ShellCrash/starts/bfstart.sh
ExecStart=/etc/ShellCrash/CrashCore run -D /etc/ShellCrash -C /tmp/ShellCrash/jsons >/dev/null
ExecStartPost=/etc/ShellCrash/starts/afstart.sh
ExecStopPost=/etc/ShellCrash/starts/fw_stop.sh
Restart=on-abnormal
RestartSec=10s
LimitNOFILE=infinity

[Install]
WantedBy=multi-user.target
```

两个重要观察 `[上游源码]`：

1. unit 里的 `ExecStart` 是 **sing-box 参数形态**（`run -D ... -C ...`），只在 `init.sh:60` 用 `sed "s%/etc/ShellCrash%$CRASHDIR%g"` 改路径。真正启动前，`scripts/start.sh:48-53` 会在**运行期**再次改写 unit 文件：

   ```sh
   49      FragmentPath=$(systemctl show -p FragmentPath shellcrash | sed 's/FragmentPath=//')
   51      sed -i "s#^ExecStart=.*#ExecStart=$COMMAND >/dev/null#" "$FragmentPath"
   52      systemctl daemon-reload
   ```

   `$COMMAND` 来自 `configs/command.env`（由 `core_check()` 在换内核时写入，见 §3.3）。**"启动服务"这个动作会去改 systemd unit 文件**，这是我们要明确避免的设计。
2. `User=shellcrash` 并非低权限账户：`init.sh:49-57` 先 `userdel shellcrash`、清 `/etc/passwd` 中的 `0:7890` 标记，再 `useradd shellcrash -u 7890` 然后 **`sed -Ei s/7890:7890/0:7890/g /etc/passwd`** —— 即 uid 变成 **0**、gid 变成 **7890**。`shellcrash.openrc:30` 的判据 `grep -q 'shellcrash:x:0:7890' /etc/passwd` 印证了这个字段布局。独立 gid 的唯一用途是让防火墙规则用 `-m owner --gid-owner 7890 -j RETURN` 豁免内核自身流量（`fw_iptables.sh:30-31`、`fw_nftables.sh:36`、`fw_nftables.sh:119`）。

**OpenRC**（`scripts/starts/shellcrash.openrc`）：用 `supervise-daemon "${name}" --pidfile /run/shellcrash.pid --user ${runuser} --respawn-max 0 --respawn-delay 3 --start ${COMMAND%% *} -- ${COMMAND#* }`（`:44-49`）；`CRASHDIR` 竟然是从 `/etc/profile` 里 `grep CRASHDIR | awk -F '"' '{print $2}'` 反查出来的（`:7-8`）——**服务定义依赖 shell profile 文本**。`firewall_area=5`（旁路由主旁转发）时直接不启内核，只跑 `fw_start.sh`（`:22-27`）。

**procd**（`scripts/starts/shellcrash.procd`，`USE_PROCD=1`）：`procd_set_param respawn` + `procd_set_param user $USER`（`:23-29`）；同样从 `/etc/profile` 反查 `CRASHDIR`（`:9-10`）。

**保守模式**：`start_old=ON` → 无守护进程，靠 cron 每分钟 `start_legacy_wd.sh`；该脚本用 `mkdir "$LOCKDIR" || exit 1` 做互斥、用 `/tmp/ShellCrash/shellcrash.pid` 判断存活（`start_legacy_wd.sh:3-19`）。`general_init.sh:30` 还会 `echo 0 > /proc/sys/vm/overcommit_memory` 来"优化系统默认内存检测机制"——直接改宿主内核参数。

**开机自启检测**：`scripts/libs/check_autostart.sh` 对五种后端各写一条判据（`start_old` / procd 的 `/etc/rc.d` / `systemctl is-enabled` / s6 的 `contents.d/afstart` / `rc-update show default`），并用 `$CRASHDIR/.dis_startup` 作为"用户主动禁用"标记。

### 3.3 Mihomo(Clash) 二进制管理

- **多内核**：`crashcore` ∈ `meta`（mihomo）/ `clash` / `clashpre` / `singboxp` / `singboxr`（`scripts/starts/check_target.sh:1-8` 只按 `singbox` 与否分流出 `target=singbox|clash`、`format=json|yaml`；`core_tools.sh:51-59` 用 `core_new -h | grep -q 'sing-box'` / `grep -q '\-t'` 识别内核族）。README_CN.md:24 自述"管理及切换 mihomo 与 sing-box 内核"。
- **下载源**（`scripts/libs/web_get_bin.sh:3-18`）：默认 `update_url=https://testingcf.jsdelivr.net/gh/juewuy/ShellCrash@master`；若配置了 `url_id`，则从 `configs/servers_chs.list` 取镜像地址；`bin/` 路径**强制切到 `update` 分支**，`public|rules` 切到 `dev` 分支。镜像清单（`public/servers.list`）：

  ```
  101 Jsdelivr_CDN源       https://cdn.jsdelivr.net/gh/juewuy/ShellCrash
  102 Github直连源          https://raw.githubusercontent.com/juewuy/ShellCrash
  103 ShellClash自建源(请勿滥用!) https://gh.jwsc.eu.org
  104 Cloudflare_CDN源(推荐) https://testingcf.jsdelivr.net/gh/juewuy/ShellCrash
  202 http私人内测源(危险!非必要请勿使用) http://t.jwsc.eu.org   ← 明文 HTTP
  401 作者提供,支持vless|hy2 https://sub.jwsc.eu.org ua          ← 订阅转换
  402 肥羊提供(有广告)          https://api.v1.mk diyua           ← 第三方 subconverter
  403 肥羊提供(有广告)          https://url.v1.mk diyua
  ```

- **第三方/自定义内核**：`9_upgrade.sh:329-390` `checkcustcore()` 走 `https://api.github.com/repos/${project}/releases/${api_url}`，用 `grep browser_download_url` + `grep -oE "...linux.*${cpu_type}.*\.(gz|upx)\""` 解析下载地址；也支持 `custcorelink` 完全自定义直链（`core_tools.sh:96-103`，按后缀推断 `zip_type`）。
- **完整性校验：无**。替代品是 `core_check()`（`core_tools.sh:48-87`）：把新下载物解到 `core_new`，**执行它**并判断是不是目标内核、顺便取版本号：

  ```sh
  53  if [ -n "$sbcheck" ] && "$TMPDIR"/core_new -h 2>&1 | grep -q 'sing-box'; then
  56  elif [ -z "$sbcheck" ] && "$TMPDIR"/core_new -h 2>&1 | grep -q '\-t';then
  60  if [ -z "$v" ]; then rm -rf "$1" "$TMPDIR"/core_new; return 2   # 识别失败 → 丢弃并要求手选架构
  ```

  这等价于"可执行性 + 指纹探测"，**不是密码学校验**：镜像被投毒时无法发现。`[上游源码]`
- **架构识别**：`scripts/libs/check_cpucore.sh` 用 `uname -ms` + `/proc/cpuinfo` 的 `vfp` + 对 `echo -n I | hexdump` 的端序判断 mips/mipsle；识别失败则要求手动指定（`core_tools.sh:12` 报错并要求 `setcpucore`）。
- **内核切换与降级**：`9_upgrade.sh:257-285` `switch_core()` 在 clash↔sing-box 之间切换时提示是否保留 geodata，并自动改 `dns_mod`（`redir_host` ↔ `mix`）；内核**版本**本身不可选（只有脚本自身能选 tag，见 §3.1），第三方内核可选 release tag。
- **存储策略（值得一提的巧思）**：`core_tools.sh:8-17` `store_raw_worth_it()` 先判断 `$TMPDIR` 是否 tmpfs，再按 `$BINDIR` 文件系统类型（`squashfs|ubifs|overlay|overlayfs` 按 2:1 压缩估算）与剩余空间决定"裸存二进制"还是"gz 压缩包 + 启动时解压到 /tmp"（`core_tools.sh:65-79`）。支持的形态：`tar.gz` / `gz` / `upx` / `raw`，`setziptype()`（`9_upgrade.sh:477`）可手动切换。**这是为 OpenWrt 小闪存做的优化，对 PVE LXC 场景我们的价值有限，但"先估算落盘体积再决定存储形态"的思路可以借鉴到规则集缓存。**
- **落盘位置**：内核常驻在 `$TMPDIR/CrashCore`（`/tmp/ShellCrash/CrashCore`，即内存盘），`$BINDIR` 只存压缩包 `CrashCore.{tar.gz,gz,upx,raw}`；`init.sh:86-95` 把 `COMMAND` 写进 `configs/command.env`。**内核本身在重启后需要重新解压**——这是 1.9.0 release notes 提到的"全面修改内核压缩方式"的结果。

### 3.4 配置管理

**配置目录**：`$CRASHDIR/yamls/`（Clash/mihomo）与 `$CRASHDIR/jsons/`（sing-box）；`check_target.sh:8` 定义 `core_config="$CRASHDIR/${format}s/config.$format"`。

**生成 / 合并策略**（`scripts/starts/clash_modify.sh`，`modify_yaml()` 是唯一入口，`:262-270`）：

```
prepare_clash_base_config      → 生成 set.yaml（端口/controller/tun/dns/sniffer/hosts 开关）
generate_set_and_hosts_yaml    → set.yaml 全文 + 从 /etc/hosts 抄 hosts 段
split_and_customize_yaml_parts → 用 sed 从 core_config 切出 proxies/proxy-groups/
                                 proxy-providers/rules/rule-providers/sub-rules/listeners，
                                 再把用户自定义段(user 目录下的分段文件)按缩进"粘"回去
add_custom_inbounds_and_rules  → 自定义入站 listeners + 节点绕过 + 自定义规则
merger_yaml                    → cut -c 1- 拼接 set.yaml + dns.yaml + hosts.yaml +
                                 user.yaml + others.yaml + 各段 (clash_modify.sh:233)
test_yaml                      → CrashCore -t -d $BINDIR -f config.yaml 校验
finalize_clash_yaml            → 软链/拷贝到 $BINDIR/config.yaml，清理临时段
```

- **用户修改如何保存**：三个层次——(a) `yamls/user.yaml`，在 `merger_yaml()` 中**优先于**生成的 `set.yaml`：对 `mode allow-lan log-level tun experimental external-ui-url interface-name dns store-selected unified-delay` 这些字段，若 user.yaml 里有则先 `sed -i "/^$char/d" set.yaml` 删掉生成的（`:214-221`）；(b) `yamls/{proxies,proxy-groups,rules,others}.yaml` 作为"自定义片段"，用 `#自定义策略组开始/结束`、`#自定义代理`、`#自定义规则` 等**注释锚点**插入（`:129-178, 195-201`）；(c) Dashboard 里的策略组选择另存为 `configs/web_save`（`libs/web_save.sh` 调 `GET /proxies` 提取 `"Selector"` 的 `now`，`libs/web_restore.sh` 启动后逐个 `PUT /proxies/<group>` 还原）。
- **校验与降级**（`test_yaml()`，`:236-250`）：`-t` 失败时把生成的 config 挪成 `error.yaml` 留证，**删掉自定义策略组段落、用 `set_bak.yaml` 重拼一份基础配置**再启动，并提示"自定义配置文件校验失败！将使用基础配置文件启动！"。这是"宁可少功能也要起得来"的降级策略。`[上游源码]`
- **版本化：没有**。全项目与配置备份相关的只有一处：`scripts/starts/core_config.sh:100-105`，订阅拉取成功后 `compare`，若不同则 `mv -f "$core_config" "$core_config".bak && mv -f "$core_config_new" "$core_config"`——**只有一份 `.bak`，且只在订阅更新路径上产生**。没有版本目录、没有 checksum、没有 list/diff/activate/rollback。`[上游源码]`
- **配置存储格式**：`configs/ShellCrash.cfg` 是扁平的 `key=value` 文本，读写靠 `setconfig()`（`libs/set_config.sh:2-6`）：

  ```sh
  sed -i "/^${1}=.*/d" "$configpath"
  printf '%s=%s\n' "$1" "$2" >>"$configpath"
  ```

  **读配置 = `. "$CRASHDIR"/configs/ShellCrash.cfg`（直接 source 进 shell）**（`libs/get_config.sh:6`）——配置值会被当成 shell 代码执行，且 `init.sh:232-244` 用一堆 `sed -i` 做"变量改名/取值统一"的历史迁移。模板来源是仓库内 `rules/clash_providers/*.yaml`、`rules/singbox_providers/*.json`，规则模板是 `rules/*.ini`（Acl4SSR 系）。

### 3.5 订阅管理

**两条互不相同的路径** `[上游源码]`：

**(a) providers 模式（元内核推荐路径）** —— `scripts/menus/providers_clash.sh:84-127` `gen_providers_txt()` 为每个订阅生成 mihomo 原生 `proxy-providers`：

```yaml
  <tag>:
    type: http                     # 或 file（本地文件）
    url: "<订阅地址>"
    path: "./providers/<tag>.yaml"
    interval: <interval2*3600>     # 默认 12h
    health-check:
      enable: true
      lazy: true
      url: "https://www.gstatic.com/generate_204"
      interval: <interval*60>      # 默认 3min
    header:
      User-Agent: ["<ua>"]         # 默认 clash.meta
    override:
      udp: true
      skip-cert-verify: true
    filter: "<include>"
    exclude-filter: "<exclude>"
```

- **节点过滤**：靠 mihomo 原生 `filter` / `exclude-filter`（正则在 `providers.cfg` 里以 `#exclude #include` 形式保存，`6_core_config.sh:116-117`、`providers_clash.sh:98-99`），**不做本地重命名**——重命名/分组交给模板里的 `proxy-groups` 与 `override`。
- 订阅配置持久化在 `configs/providers.cfg`（一行一订阅：`name link interval interval2 ua #exclude #include`）与 `configs/providers_uri.cfg`（分享链接形式的节点）。
- 生成后立刻用内核 `-t` 校验（`providers_clash.sh:64`），成功才落盘到 `yamls/config.yaml`，失败则删掉并提示（`:77-81`）。
- 可单独为一个订阅生成（`gen_providers "$name" "$link" ...`，`6_core_config.sh:257`）。

**(b) subconverter 模式** —— `scripts/starts/core_config.sh:30-108` `get_core_config()`：

```sh
46  Https="${Server}/sub?target=${target}&${Server_ua}=${user_agent}&insert=true&new_name=true&scv=true&udp=true&${urlencodeUrl}"
```
  其中 `urlencodeUrl="exclude=...&include=...&url=...&config=<Acl4SSR .ini 的 URL>"`（`:42-45`），`Server` 取自 `servers_*.list` 中类型 `3|4` 的行（`:33`），实际就是 **第三方公共 subconverter 服务**（`sub.jwsc.eu.org` / `api.v1.mk` / `url.v1.mk`）。也就是说：**它自己不实现订阅转换，而是把用户的订阅 URL + 规则模板 URL 发到别人的公共服务上，取回渲染好的 YAML。** 这是我们要用 `SubscriptionConverter` port 替换掉的核心点——同时也意味着**用户订阅地址会离开本机**，是隐私/可靠性双重风险。

**订阅更新失败如何处理**（`core_config.sh:56-108`）：

```sh
60  if [ "$?" != "0" ]; then
68          if [ -n "$retry" ] && [ "$retry" -ge 3 ]; then
69              logger "无法获取配置文件，请检查链接格式以及网络连接状态！" 31
71              exit 1                        # ← 旧 config.yaml 原样保留
77              update_servers                 # 第 1 次重试：先刷新服务器列表
85              server_link=$((server_link + 1)) # 之后轮换服务器
98          check_config                    # clash_config_check.sh：必须含节点、非旧格式、非 chacha20
100         if [ -s "$core_config" ]; then
101             compare "$core_config_new" "$core_config"
102             [ "$?" = 0 ] || mv -f "$core_config" "$core_config".bak && mv -f "$core_config_new" "$core_config"
```

`check_config()`（`scripts/starts/clash_config_check.sh:4-41`）会验证"确实有节点或 `proxy-providers:`"、"不是旧格式"、"不含 `cipher: chacha20`"、并尝试删除指向 DIRECT 的无效策略组。

**结论**：`订阅下载失败 / 校验失败 → 保留旧配置`这一点 ShellCrash **做对了**（Reuse）；但链条止步于"写入文件"，**没有"激活 → reload → 健康检查 → 失败回滚"**（Improve，正是我们 AGENTS.md 里要求的流程）。

**订阅定时更新**：走 cron（见 §3.8），任务体在 `task/task.sh`；`providers_clash.sh:74` 在生成成功后会 `cronset "$PROVIDERS_CRON_SUB_UPDATE"` 顺手注册定时任务。

### 3.6 TUN / TProxy / iptables / nftables

放在第 4 章单独展开（这是本次调研的重点）。

### 3.7 权限与环境检测

| 检测项 | 实现 | 证据 |
| :--- | :--- | :--- |
| root | 安装时 `[ "$USER" != "root" ] && [ -z "$systype" ]` 则警告并要求确认；运行时大量依赖 `$USER` 与 `cat /proc/1/comm` | `install.sh:311-319`；`init.sh:47`、`start.sh:48` |
| 容器 | `grep -qE '/(docker\|lxc\|kubepods\|crio\|containerd)/' /proc/1/cgroup \|\| [ -f /run/.containerenv ] \|\| [ -f /.dockerenv ]` → `systype=container` | `init.sh:20` |
| 容器缺省值 | `CRASHDIR=/etc/ShellCrash`；`crashcore=meta`、`dns_mod=mix`、`firewall_mod=nftables`、`firewall_area=1`、`start_old=OFF` | `init.sh:22`、`init.sh:162-169` |
| 固件类型 | `Padavan`（`/etc/storage/started_script.sh`）、`asusrouter`（`/jffs`）、`mi_snapshot`（`/data/etc/crontabs/root`）、`ng_snapshot`（`/var/mnt/cfg/firewall` 可写） | `init.sh:5-18` |
| 防火墙后端 | `nft add table inet shellcrash 2>/dev/null && firewall_mod=nftables`，否则 `iptables` | `init.sh:97-101` |
| iptables TPROXY | `modprobe xt_TPROXY` + `iptables -j TPROXY -h 2>/dev/null \| grep -q '\--on-port'` | `fw_iptables.sh:215-217` |
| MARK target | `grep -E '^MARK$' /proc/net/ip_tables_targets` | `fw_iptables.sh:220,237` |
| iptables REDIRECT(ipv6) | `ip6tables -j REDIRECT -h 2>/dev/null \| grep -q '\-\-to-ports'` | `fw_iptables.sh:196,206` |
| nft_tproxy | `modprobe nft_tproxy \|\| lsmod \| grep -q nft_tproxy` | `fw_nftables.sh:176` |
| tun 模块/设备 | `ckcmd modprobe && modprobe tun`；**不做 `/dev/net/tun` 存在性检测**，改为运行后轮询 `ip route list \| grep utun`（最多 29 秒），拿不到就 `logger "找不到tun模块，放弃启动tun相关防火墙规则！"` | `bfstart.sh:142`；`fw_start.sh:18-29` |
| LAN 网段 | `ip route show scope link` 后**排除** `docker|podman|virbr|vnet|ovs|vmbr|veth|vmnic|vboxnet|lxcbr|xenbr|vEthernet|wgs` 等虚拟/网桥接口；取不到则退化为 `192.168.0.0/16 10.0.0.0/12 172.16.0.0/12` 并告警 | `fw_getlanip.sh:5,8,14,39-42` |
| IP 转发 | `firewall_area ∈ {1,3,5}` 时若 `net.ipv4.ip_forward=0` 则写 `/etc/sysctl.conf` 并 `sysctl -w`；容器下额外红字警告 | `fw_start.sh:44`；`userguide.sh:75-86` |
| bridge-nf-call | 新手引导里 `sysctl -w net.bridge.bridge-nf-call-iptables=0` / `-ip6tables=0` | `userguide.sh:88-89` |

**评价**：这套探测是"散落在各处的 `if ckcmd` / `grep -q`"，**结论只以日志形式输出，不落成任何可查询的状态**——没有 `CapabilityStatus` 这类一等公民。我们的 `doctor` 要做的正是把它结构化成 `Supported / Unsupported / Unavailable / Misconfigured / Unknown`。

### 3.8 定时任务

- **只有 crontab，没有 systemd timer**（全仓库无 `.timer` 文件）。`scripts/libs/set_cron.sh` 用 `crond -h` 解析 `/etc/storage/cron/crontabs` → `/var/spool/cron/crontabs` → `/var/spool/cron` 逐个试探可写目录，或退回 `crontab` 命令（`cronadd`/`cronload`，`set_cron.sh:11-28`）。
- **五类触发点**（`scripts/menus/5_task.sh:34-49` `set_service()` 与 `afstart.sh:39-56`）：
  - `cron`：标准 crontab 行 `min hour * * week $CRASHDIR/task/task.sh <id> <name>`（`5_task.sh:26`）
  - `bfstart`：内核启动**前**执行（`bfstart.sh:82`）
  - `afstart`：内核启动**后**执行（`afstart.sh:49`）
  - `running`：每分钟执行（`5_task.sh:40-43`）
  - `affirewall`：**注入到 `/etc/init.d/firewall`**，在 `fw restart`/`fw start` 之后重新应用规则（`afstart.sh:50-56`，用 `sed -i.bak` 改宿主防火墙脚本）
- **每次启动重建 cron 表**（`afstart.sh:39-47`）：`cronload` 取出全部 → 追加 `$TASKCFGDIR/{cron,running}` → 需要时追加守护进程行 → `awk '!x[$0]++'` 去重 → `cronadd`。**去重会顺带删掉用户自己写的重复行**。
- **Docker 环境**单独跑一个 s6 服务 `crond`（`docker/s6-rc.d/crond/run`），不依赖宿主机 cron。
- **并发控制：基本没有**。唯一的锁是保守模式守护进程自己的 `mkdir "$LOCKDIR"`（`start_legacy_wd.sh:4-7`）。全局靠 `start.sh:37-38` 的 `[ -n "$(pidof CrashCore)" ] && $0 stop #禁止多实例` 隐式串行——**同一订阅的并发更新没有任何 per-subscription 保护**。

### 3.9 日志与故障恢复

**日志**（`scripts/libs/logger.sh:8-56`）：

- 终端彩色输出 + 追加到 `/tmp/ShellCrash/ShellCrash.log`，格式 `YYYY-MM-DD_HH:MM:SS~<文本>`；
- **环形裁剪**：`[ "$(wc -l ...)" -gt 199 ] && sed -i '1,20d'`（`:14`）——超过 199 行就砍掉最老 20 行；
- **日志在 `/tmp`**：重启即失，且没有级别字段、没有结构化 key（`subscription_id` / `config_version` 这类）；
- **远程推送**：TG / Bark / PushDeer / Pushover / PushPlus / Gotify / SynoChat 七种，每种都是 `web_json_post ... &` 后台并发 curl（`:16-55`）；日志正文会把订阅相关的 `$device_name` 拼进去，但**不区分敏感字段**；
- 其他日志文件：`$TMPDIR/core_test.log`（启动失败时的内核输出，`start_error.sh:3,6`）、`$TMPDIR/debug.log`（debug 模式，`start.sh:122`）。

**故障恢复**：

| 机制 | 实现 | 评价 |
| :--- | :--- | :--- |
| 启动前自检 | `bfstart.sh`：`.start_error` 检查 → 联网检查 `check_network` → 配置/内核/geo 检查 → 条件任务 → 生成 config | 顺序清晰，但失败只能 `exit 1` |
| 启动失败标记 | `start_error.sh:10` `touch "$CRASHDIR"/.start_error`，随后 `start.sh stop`；`bfstart.sh:11` 见到该文件立即 `exit 1`；`start_legacy_wd.sh:6` 同样跳过 | **自我锁死**：必须人工 `crash -s start`（`start.sh:40` 会 `rm -f .start_error`）或 debug 菜单（`8_tools.sh:694`）解除 |
| 失败取证 | `start_error.sh:2-8`：systemd 下 `journalctl -u shellcrash`，否则直接跑内核 2 秒抓输出，再 `grep -iEo 'error.*=.*\|.*ERROR.*\|.*FATAL.*'` 提取错误行 | 简单有效 |
| 进程守护 | systemd `Restart=on-abnormal RestartSec=10s`；OpenRC `supervise-daemon --respawn-max 0`；procd `respawn`；保守模式 cron 每分钟 watchdog | 四种后端四种做法 |
| 配置降级 | `clash_modify.sh:236-250` `test_yaml()` `-t` 失败 → 丢弃自定义段，用基础配置启动 | **值得 Reuse 的概念** |
| 完全重置 | `fw_stop.sh` 手工逐条拆规则 + `ip rule/route flush` + `ipset destroy` + `nft delete table inet shellcrash` | 见第 4 章 |
| 监控/看门狗 | **没有**"代理不通就摘规则"的连通性看门狗；只有开机时的 `check_network.sh` 联网检测 | 断网事故（issue #864）正是这个缺口的后果 |

### 3.10 交互形态

- **主入口是菜单**：`scripts/menu.sh:206-282` `main_menu()`，九项 ANSI 边框菜单；布局库是自研的 `scripts/menus/tui_layout.sh`（`TABLE_WIDTH=60` + `awk` 逐字符计算显示宽度以对齐中文/ANSI），简化版 `tui_lite.sh` 用 `crash -l` 切换（`menu.sh:24-25`）。
- **TUI 的实质**：是"跑在 sh 里的菜单渲染器"，不是全屏 TUI 应用；**没有 ratatui 那种焦点/刷新模型**。
- **非交互能力非常有限**（`menu.sh:284-358`）：`-l`（lite 布局）、`-t`（shell 语法自检）、`-s <start|stop|restart>`（转发给 `start.sh`）、`-i`（重新初始化）、`-u`（卸载）、`-d`（debug）、`-st`（直接前台启动内核）；其他参数只打印帮助。**没有 `--json`、没有语义化 exit code、没有 `config list/validate/rollback`。**
- **Dashboard**：本地 `ui/` + mihomo 的 `external-ui: ui`（`clash_modify.sh:83`）+ `external-ui-url`；仓库内置 6 种面板压缩包（`bin/dashboard/{zashboard,yacd,meta_db,meta_xd,meta_yacd,clashdb}.tar.gz`），未安装时 `bfstart.sh:26-49` `makehtml()` 生成跳转页指向在线面板。
- **TG Bot**：`tools/tg_bot.sh` + `scripts/menus/bot_tg.sh`（支持管理内核服务、查看日志、上传/下载配置、备份还原脚本设置）；`bot_tg_service=ON` 时由 cron 每分钟守护（`afstart.sh:43`）。
- **DDNS / 内网穿透 / 公网入站**：`scripts/menus/ddns.sh`、`tools/ShellDDNS.sh`、`libs/meta_listeners.sh`、Tailscale/Wireguard 配置（`7_gateway.sh`）。这些超出我们 MVP 范围。

### 3.11 多实例

**明确不支持，且被硬编码禁止**：

- `scripts/start.sh:37-38`：`[ -n "$(pidof CrashCore)" ] && $0 stop #禁止多实例`；`debug` 分支同样（`start.sh:112`）。
- 单例判据是**全局进程名** `pidof CrashCore`（内核在 `/tmp/ShellCrash/CrashCore`，名字固定），因此同一台机器上不可能出现两个实例，也不能按 profile 区分。
- 端口、fwmark、路由表全部是全局单值：`mix_port=7890`、`redir_port=7892`、`tproxy_port=7893`、`db_port=9999`、`fwmark=$redir_port`、`table=100`（`libs/get_config.sh:8-16`）。
- 唯一有"多份"概念的是订阅（`configs/providers.cfg` 多行）。

---

## 4. 网络规则与故障自救机制（重点）

### 4.1 运行模式模型

两个正交维度 `[上游源码]`：

- **劫持方式 `redir_mod`**：`Redir`（nat REDIRECT）/ `Tproxy`（mangle TPROXY + 策略路由）/ `Tun` / `Mix`（TUN 处理 TCP + TPROXY/mark 处理 UDP），另有旁路由专用的 `TCP旁路转发` / `T&U旁路转发`。
- **代理范围 `firewall_area`**：`1`=仅局域网设备、`2`=仅本机、`3`=本机+局域网、`4`=纯净（不劫持）、`5`=主旁转发（`bypass_host`）。
  由 `fw_start.sh:40-45` 折算成 `lan_proxy` / `local_proxy` 两个布尔量，再由 `fw_iptables.sh` / `fw_nftables.sh` 分派。

**策略路由**（`fw_start.sh:16-38`）：

```sh
17  [ "$redir_mod" = "Tproxy" ] && ip route add local default dev lo table $table
18  [ "$redir_mod" = "Tun" -o "$redir_mod" = "Mix" ] && {
20      while [ -z "$(ip route list | grep utun)" -a "$i" -le 29 ]; do sleep 1; i=$((i + 1)); done
24      if [ -z "$(ip route list | grep utun)" ]; then
25          logger "找不到tun模块，放弃启动tun相关防火墙规则！" 31
27      else ip route add default dev utun table $table && tun_statu=true; fi
31  [ "$redir_mod" != "Redir" ] && ip rule add fwmark $fwmark table $table
```

### 4.2 iptables 规则集

**结构**：为 `PREROUTING` / `OUTPUT` 各建一组自建链，链内先 RETURN 白名单、最后 JUMP 到劫持动作，再把系统链的流量 `-I` 进自建链（`fw_iptables.sh:7-96` `start_ipt_route()`）：

- nat 表：`shellcrash`（prerouting v4）/ `shellcrash_out`（output v4）/ `shellcrash_dns` / `shellcrash_dns_out` / `shellcrash_vm` / `shellcrash_vm_dns`
- mangle 表：`shellcrash_mark` / `shellcrash_mark_out`
- ipv6 全部加 `v6` 后缀（`shellcrashv6`、`shellcrashv6_mark` …）

链内 RETURN 顺序（`fw_iptables.sh:23-56`）：

```
- N <chain>
- A <chain> -p tcp --dport 53 -j RETURN          # DNS 单独走 dns 链
- A <chain> -p udp --dport 53 -j RETURN
- A <chain> -m mark --mark $routing_mark -j RETURN    # 防回环（内核自己发出的流量）
- A <chain> -m owner --gid-owner 453 -j RETURN        # OUTPUT 链：按 gid 豁免
- A <chain> -m owner --gid-owner 7890 -j RETURN
- A <chain> -s $bypass_host -j RETURN                 # firewall_area=5
- A <chain> -p tcp -m multiport --dports "$mix_port,$redir_port,$tproxy_port" -j RETURN   # 仅当 common_ports=OFF
- A <chain> -d <HOST_IP> -j RETURN                    # 本机网段
- A <chain> -d <RESERVED_IP> -j RETURN                # 保留地址（0/8,10/8,127/8,100.64/10,169.254/16,172.16/12,192.168/16,224/4,240/4）
- A <chain> -m set --match-set cn_ip dst -j RETURN    # 绕过 CN IP（dns_mod != fake-ip 且 cn_ip_route=ON）
- A <chain> -m mac --mac-source <mac> -j RETURN       # MAC 黑名单
- A <chain> -s <ip_filter> -j RETURN                  # IP 黑名单
[proxy_set] -A <chain> -p <tcp|udp> -s <HOST_IP> -j <JUMP>
           -I <系统链> -p <tcp|udp> [-m multiport --dports <group>] -j <自建链>
```

JUMP 的三种形态（`fw_iptables.sh:191-310`）：`REDIRECT --to-ports $redir_port`、`TPROXY --on-port $tproxy_port --tproxy-mark $fwmark`、`MARK --set-mark $fwmark`。额外动作：`-I FORWARD -o utun -j ACCEPT`（`:258`）、QUIC 屏蔽 `-I FORWARD/-I INPUT -p udp --dport 443 ... -j REJECT`（`:297-310`）、公网防护 `-I INPUT -p tcp -m multiport --dports "$mix_port,$db_port" -j REJECT` + `-I INPUT -i lo -j ACCEPT`（`:156-186`）。

**DNS 链**（`fw_iptables.sh:97-155`）：`-A <dns链> -p tcp -s <网段> -j REDIRECT --to-ports $dns_redir_port`（udp 同），再 `-I PREROUTING -p tcp --dport 53 -j <dns链>`。

### 4.3 nftables 规则集

**结构**：单一表 `inet shellcrash`，钩子链 `prerouting` / `output` / `prerouting_vm`、`*_dns`、`*_mixtcp`、`input`（`fw_nftables.sh:22-134,159-211`）：

```sh
161  nft add table inet shellcrash 2>/dev/null
162  nft flush table inet shellcrash 2>/dev/null         # ← 先清空再逐条 add
...
29   nft add chain inet shellcrash $1 { type $3 hook $2 priority $4 \; }
32   nft add rule inet shellcrash $1 tcp dport 53 return
35   nft add rule inet shellcrash $1 meta mark $routing_mark return
36   nft add rule inet shellcrash $1 meta skgid 7890 return
59   nft add rule inet shellcrash $1 ip saddr != {$HOST_IP} return
47   nft add rule inet shellcrash $1 ip daddr {$RESERVED_IP} return
79-81 nft add set inet shellcrash cn_ip { type ipv4_addr \; flags interval \; }
      nft add element inet shellcrash cn_ip { $CN_IP }
      nft add rule inet shellcrash $1 ip daddr @cn_ip return
97   nft add rule inet shellcrash "$1" "$JUMP"
```

JUMP 形态：`meta l4proto tcp redirect to $redir_port`、`meta l4proto {tcp,udp} mark set $fwmark tproxy to :$tproxy_port`、`meta l4proto {tcp,udp} mark set $fwmark`。

**关键差异点——它会改系统的 `fw4` 表**（`fw_nftables.sh:155-157, 192-196`）：

```sh
155  nft list chain inet fw4 input >/dev/null 2>&1 && \
156  nft list chain inet fw4 input | grep -q '67890' || \
157  nft insert rule inet fw4 input meta mark 0x67890 accept 2>/dev/null
...
192  nft list table inet fw4 >/dev/null 2>&1 || nft add table inet fw4
193  nft list chain inet fw4 forward >/dev/null 2>&1 || nft add chain inet fw4 forward { type filter hook forward priority filter \; }
195  nft list chain inet fw4 forward | grep -q 'oifname "utun" accept' || nft insert rule inet fw4 forward oifname "utun" accept
196  nft list chain inet fw4 input | grep -q 'iifname "utun" accept' || nft insert rule inet fw4 input iifname "utun" accept
```

而清理逻辑（`fw_stop.sh:170`）只有：

```sh
170  ckcmd nft && nft delete table inet shellcrash >/dev/null 2>&1
```

**→ 插进 `inet fw4` 的那三条规则不会被回收**（OpenWrt 的 fw4 表本身在防火墙重载时会重建，所以实践上"被掩盖"了；但在 Debian/nftables 环境下没有 `fw4` 表，这段逻辑不会触发）。这是一个典型的"改别人的表但不负责还原"的设计缺陷。

### 4.4 规则清理：与"加规则"手工对称

`fw_stop.sh` 是整个项目最脆弱的文件：它把 `fw_iptables.sh` + `fw_nftables.sh` 里加过的**每一条**规则用 `-D` 重写一遍（175 行里 19-160 行几乎全是 `-D`），并且要**自己重新推导一遍**默认值（`:10-14` 重新算 `common_ports`/`multiport`/`accept_ports`），因为 `fw_stop.sh` 可能在内核未启动、配置文件缺失的上下文中被调用（`systemd` 的 `ExecStopPost`）。

一处典型的不变量耦合：

- 加规则时链内是否插入"代理端口 RETURN"，取决于 `fw_iptables.sh:34` 的 `[ -z "$ports" ]`；
- 删规则时是否按 `multiport --dports` 分组删，取决于 `fw_stop.sh:23` 的 `[ -n "$ports" ]`；
- 两处 `ports` 都由各自文件顶部的 `common_ports`/`multiport` 默认值推导（`fw_start.sh:8-10` vs `fw_stop.sh:10-12`）。

**只要这三次推导有任何一次不一致，规则就会残留或删错。** 这正是我们要用"声明式规则集 + 生成器 + 统一 teardown"替换的核心风险。

清理还额外处理（`fw_stop.sh:161-174`）：`ipset destroy cn_ip/cn_ip6`、`ip rule del fwmark $fwmark table $table`、`ip route flush table $table`（含 v6 的 `table+1`）、`nft delete table inet shellcrash`、`mv -f /etc/init.d/firewall.bak /etc/init.d/firewall`、`sed -i '/shellcrash-dns-repair/d' /etc/resolv.conf`。

### 4.5 防断网 / 自救机制（现状清单）

| 机制 | 位置 | 说明 |
| :--- | :--- | :--- |
| 保留地址 & 本机网段白名单 | `fw_iptables.sh:39-42`、`fw_nftables.sh:47` | 私网/回环/多播全部 RETURN，避免劫持本机互通 |
| 防回环 | `fw_iptables.sh:29`、`fw_nftables.sh:35` | `meta mark $routing_mark` RETURN，避免内核自身流量被二次劫持成环 |
| 按 gid 豁免 | `fw_iptables.sh:30-31`、`fw_nftables.sh:36,119` | 豁免 gid `453`/`7890`（内核进程） |
| 管理端口护城河 | `fw_iptables.sh:156-186`、`fw_nftables.sh:135-157` | `-I INPUT ... --dports "$mix_port,$db_port" -j REJECT` **先拒外网访问代理/控制器端口**，再放行 `lo`、`host_ipv4`、白名单端口 |
| DNS 修复标记 | `fw_start.sh:50-53` / `fw_stop.sh:174` | 往 `/etc/resolv.conf` 插 `nameserver 127.0.0.1 #shellcrash-dns-repair`，靠注释文本 sed 删除 |
| 启动失败禁自启 | `start_error.sh:10` + `bfstart.sh:11` | **锁死而非回退** |
| TUN 拿不到就降级 | `fw_start.sh:24-26` | 只放弃 tun 相关规则并 log，不阻止内核启动（**正确的降级姿态**） |
| 配置 `-t` 失败降级 | `clash_modify.sh:236-250` | 回落到基础配置 |
| QUIC 屏蔽 | `fw_iptables.sh:297-310`、`fw_nftables.sh:95` | 规避 QUIC 绕过代理 |
| 连通性看门狗 | ❌ **不存在** | 没有"代理 5 秒不通就摘掉规则"的机制；没有定时器校验劫持是否生效 |
| 规则变更原子性 | ❌ iptables 逐条 `-I`/`-A`（非原子）；nftables `flush table` 后逐条 add（中途失败=半套规则） | 见 §4.3 |
| 规则与宿主防火墙隔离 | ❌ 注入 `/etc/init.d/firewall`（`afstart.sh:50-56`）、插 `inet fw4` 规则 | |
| Mihomo 侧不抢路由 | ✅ `clash_modify.sh:9-10` 显式 `tun: {enable: true, stack: system, device: utun, auto-route: false, auto-detect-interface: false}` | 让脚本独占路由决策，避免内核与脚本互相打架——**与我们"Agent 是控制面、Mihomo 是数据面"的边界划分一致** |

---

## 5. PVE/LXC 经验与已知失败模式

### 5.1 上游对 PVE/LXC 的官方立场

- `[上游文档]` README_CN.md:39：**"Docker：部分可能不兼容的设备（如群辉、PVE），支持 docker 环境运行。"** —— PVE 被明确点名为"可能不兼容"，官方推荐用容器方式运行 ShellCrash 本身。README.md:39 英文版同义。
- `[上游文档]` README_CN.md:128-139：虚拟机场景"强烈建议使用 Alpine 镜像"，并给出显式依赖清单：`apk add --no-cache wget openrc ca-certificates tzdata nftables iproute2 dcron` ——**明示需要 nftables + iproute2 + openrc/crond**。
- `[上游文档]` `docker/README.md:1-3`：镜像"用于在容器环境中运行 ShellCrash，支持 HTTP/SOCKS 代理与旁路由透明代理两种部署模式"。

### 5.2 LXC/容器的具体做法（源码）

容器内 ShellCrash 的自适应（`init.sh:19-22,162-178`）：

```sh
20  grep -qE '/(docker|lxc|kubepods|crio|containerd)/' /proc/1/cgroup 2>/dev/null || [ -f /run/.containerenv ] || [ -f /.dockerenv ] && systype='container'
22  [ "$systype" = 'container' ] && CRASHDIR='/etc/ShellCrash'
162 [ "$systype" = 'container' ] && {
163     setconfig userguide '1'
164     setconfig crashcore 'meta'
165     setconfig dns_mod 'mix'
166     setconfig firewall_area '1'
167     setconfig firewall_mod 'nftables'
168-169 setconfig release_type 'master'; setconfig start_old 'OFF'
```

容器下另有两处特殊处理：

- `fw_nftables.sh:164`：`[ "$fw_wan" != OFF ] && [ "$systype" != 'container' ] && start_nft_wan` —— 容器里**不建立公网访问防火墙**（因为不属于它的责任，也是为了避免把宿主/网段拒掉）。
- `fw_start.sh:50`：DNS 修复不作用于 container（`[ "$systype" != 'container' ]`）。
- `fw_getlanip.sh:5,8,14`：识别 LAN 网段时排除 `lxcbr` / `vmbr` / `veth` —— 说明 PVE 的网桥接口曾把"LAN 网段"识别错（把宿主网桥当内网），这正是 LXC 上的典型踩坑点。

Docker 旁路由部署的必要条件（`docker/compose.yml:10-18`）：

```yaml
    cap_add: [SYS_ADMIN, NET_ADMIN, NET_RAW]
    devices: ["/dev/net/tun:/dev/net/tun"]
    sysctls: ["net.ipv4.ip_forward: 1"]
    networks: macvlan_lan  (ipv4_address: 192.168.31.222)
```

即：**TUN 需要挂设备节点，透明代理需要 NET_ADMIN + macvlan/独立 IP + 打开 ip_forward**；`docker/README.md:33-41` 明确"需提前创建 macvlan，这里不推荐使用容器的 host 模式"。

### 5.3 已知失败模式与真实 issue

以下 4 条 issue 的**标题与编号**来自 GitHub 页面（本次调研经 `web_fetch` 直接读取页面 `<title>` 核实）；正文细节未能完整拉取（GitHub 网页内容在抓取时被截断），故只做标题级引用。

| # | 标题 | 与 PVE/LXC 的关联 | 链接 |
| :--- | :--- | :--- | :--- |
| 864 | `[Bug] 特定情况下，会屏蔽路由器所有外部请求` | 防火墙规则把整机对外请求全掐断 —— 正是"规则无看门狗、无自动摘除"的直接后果；在 PVE 宿主/网关上后果放大 | <https://github.com/juewuy/ShellCrash/issues/864> |
| 737 | `[Bug] 启动服务iptables chain_add failed` | 自建链已存在 / 内核模块缺失导致 `chain_add` 失败，服务起不来 —— 与 §4.4 的"手工对称清理"缺陷同源 | <https://github.com/juewuy/ShellCrash/issues/737> |
| 937 | `[Bug] 修改管理面板后死机（CPU和内存迅速接近100）的解决办法` | Dashboard 交互引发资源打满；低配 LXC（内存/CPU 限额）下更易触发 | <https://github.com/juewuy/ShellCrash/issues/937> |
| （外部） | lxc/lxc `#4123` `/dev/net/tun does not exist on any containers not in default directory /var/lib/lxc` | LXC 非默认路径下 `/dev/net/tun` 不存在的通用坑；说明"LXC ⇒ TUN 可用"是错误假设 | <https://github.com/lxc/lxc/issues/4123> |

> `[未验证]` 我在 GitHub 网页搜索 PVE/LXC 关键词时，抓取结果在导航区被截断，未能枚举出更多 PVE 专属 issue 编号；issue 存量统计留待后续用带 token 的 API 补齐。

### 5.4 从 LXC 视角总结的 4 个真实约束

1. **`/proc/1/cgroup` 不是可靠的容器判据**：现代 cgroup v2 下 LXC/Docker 的 cgroup 路径形态与 v1 不同（ShellCrash 的正则要求路径里出现 `/lxc/`、`/docker/` 等目录名）。**必须叠加 `/.dockerenv`、`/run/.containerenv`、`systemd-detect-virt --container`、`/proc/1/environ`、`lxc-*` 等多种信号。** `[未验证]`（未在真实 PVE LXC 上实测命中率）
2. **privileged vs unprivileged 决定一切**：ShellCrash 完全没有这个维度。unprivileged LXC 默认没有 `CAP_NET_ADMIN`、`/dev/net/tun` 可能不存在、写 `/etc/sysctl.conf` 无效——ShellCrash 的应对是"跑起来看哪条命令报错"。我们的 `CapabilityStatus` 必须在**动作之前**把结论算出来。
3. **LAN 网段识别是 PVE 的坑中之坑**：`vmbr0`（宿主网桥）、`veth*`（容器 veth 对）、`fwln*/fwpr*` 都不该被当成"要劫持的 LAN"。ShellCrash 只能靠排除名单（`fw_getlanip.sh:5`）——名单法永远漏。正确做法是显式让用户/配置声明 LAN 网段，并把自动探测结果作为"建议值 + 判定证据"呈现。
4. **容器里不该做的事**：ShellCrash 的容器分支关掉了 `fw_wan`（公网防火墙）和 `resolv.conf` 修复，说明作者也意识到"容器不是宿主"。但**改 `/etc/passwd`、`useradd`、`modprobe`、`sysctl -w`、`/etc/profile` 写入、cron 注入**这些动作在容器里依然照做（`bfstart.sh:128-142`）——在 LXC 里会污染容器模板或直接失败。

---

## 6. ShellCrash Feature Matrix（含 Reuse/Improve/Ignore）

> 处置定义：**Reuse** = 概念上复用/对齐（不抄代码，只对齐问题解法与不变量）；**Improve** = 同类能力必须有，但我们要做得更对；**Ignore** = 明确不做。

| 能力 | ShellCrash 是否具备 | 证据 | 我们的处置 | 理由 |
| :--- | :--- | :--- | :--- | :--- |
| **安装 / 升级** | ✅ 有（在线 tar.gz 覆盖安装） | `install.sh:93-117`；`9_upgrade.sh:135-161` | **Improve** | 必须用**版本化安装 + 原子替换 + 校验和/签名 + 失败回滚**；ShellCrash 是覆盖解压且解压失败即坏（`getscripts()` 无备份），我们要"新版本独立目录 + 原子切换 + 保留上一版" |
| **内核（Mihomo）生命周期** | ✅ 有（start/stop/restart/debug，多后端） | `start.sh:35-129`；`bfstart.sh`/`afstart.sh`/`fw_stop.sh` 三段钩子 | **Reuse** | "启动前自检 → 启动 → 启动后配置网络 → 停止时清理网络"的**三阶段钩子**是干净的；我们把它落到 Application 用例 + Port |
| **内核二进制管理（多内核/多架构）** | ✅ 有 | `core_tools.sh:48-111`；`check_cpucore.sh`；`check_target.sh` | **Reuse** | 架构探测 + 换内核后重写启动命令 + 落盘前先跑一次二进制自检（`core_check` 的"先验证后替换"）值得对齐 |
| **内核二进制完整性校验** | ❌ 无 | `grep sha256/gpg/md5` 命中 0；`core_check()` 只做可执行性/指纹探测（`core_tools.sh:53-59`） | **Improve** | 必须校验 sha256（并在文档里说明上游 mihomo 是否提供校验和），否则镜像/中间人可换内核 |
| **下载源可用性** | ⚠️ 有多个镜像，但依赖公共 CDN 与作者私有源 | `public/servers.list:101-104,202`；`web_get.sh:6-12` 自动改写 jsdelivr/raw 互转 | **Improve** | 我们只把镜像作为**可配置的 fallback**，并记录"当前来源 + 校验结果"；绝不把作者私有源写进默认路径 |
| **TLS 校验** | ❌ 默认关闭 | `install.sh:34,45`；`web_get.sh:25,43` | **Improve** | 默认严格校验，`--insecure` 必须是显式开关并留下审计日志 |
| **配置版本化** | ❌ 无 | 只有 `core_config.sh:102` 的单份 `config.yaml.bak` | **Improve** | 这是本项目第一等公民：不可变版本 + version id + checksum + list/show/diff |
| **配置回滚** | ❌ 无（只有"降级重装脚本"） | `9_upgrade.sh:1227-1267` 选 tag → 覆盖安装 | **Improve** | 回滚必须是"切 active 指针 + reload + 健康检查"，秒级、可重复、不需重新下载 |
| **配置生成前校验** | ✅ 有（内核 `-t`） | `clash_modify.sh:236-250`；`providers_clash.sh:64`；`clash_config_check.sh` | **Reuse** | **最有价值的复用点之一**：任何生成的配置在生效前必须过内核自己的 `-t`；我们要把它做成 `ValidateConfig` 用例（带结构化校验结果） |
| **校验失败降级** | ✅ 有（丢弃自定义段回落到基础配置） | `clash_modify.sh:241-248` | **Reuse**（改进语义） | 概念对：失败要降级到可用状态；但我们要降级到**上一份 known-good 版本**，而不是"去掉用户自定义的基础配置" |
| **用户配置覆盖（声明式）** | ✅ 有（`yamls/user.yaml` 优先于生成值） | `clash_modify.sh:214-221` | **Reuse** | "生成物 + 用户覆盖层"的分层是对的；我们升级为"受管字段白名单 + schema 校验"，不再用 `sed` 按行改 |
| **配置存储格式** | ⚠️ 扁平 `key=value`，且被 `.` source 进 shell | `libs/set_config.sh:2-6`；`libs/get_config.sh:6` | **Ignore** 其实现 / **Improve** 概念 | 配置即 shell 代码执行是安全隐患；我们用 typed config（TOML）+ schema 校验 |
| **订阅（拉取/管理）** | ✅ 有（providers 模式 + 直连配置模式） | `providers_clash.sh:84-127`；`6_core_config.sh`；`configs/providers.cfg` | **Reuse** | 多订阅、每订阅 UA、健康检查 URL、更新间隔、include/exclude 过滤这套字段设计合理，直接对齐 |
| **订阅转换** | ⚠️ 有，但**外包给第三方公共 subconverter** | `core_config.sh:42-46`；`servers.list:401-403`（`sub.jwsc.eu.org` / `api.v1.mk` / `url.v1.mk`） | **Improve** | 必须走 `SubscriptionConverter` port（Sub-Store / sub-store-convert / native），**默认不把用户订阅 URL 交给第三方**；远程转换必须是显式选择项 |
| **订阅更新失败保护** | ✅ 有 | `core_config.sh:60-90`（3 次重试 + 轮换服务器 + `exit 1` 保留旧配置） | **Reuse** | 与 AGENTS.md 的"失败绝不破坏当前配置"完全一致，概念直接对齐 |
| **激活后健康检查 + 自动回滚** | ❌ 无 | 全流程止于文件替换（`core_config.sh:100-105`），无 reload 后探测 | **Improve** | 我们要补齐 `activate → reload → health check → 失败回滚` 的闭环 |
| **TUN** | ✅ 有 | `clash_modify.sh:9-10`（`tun: {enable:true, stack:system, device:utun, auto-route:false, auto-detect-interface:false}`）；`bfstart.sh:142`；`fw_start.sh:18-29` | **Improve** | 概念可复用（内核关 auto-route、由控制面管路由；拿不到 tun 就降级而非整体失败）；但要**先检测 `/dev/net/tun` + `CAP_NET_ADMIN`**，而不是轮询 29 秒后放弃 |
| **TProxy** | ✅ 有（iptables + nftables 双实现） | `fw_iptables.sh:214-250`；`fw_nftables.sh:176-185` | **Improve** | 能力探测（`xt_TPROXY` / `nft_tproxy`）值得复用；但规则要用声明式规则集生成，且必须能与宿主防火墙共存/回滚 |
| **iptables** | ✅ 有 | `fw_iptables.sh`（311 行） | **Improve** | 保留"自建链 + 链内白名单 + 只 `-I` 系统链"的隔离思想；抛弃"加删两处手工对称"和 `/proc/net/ip_tables_targets` 式隐式探测 |
| **nftables** | ✅ 有（1.9.1 起重写，含 Tun/Mix） | `fw_nftables.sh`（211 行）；release 1.9.1 notes "重写nftables，添加tun、混合模式支持" | **Improve** | 单表单事务是正确方向；我们要**一个 nft transaction 原子提交 + 幂等 reconcile**，且不向 `inet fw4` 塞规则（`fw_nftables.sh:192-196`） |
| **规则清理** | ⚠️ 有但脆弱 | `fw_stop.sh:19-160` 逐条 `-D`；`ip rule/route flush`；`nft delete table` | **Improve** | 改为"整表/整链所有权"模型：我们只删自己拥有的 table/chain，绝不做 N 条 `-D` |
| **防断网 / 故障自救** | ⚠️ 部分（白名单 + 启动失败锁死） | `fw_iptables.sh:39-42`；`start_error.sh:10` + `bfstart.sh:11` | **Improve** | 白名单思路复用；但必须有**连通性看门狗 + 超时自动摘规则**（`nft` 单事务 teardown），并把"启动失败"变成"自动回滚 + 降级运行"而不是 `.start_error` 锁死 |
| **PVE / LXC 检测** | ⚠️ 仅粗判定 | `init.sh:20`（`/proc/1/cgroup` + `/.dockerenv`）；`init.sh:162-169` 容器缺省值 | **Improve** | 必须建模 `ContainerEnvironment`（None/Docker/LXC-privileged/LXC-unprivileged/Podman）+ `CAP_NET_ADMIN` + `/dev/net/tun` + cgroup v1/v2，输出显式 `CapabilityStatus` |
| **systemd** | ✅ 有 | `shellcrash.service`（19 行）；`init.sh:47-62`；`start.sh:48-54` | **Improve** | unit 作为**静态打包产物**（Deb 包的一部分）安装，绝不运行期 `sed -i` 改 unit + `daemon-reload`；`User=` 要用真正的非 root 用户 + 精细 `AmbientCapabilities`，而不是 uid 0 + 独立 gid 的伪装 |
| **OpenRC / procd** | ✅ 有 | `shellcrash.openrc`、`shellcrash.procd` | **Ignore**（MVP 后） | MVP 只做 Debian/Ubuntu + systemd；把 InitSystem 建成 domain 枚举即可，不为 OpenRC/procd 提前实现 |
| **s6（Docker 内）** | ✅ 有 | `docker/s6-rc.d/*`；`start.sh:55-59` | **Ignore** | 我们不复刻 ShellCrash 的容器分发形态；如果需要容器化，用自己的 compose 与 entrypoint |
| **定时任务** | ⚠️ 只有 crontab | `libs/set_cron.sh`；`5_task.sh`；`afstart.sh:39-47` | **Improve** | 我们首发用 **systemd timer 或进程内 scheduler**（更可控、有日志、有退出码）；cron 只作为可选后端；必须有 per-subscription 并发互斥 |
| **任务并发保护** | ❌ 基本没有 | 仅 `start_legacy_wd.sh:4-7` 的 `mkdir` 锁；`start.sh:38` 用 `pidof CrashCore` 全局禁多实例 | **Improve** | 必须有 per-subscription / per-instance 锁，重复触发要返回"已在执行"而不是再跑一遍 |
| **日志** | ⚠️ 有（200 行环形 + 推送） | `libs/logger.sh:8-14`（`/tmp/ShellCrash/ShellCrash.log`，`>199` 行删最老 20 行） | **Improve** | 用 tracing + 结构化字段 + 落盘持久化 + 轮转；日志留在 `/tmp` 等于重启即丢，对"事后归因"无价值 |
| **日志推送 / 告警** | ✅ 有（TG/Bark/PushDeer/Pushover/PushPlus/Gotify/SynoChat） | `logger.sh:16-55` | **Ignore**（MVP 后） | 超出 MVP；我们保留"事件流 + 可选 webhook"作为可替换 adapter 即可 |
| **CLI** | ⚠️ 有但是菜单为主 | `menu.sh:284-358`（只有 `-l/-t/-s/-i/-u/-d/-st`） | **Improve** | 我们的 CLI 必须**机器可读优先**：子命令 + `--json` + 语义化 exit code；菜单只能是 TUI，不能是唯一入口 |
| **TUI** | ⚠️ 有（自制 ANSI 菜单 + lite 版） | `menus/tui_layout.sh`（`TABLE_WIDTH=60` + awk 宽度对齐）；`menu.sh:24-25`；`menus/tui_lite.sh` | **Reuse**（问题意识）/ **Improve**（实现） | CJK 显示宽度对齐这个问题我们一定会遇到，`tui_layout.sh` 的处理**思路**可参考；但实现要用 ratatui，且 TUI 只调度 Application 命令 |
| **Dashboard** | ✅ 有（本地 6 种面板 + 在线回退） | `bin/dashboard/*.tar.gz`；`clash_modify.sh:83-84`（`external-ui: ui`、`external-ui-url`）；`bfstart.sh:26-49` `makehtml()` | **Reuse**（概念） | 「本地内置 dashboard 静态资源 + `external-ui` 指向它 + 缺失时给出跳转页」是对的；我们按 AGENTS.md 直接集成 metacubexd，不自研 |
| **Dashboard 选择持久化** | ✅ 有（`configs/web_save` + 启动后 PUT 还原） | `libs/web_save.sh`、`libs/web_restore.sh`；`afstart.sh:29-32` | **Reuse** | 这是很实际的坑：面板里手选的策略组在重启后会被配置重写冲掉；我们应把"运行时选择"作为**独立于配置版本的状态**持久化并自动还原 |
| **Doctor / 环境自检** | ⚠️ 只有零散自检菜单 | `8_tools.sh:572-693` `testcommand()`（debug/端口占用/openssl 性能等）；能力探测散落各处（§3.7） | **Improve** | 我们要的是**单一 `doctor` 用例**：一次性算出 Platform/Arch/InitSystem/ContainerEnvironment/Capabilities 并给出 `Supported/Unsupported/Unavailable/Misconfigured/Unknown`，且可 `--json` |
| **多实例** | ❌ 明确禁止 | `start.sh:37-38` `pidof CrashCore` + `#禁止多实例`；端口/fwmark/table 全为全局单值（`get_config.sh:8-16`） | **Improve** | AGENTS.md 把 multi-instance 列为可延后，但**领域模型必须从一开始就带 `MihomoInstanceId`**，否则端口/路由/规则全是全局单例，后期无法演进 |
| **权限隔离** | ❌ 无（服务账户 uid=0） | `init.sh:49-57`（`sed -Ei s/7890:7890/0:7890/g /etc/passwd`）；`shellcrash.service:7` | **Improve** | 真正的分离：Agent 自身非 root 常驻，特权操作走受控的端口/能力，绝不暴露任意命令执行 |
| **控制器暴露面** | ❌ 默认 0.0.0.0 | `clash_modify.sh:82` `external-controller: :$db_port`（第 7 行还准备了未使用的 `0.0.0.0:$db_port` 变量）；仅靠 `secret` + INPUT REJECT | **Improve** | 默认 `127.0.0.1` 或 Unix socket（与 AGENTS.md 一致）；远程访问必须显式开启并配认证 |
| **宿主污染面** | ❌ 大 | `sed -i` 改 `/etc/passwd`、`/etc/group`、`/etc/profile`、`/etc/resolv.conf`、`/etc/sysctl.conf`、`/etc/init.d/firewall`(.bak)、`nvram`、`uci`；`useradd`/`userdel`；`modprobe` | **Improve** | 所有系统级修改必须收敛为**幂等的、可枚举、可撤销**的 `SystemIntegration` 操作，并记录"我们改了什么"以便卸载时精确还原 |
| **Bypass / 旁路由 / DDNS / TG Bot / 内网穿透** | ✅ 有 | `7_gateway.sh`；`menus/ddns.sh`；`tools/tg_bot.sh`；`menus/8_tools.sh` | **Ignore** | 明确不在 MVP 范围（"reliability > feature count"） |

---

## 7. 我们不应重复的设计（教训清单）

每条都给出源码证据，不做印象式判断。

1. **覆盖式自更新 / 覆盖式覆盖安装**
   `9_upgrade.sh:144-159`：`start.sh stop` → `tar -zxf` 覆盖到 `$CRASHDIR` → `init.sh` → `exit`。中间失败只提示换源（`error_down()`），**旧的可用版本已经被覆盖掉一半**。
   `install.sh:347`："卸载旧版本并安装" = `rm -rf "$CRASHDIR"`。
   → 我们要"新版本落独立目录 + 原子切换 + 保留上一版 + 校验和"。

2. **默认关闭 TLS 校验 + 无任何产物校验**
   `install.sh:34`（`curl ... -ko`）、`install.sh:45`（`--no-check-certificate`）、`web_get.sh:25,43`；全仓库 `sha256|gpg|md5` 命中 0。
   → 默认严格校验；`insecure` 必须是显式、被记录的选择。

3. **把网络规则写两遍（加一遍、删一遍），靠人工保持对称**
   `fw_iptables.sh:24-96` 与 `fw_stop.sh:19-88`；`fw_nftables.sh:29-97` 与 `fw_stop.sh/170`；`ports`/`common_ports`/`accept_ports` 在 `fw_start.sh:8-14`、`fw_stop.sh:10-14`、`fw_iptables.sh:34` 三处各自推导。
   → 我们要"声明式规则集 + 生成器 + 统一 teardown + reconcile 校验"。

4. **向不属于自己的防火墙表插入规则，却不负责回收**
   `fw_nftables.sh:155-157,192-196` 向 `inet fw4` 插入 3 条规则；`fw_stop.sh:170` 只 `nft delete table inet shellcrash`。
   `afstart.sh:50-56` 还用 `sed -i.bak` 往 `/etc/init.d/firewall` 里插 `affirewall`。
   → 我们只拥有自己的 table/chain，绝不改别人的表。

5. **启动失败即自我锁死，而不是自动回滚**
   `start_error.sh:10` `touch .start_error` → `bfstart.sh:11` `exit 1`（含开机自启路径）→ 只能靠人工 `crash -s start`（`start.sh:40`）或 debug 菜单（`8_tools.sh:694`）解锁。
   → 我们要"回滚到上一份 known-good 配置 + 重试一次 + 明确告警"。

6. **配置没有版本，只有一份 `.bak`**
   `core_config.sh:100-105` 是唯一的备份点。没有 version id、checksum、diff、activate、rollback。
   → 配置版本化 + 原子激活 + 回滚是 MVP 核心。

7. **运行期修改 systemd unit 文件**
   `start.sh:48-53`：从 `systemctl show -p FragmentPath` 拿到 unit 路径，`sed -i "s#^ExecStart=.*#ExecStart=$COMMAND ...#"`，再 `daemon-reload`。
   另外 `init.sh:59-60` 安装时用 `sed` 把 `/etc/ShellCrash` 替换成实际 `$CRASHDIR`。
   → unit 应是静态打包产物；变化的东西放 `/etc/proxy-agent/config.toml` 或 drop-in，而不是 `sed` 主 unit。

8. **服务账户假隔离（uid 0 + 独立 gid）**
   `init.sh:52-56`：`useradd shellcrash -u 7890` 后 `sed -Ei s/7890:7890/0:7890/g /etc/passwd` → uid 变 0；`shellcrash.openrc:30` 用 `shellcrash:x:0:7890` 做判据；独立 gid 的用处仅是防火墙 `--gid-owner` 豁免（`fw_iptables.sh:30-31`）。
   → 我们要真正的权限分离：非 root 常驻 + 受控特权操作。

9. **改宿主系统文件而不留"改了什么"的账本**
   `sed -i` 目标包括：`/etc/passwd`、`/etc/group`（`init.sh:49-57`）、`/etc/profile`、`~/.bashrc`、`~/.zshrc`（`init.sh:107-119`）、`/etc/resolv.conf`（`fw_start.sh:52` / `fw_stop.sh:174`）、`/etc/sysctl.conf`（`fw_start.sh:44`）、`/etc/hosts`（只读，`clash_modify.sh:106-115`）、`/etc/init.d/firewall`（`afstart.sh:53-55`）、`uci`/`nvram`（`init.sh:142-148,155-160`）。还原只靠固定的 `sed '/pattern/d'`，pattern 一旦变化就永久残留。
   → 每个系统级改动必须登记（改哪个文件、加了什么标记、如何撤销），卸载时按账本逆向。

10. **日志放 `/tmp` + 200 行环形 + 无结构**
    `logger.sh:9-14`：`TMPDIR=/tmp/ShellCrash`；`>199` 行时 `sed -i '1,20d'`。无 level、无结构化字段。
    → tracing + 持久化 + 轮转 + 结构化字段（`config_version`、`subscription_id`、`job_id`）。

11. **菜单是唯一入口，自动化能力几乎为零**
    `menu.sh:284-358` 只支持 `-l/-t/-s/-i/-u/-d/-st`；无 JSON 输出、无语义化 exit code、无 `config list/validate/rollback`。
    → CLI 必须自动化优先。

12. **配置生成靠 `sed` 猜缩进 + 靠注释锚点**
    `clash_modify.sh:136`（`sed "s/^ */${space_name}  /g"`）、`:145-150`（用 `grep -A 8 "\- name: $name" | grep -n "proxies:$"` 计算插入行号）、`:129`（`#自定义策略组开始/结束` 锚点）。YAML 结构稍有变化就静默插错位置。
    → 用真正的 YAML/JSON 数据模型（serde）生成配置，绝不做文本拼接。

13. **解析下载内容用 `grep`/`sed` 而不是 JSON 解析**
    `9_upgrade.sh:335-341`：`grep '"tag_name":' | awk -F '"'`、`grep "browser_download_url" | grep -oE "...linux.*${cpu_type}.*\.(gz|upx)\""` 解析 GitHub API；`:1233` 解析 tags 列表同理。
    → 我们用 reqwest + serde 解析 API 响应。

14. **全局单实例 + 全局单命名空间**
    `start.sh:37-38`（`pidof CrashCore`）；端口/fwmark/table 全是全局单值（`get_config.sh:8-16`）。
    → 领域模型从一开始就带 instance id，端口/规则/路由按实例派生。

15. **依赖单点外部服务且没有降级语义**
    订阅转换依赖 `sub.jwsc.eu.org` / `api.v1.mk` / `url.v1.mk`（`servers.list:401-403`）；脚本更新依赖 `api.github.com` 取 tags（`9_upgrade.sh:1233`，实测该接口在未认证时限流即返回 403，回退功能直接不可用）；资源下载依赖 jsdelivr + 作者私有源。
    → 每个外部集成都必须是可替换 adapter（`SubscriptionConverter`），并有明确的失败语义与降级路径。

---

## 8. 对 Agent 架构的影响

把上面的结论映射到本项目的分层与 Port（不写 ADR，只列影响点）：

**Domain**

- `Configuration` 域：`ConfigVersion`（版本 id + checksum + 来源 `ConfigSource`）+ `ActivationState`。ShellCrash 用单份 `.bak`（`core_config.sh:102`）证明"没有版本模型"会立刻退化成人肉操作；新版优先。
- `System` 域：`Platform` / `Architecture` / `InitSystem`（含 `Systemd`；`OpenRc`/`Procd` 先入枚举不实现）/ `ContainerEnvironment`（`None`/`Docker`/`Podman`/`LxcPrivileged`/`LxcUnprivileged`）/ `CapabilityStatus`（`Supported`/`Unsupported`/`Unavailable`/`Misconfigured`/`Unknown`）。ShellCrash §3.7 的探测结论只进日志，我们要让它们成为领域值。
- `Subscription` 域：`Subscription` / `SubscriptionSource` / `ConverterId` / `Schedule`。`providers.cfg` 的字段设计（name/link/interval/health-check/UA/include/exclude）可直接对齐为领域属性。

**Application（用例）**

- `UpdateMihomo`：对齐 `core_tools.sh:48-87` 的"**下载 → 本地可执行性自检 → 通过才替换**"，并补 checksum。
- `ValidateConfig`：直接对齐 `clash_modify.sh:236-250`（调用内核 `-t`）；输出结构化 `ValidationResult`，不只打印错误行。
- `ActivateConfig` / `RollbackConfig`：ShellCrash 缺失的那一环；必须 `write temp → fsync → atomic rename → activate → reload → health check →（失败）rollback`。
- `UpdateSubscription`：**保留** ShellCrash 已做对的"失败不覆盖当前配置"（`core_config.sh:60-90`），并补齐"激活后健康检查"。
- `RunDoctor`：把 §3.7 的零散探测一次性算清并结构化输出（`--json`）。
- 并发：`AGENTS.md` 要求的"同一订阅不得并发更新"，恰好是 ShellCrash 的空白点（它只有 `pidof CrashCore` 全局单例）。

**Ports / Infrastructure**

- `ProcessManager`：ShellCrash 的 systemd 路径是"运行期 sed 改 unit"（`start.sh:48-53`），反例明确——systemd 集成必须以**静态 unit + `systemctl` 调用**实现，unit 由打包产生。
- `NetworkRuleManager`（nftables/iptables）：必须支持 `plan → apply(原子) → verify → teardown(按所有权)`。
  - 从 `fw_iptables.sh`/`fw_nftables.sh` 复用：**自建链/自建 table 命名空间**、链内白名单顺序（DNS → 防回环 mark → gid 豁免 → 保留地址 → LAN 网段 → CN 绕过 → 用户过滤 → JUMP）、`ip rule fwmark + table` 策略路由骨架、内核侧 `auto-route: false` 让控制面独占路由。
  - 必须替换：逐条 `-I`/`-D` 的对称维护、`inet fw4` 注入、`/etc/resolv.conf` 的注释式修补（改为不依赖系统 resolver，或用 systemd-resolved drop-in）。
- `CapabilityProbe`：把 §3.7 的探测项（`modprobe tun` 后 `ip route | grep utun`、`xt_TPROXY`/`nft_tproxy`/`MARK`/`REDIRECT` 可用性、`sysctl ip_forward`）变成显式实现，**在动作前**给出结论，而不是"跑起来看哪条报错"。
- `SubscriptionConverter`：默认 `NativeConverter` / `SubStoreConverter`；远程公共 subconverter 只能是显式配置项（对应 `servers.list:401-403` 的反例）。
- `Scheduler`：不用裸 crontab（`set_cron.sh`）；用进程内 scheduler 或 systemd timer，带 per-subscription 锁与结构化日志。
- `ConfigStore`：不可变版本目录（对齐 AGENTS.md 的 `/var/lib/proxy-agent/configs/vNNN.yaml` + `active` 指针），把 ShellCrash 的 `yamls/user.yaml` **覆盖层概念**保留下来，但用 serde 数据模型实现合并。
- 日志/可观测：`tracing` + 文件持久化 + 轮转（对着 `logger.sh:9-14` 的 `/tmp` + 200 行环形做反面）。

**Interfaces**

- CLI：自动化优先（`--json`、语义化 exit code）。ShellCrash 的 `menu.sh:284-358` 说明"菜单即入口"会把自动化需求逼到 shell 拼字符串。
- TUI：ratatui，且只调度 Application 用例。`tui_layout.sh` 值得一读的点是 **CJK/ANSI 显示宽度对齐**（它用 awk 逐字符算宽度），这是我们会真实遇到的坑。
- Dashboard：沿用 metacubexd + `external-ui` 指向本地静态资源的模式（`clash_modify.sh:83-84`），并**照抄它的教训**：保留"面板运行时选择"的独立持久化与启动后还原（`web_save.sh`/`web_restore.sh`），否则重启即丢。

**Security**

- Mihomo controller 默认 `127.0.0.1` 或 Unix socket（对照 `clash_modify.sh:82` 的 `external-controller: :$db_port`，`db_port` 默认 9999，空地址等价监听所有接口）。
- 真正的权限分离（对照 `init.sh:52-56` 的 uid 0 伪装）；privileged 操作映射到显式用例，不做任意命令执行。

---

## 9. 证据与来源

### 9.1 上游源码（本次只读解包，`scripts/` 与仓库根）

仓库快照：`juewuy/ShellCrash` master 分支 archive，解包于 `/tmp/r08-shellcrash/ShellCrash-master`（仅解出文本文件，`bin/{meta,singboxp,singboxr,dashboard,geodata,hfs,fix}` 二进制未解包）。脚本总量 **12,027 行**（`find scripts -name '*.sh' | xargs wc -l`）。

| 主题 | 文件:行 | 关键点 |
| :--- | :--- | :--- |
| 安装 | `install.sh:11,93-117,210-266,280-295,311-319,333-367` | 源、payload 下载解压、目录选择、版本通道、root 检查、覆盖安装 |
| init 分派 | `scripts/init.sh:41-72` | procd / systemd / OpenRC / 保守模式 |
| 容器检测与缺省 | `scripts/init.sh:19-22,162-178` | cgroup/`.dockerenv` 判定、容器缺省值 |
| 服务账户 | `scripts/init.sh:49-57`；`starts/shellcrash.openrc:30` | uid 0 + gid 7890 |
| systemd unit | `scripts/starts/shellcrash.service:1-19` | `User=shellcrash`、`ExecStart(Pre/Post)`、`ExecStopPost=fw_stop.sh`、`Restart=on-abnormal` |
| 运行期改 unit | `scripts/start.sh:37-66,81-96` | `sed -i` 改 `ExecStart` + `daemon-reload`；禁多实例 |
| 启动前/后钩子 | `scripts/starts/bfstart.sh`、`afstart.sh` | 自检、条件任务、cron 重建、防火墙注入 |
| 内核管理 | `scripts/libs/core_tools.sh:8-111`、`starts/check_core.sh`、`libs/web_get_bin.sh`、`libs/check_cpucore.sh`、`libs/check_target.sh` | 下载源、存储策略、可执行性自检、架构探测 |
| 配置生成 | `scripts/starts/clash_modify.sh:4-270` | set/user/others 分层、sed 合并、`-t` 校验与降级 |
| 订阅（providers） | `scripts/menus/providers_clash.sh:84-127` | proxy-providers 字段、filter/exclude-filter |
| 订阅（subconverter） | `scripts/starts/core_config.sh:14-108` | `/sub?target=...&config=<ini>` 拼接、重试、备份替换 |
| 订阅校验 | `scripts/starts/clash_config_check.sh:4-41` | 节点存在性/旧格式/chacha20/无效策略组 |
| 配置读写 | `scripts/libs/set_config.sh:2-6`、`libs/get_config.sh:1-24` | `key=value` + source 执行 |
| iptables | `scripts/starts/fw_iptables.sh:1-311` | 自建链、白名单、JUMP 三形态、能力探测 |
| nftables | `scripts/starts/fw_nftables.sh:1-211` | 单表、`fw4` 注入 |
| 策略路由与范围 | `scripts/starts/fw_start.sh:1-58` | `ip rule/route`、`firewall_area` 折算、resolv.conf 修补 |
| 规则清理 | `scripts/starts/fw_stop.sh:1-175` | 逐条 `-D`、flush、还原 firewall.bak |
| LAN 网段 | `scripts/starts/fw_getlanip.sh:1-50` | 排除 lxcbr/vmbr/veth 等 |
| 定时任务 | `scripts/libs/set_cron.sh:1-41`、`scripts/menus/5_task.sh:15-49`、`starts/afstart.sh:39-56` | crontab 探测、五类触发点、cron 重建 |
| 日志 | `scripts/libs/logger.sh:1-56` | `/tmp` 环形日志 + 7 种推送 |
| 故障 | `scripts/starts/start_error.sh:1-14`、`starts/start_legacy_wd.sh:1-30`、`starts/check_autostart.sh` | `.start_error` 锁死、保守模式 watchdog |
| 工具/自检 | `scripts/menus/8_tools.sh:76-200,572-720` | tools 菜单、`testcommand()`、`debug()` |
| TUI | `scripts/menus/tui_layout.sh:1-45`、`menus/tui_lite.sh`、`menu.sh:24-25,206-282,284-358` | ANSI 布局库、lite 版、菜单与 CLI 参数 |
| 面板 | `scripts/libs/web_save.sh`、`libs/web_restore.sh`、`menus/9_upgrade.sh:38`、`starts/bfstart.sh:26-49` | 选择持久化、PAC、跳转页 |
| 卸载 | `scripts/menus/uninstall.sh:7-75` | 保留配置的卸载流程 |
| 服务端清单 | `public/servers.list`、`public/servers_chs.list`、`public/task_chs.list` | 镜像/转换服务/任务模板 |
| Docker | `Dockerfile:1-80`、`docker/compose.yml:1-35`、`docker/README.md:1-60`、`docker/s6-rc.d/*` | macvlan + cap + /dev/net/tun + s6 |
| 规则模板 | `rules/*.ini`、`rules/clash_providers/*.yaml`、`rules/singbox_providers/*.json` | Acl4SSR 系模板与 proxy-groups 骨架 |
| 版本 | `version`（`1.9.5beta3`）、`bin/release_version`（首行 `1.9.1` + 历史列表）、`bin/version`（`meta_v`/`singboxr_v`/`versionsh`/`GeoIP_v` 变量） | 版本元数据分散在三个文件 |

### 9.2 上游文档

- `README_CN.md:24,33-39,45-152,165-172,183-185`（平台、Docker/PVE 立场、依赖、GPL-3.0）
- `README.md:39,136-140`（英文版同义）
- `docker/README.md:1-60`（容器部署模式、cap 与 sysctl）
- `LICENSE.txt`（GPLv3 全文）
- `https://img.shields.io/github/stars/juewuy/ShellCrash.json`（stars=13k，读取于 2026-09-12）

### 9.3 上游 issue / release

- `https://github.com/juewuy/ShellCrash/issues/864` — `[Bug] 特定情况下，会屏蔽路由器所有外部请求`（页面 `<title>` 已核实）
- `https://github.com/juewuy/ShellCrash/issues/737` — `[Bug] 启动服务iptables chain_add failed`（页面 `<title>` 已核实）
- `https://github.com/juewuy/ShellCrash/issues/937` — `[Bug] 修改管理面板后死机（CPU和内存迅速接近100）的解决办法`（搜索结果标题）
- `https://github.com/lxc/lxc/issues/4123` — `/dev/net/tun does not exist on any containers not in default directory /var/lib/lxc`（外部参照）
- `https://github.com/juewuy/ShellCrash/releases.atom` — `1.9.4-release`（2026-02-15）、`1.9.3-release`、`1.9.1-release`、`1.9.0-release`：容器化/s6/OpenRC、nftables 重写、tag 回退等自述
- `https://github.com/juewuy/ShellCrash/commits/master.atom` — master 最近提交 `eb0c211`（2026-08-02T02:42:00Z）、`78ad72f`（~1.9.5beta3 pkg）

### 9.4 采集方式与环境限制（可复现性说明）

- 直连 `github.com:443` 超时 → clone/payload 经代理 `https://ghfast.top/https://github.com/juewuy/ShellCrash/archive/refs/heads/master.tar.gz` 获取（223,675,880 bytes，gzip）。
- `api.github.com` 未认证限流耗尽（实测 `403`）→ release/commit 事实改从 Atom feed 与 shields.io 获取；`9_upgrade.sh:1233` 的 tag 回退接口因此**实测不可用**，这本身也是一条证据（单点依赖）。
- **全程未执行任何安装脚本**：只做 `tar -tzf` / `tar -xzf`（限定 exclude 二进制目录）与文本读取；未 `bash install.sh`、未 `sudo`、未改宿主网络。
- GitHub issue 正文抓取被页面导航截断，故 issue 只做标题级引用。

---

## 10. 未验证假设与开放问题

1. `[未验证]` **PVE LXC 下容器判定的实际命中率**：`init.sh:20` 的 `grep -qE '/(docker|lxc|kubepods|crio|containerd)/' /proc/1/cgroup` 依赖 cgroup v1 风格路径。**Proxmox VE 7/8 的 LXC 使用 cgroup v2，`/proc/1/cgroup` 往往只有一行 `0::/`**，正则可能不命中 → 会被误判为宿主。需在真实 PVE LXC（privileged 与 unprivileged 各一）上实测。这是本次调研最需要后续验证的一条。
2. `[未验证]` unprivileged LXC 中 ShellCrash 的实际表现（`modprobe tun` 是否会失败、`/dev/net/tun` 是否可写、nftables 是否可用）——本次未搭建实验环境（Docker Hub 亦不可达，未做容器实验）。
3. `[未验证]` `bin/release_version`（首行 1.9.1 + 历史版本列表）的**消费者**未找到——全仓库检索未见读取该文件的调用点；`bin/version` 则是被 `.` source 的 shell 变量文件（`9_upgrade.sh:114-122`），用于 `core_v_new=$(eval echo \$"$crashcore"_v)` 取内核版本（`9_upgrade.sh:25`）。`bin/release_version` 是否为历史遗留待确认。
4. `[未验证]` ShellCrash issue 中 PVE/LXC 专属 issue 的**完整清单与数量**：GitHub 搜索页抓取被导航区截断，未获得结果列表；需要带 token 的 API 才能可靠枚举。
5. `[未验证]` `fw_stop.sh` 在"规则形态与生成时不一致"（例如用户中途改了 `multiport`、切换了 `firewall_area`、或内核升级引入新规则）时残留规则的具体数量与后果——只从代码推断了风险，未做实验。
6. `[未验证]` `nftables` 路径下 `fw4` 注入规则在 Debian/Ubuntu（无 `fw4` 表）下的行为：`fw_nftables.sh:155-157` 用 `nft list chain inet fw4 input` 做了存在性判断，理论上不会误建，但 `:192-196` 在 `tun_statu=true` 时会**主动创建 `inet fw4` 表/链**（`nft list table inet fw4 >/dev/null 2>&1 || nft add table inet fw4`）——在 Debian 上这意味着它会**凭空造出一个 `fw4` 表**。需实测确认（读代码倾向成立）。
7. `[未验证]` `web_save`/`web_restore` 在 `external-controller` 使用 `secret` 时的鉴权行为（`web_save.sh` 用 `Authorization: Bearer ${secret}`，而 mihomo 的 `secret` 语义是否为固定格式 bearer，需与 R01/R02 的 Mihomo API 调研结论交叉验证）。
8. `[待定]` **是否要在 MVP 支持 `Mix`（TUN + TPROXY 混合）模式**：ShellCrash 在 `firewall_area ∈ {1,2,3}` 下把它作为默认（`userguide.sh:43`），复杂度显著高于纯 TUN；AGENTS.md 已把"advanced TProxy automation"列为可延后，建议 MVP 只做 `Redir`/`TProxy` 的能力探测与 TUN 主路径，Mix 留待 PVE 实测后再定。
9. `[待定]` 由于 ShellCrash 为 **GPL-3.0**，本调研**只做行为/概念级的 Feature Reverse Engineering，未复制任何代码、注释或文本片段**（文档中引用的均为短小的规则/行为描述）。若后续打算复用其规则模板文件（`rules/*.ini`、`rules/clash_providers/*.yaml`）或直接分发其产物，须按 `docs/research/13-licenses.md` 的结论先做 license 评估。
