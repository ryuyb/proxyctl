# R02 — Mihomo 配置生命周期

> 状态：已完成（由原调研证据重建 + 补充实测） | 调研日期：2026-09-12 | 证据等级：实测 + 上游源码 + 上游文档
> 关键结论一句话：`mihomo -t` 与启动走**同一条 `executor.Parse*` 路径**，因此它是可靠的**语法+语义门禁**，但它**不覆盖未知字段、TUN 可用性、provider 可达性**，且**有副作用（会联网下载 geodata、会创建缺失的 `-f` 目标文件）**；更严重的是 `PUT /configs?force=true` 在 listener 重绑失败时会**返回 204 却拆掉旧监听器**，使进程落入"控制器存活、数据面死亡"的僵尸态 —— Agent **绝不能把 reload 当作可回滚的原子操作**。

---

## 1. 结论摘要（TL;DR）

1. **`-t` 与启动共用同一解析路径，退出码语义明确**：`main.go` 中 `-t` 分支调用 `executor.Parse()` / `ParseWithBytes()`，与正常启动的 `hub.Parse()` 之前的解析阶段等价。[上游源码] 实测：合法配置 `exit 0` + `stdout: "configuration file ... test is successful"`；非法配置 `exit 1` + `stdout: "... test failed"`。[实测]

2. **`-t` 的覆盖范围是"解析期错误"，不含未知字段**：14 个 case 中 8 个被拦截（YAML 语法、类型、`mode` 枚举、proxy/rule-provider 引用、规则格式、geodata 资源），但 `04-unknown-field.yaml`（`mixed-portt`，即字段名拼错）**`exit 0` 通过**。[实测] 这意味着 **Level 2 校验必须由 Agent 自己补字段白名单**，`-t` 单独不足以发现拼写错误 —— 这是 Q001 的直接答案。

3. **`-t` 有副作用，不能视为纯 dry-run**：① 配置中出现 `GEOIP`/`GEOSITE` 规则时，`-t` **会真实触发 geodata 下载**（实测 `12-ruleset-geoip` 阻塞约 90s 后因超时失败，落盘 `geoip.metadb` 4.1MB；`13-geosite` 阻塞约 75s）；② 实测 `-t -f <不存在的文件>` **返回 exit 0 并"成功"**，因为 mihomo 会自动创建一份初始配置到该路径 —— **这是 Agent 校验门禁最危险的假阳性**。[实测]

4. **`-t` 不写 `cache.db`，正式启动会写**：在空 `-d` 目录下跑 `-t` 后目录内容为空；而 `mihomo -d <dir>` 正式启动会生成 `cache.db`（65536 字节）。[实测] 因此 `-t` 的"零残留"只针对 cache，**不针对 geodata 与外部 UI**。

5. **reload 对"解析错误"是安全的（旧配置保留），对"绑定错误"是毁灭性的**：`PUT /configs` 的 `executor.ParseWithPath` 失败 → **HTTP 400 + 旧配置完整生效 + 进程存活**（实测 11 个边界用例全部复现）。但若新配置**解析通过而 listener 绑定失败**：`?force=false` → 返回 204 且**旧端口继续监听**（安全降级）；`?force=true` → 返回 204、**旧监听器被拆除、新端口绑定失败、`/configs` 显示 `mixed-port: 0`，且后续合法 reload 再也无法恢复**，必须**重启进程**。[实测]

6. **reload 不是事务性的**：`ApplyConfig` 内部按"先 `updateListeners(..., force)` 拆除、再绑定"的顺序执行，失败不回滚。[上游源码] 这与 01-mihomo.md 结论 6「reload 是先解析后应用、失败不改现状」**并不矛盾但需要收紧**：那一结论只对**解析失败**成立；对**应用阶段失败**（bind/权限/资源）不成立。

7. **路径白名单是硬门禁**：`path` 必须是绝对路径且在 `homeDir` 或 `SAFE_PATHS` 内，否则 400 并回显允许路径；相对路径 400；文件不存在 400。`SAFE_PATHS` 以 `filepath.SplitList`（Unix 为 `:`）解析，且 `homeDir` 被自动追加进白名单。[实测][上游源码]

8. **上游没有机器可校验的官方 Schema**：`tree.json`（tag-pinned 完整树，1222 项，`truncated: false`）中**不存在 JSON Schema / VSCode schema 文件**；`config/` 目录只有 4 个 Go 文件；仅在 `config/config.go` 里有 YAML tag 作为"事实 schema"。[上游源码] 故 **Level 2 只能依赖 `mihomo -t` + Agent 自建字段白名单**（从 `config.go` 的 yaml tag 生成）。

---

## 2. 实测环境与版本

| 项 | 值 | 证据 |
|---|---|---|
| 二进制 | `/tmp/r02-config/mihomo` | [实测] |
| 版本 | `Mihomo Meta v1.19.30 darwin arm64 with go1.26.6 Sun Aug 16 10:01:05 UTC 2026` | [实测] `./mihomo -v` |
| Build tags | `with_gvisor` | [实测] |
| 运行平台 | macOS (darwin), arm64 | [实测] |
| 上游 tag | [`v1.19.30`](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30) | [上游文档] `latest.json` → `"tag_name": "v1.19.30"` |
| tag 对应 commit | `ac017cdd246ce8bd547653d927e7bf77d7ee73d5` | [上游源码] `tree.json` → `"sha"` |
| 目标平台差异 | Agent 目标为 Linux/LXC，本机为 macOS | [未验证] 见 §11 |

> **平台差异警告**：所有实测均在 **darwin/arm64** 完成。mihomo 的配置解析层与平台无关，但 **`tun`、`iptables`、`nftables`、listener 绑定权限** 在 Linux/LXC 上行为不同。凡涉及 TUN 与透明代理的结论，本文均显式标注为 `[未验证]`。

### 2.1 `mihomo -h` 全部 flag（`help.txt` 原文，[实测]）

```
-age-secret-key string    specify age secret key to decrypt configuration
-config string            specify base64-encoded configuration string
-d string                 set configuration directory
-ext-ctl string           override external controller address
-ext-ctl-pipe string      override external controller pipe address
-ext-ctl-routing-mark int override external controller routing mark
-ext-ctl-tls string       override external controller tls address
-ext-ctl-unix string      override external controller unix address
-ext-ui string            override external ui directory
-f string                 specify configuration file
-m                        set geodata mode
-post-down string         set post-down script
-post-up string           set post-up script
-secret string            override secret for RESTful API
-t                        test configuration and exit
-v                        show current version of mihomo
```

关键子命令（非 flag，来自 `main.go` 的 `os.Args[1]` 分派）：`convert-ruleset`、`generate`、`age`。[上游源码]

### 2.2 `-d` / `-f` 的解析规则（[上游源码] `src_main.go`）

- `-d`（`homeDir`）：若为相对路径，会与 `os.Getwd()` 拼接为绝对路径后 `C.SetHomeDir()`。默认值来自环境变量 `CLASH_HOME_DIR`。
- `-f`（`configFile`）：若为相对路径同样拼接 cwd；**为空时**取 `filepath.Join(C.Path.HomeDir(), C.Path.Config())`，即 `<homeDir>/config.yaml`。默认值来自 `CLASH_CONFIG_FILE`。
- `-f -` 表示从 **stdin** 读取配置。[上游源码]
- `-config`（base64）优先于 `-f`：`configBytes` 非空时走 `ParseWithBytes`。[上游源码]
- 参数覆盖的环境变量：`CLASH_OVERRIDE_EXTERNAL_CONTROLLER`、`CLASH_OVERRIDE_SECRET`、`CLASH_OVERRIDE_EXTERNAL_UI`、`CLASH_OVERRIDE_EXTERNAL_CONTROLLER_UNIX` 等。**这对 Agent 很关键**：Agent 可以完全不改配置文件，而用 `-ext-ctl-unix` / `-ext-ui` / `-secret` 在命令行注入控制面参数。[上游源码]

### 2.3 `-t` 的确切执行路径（[上游源码] `src_main.go`）

```go
if testConfig {
    if len(configBytes) != 0 {
        if _, err := executor.ParseWithBytes(configBytes); err != nil {
            log.Errorln(err.Error())
            fmt.Println("configuration test failed")
            os.Exit(1)
        }
    } else {
        if _, err := executor.Parse(); err != nil {
            log.Errorln(err.Error())
            fmt.Printf("configuration file %s test failed\n", C.Path.Config())
            os.Exit(1)
        }
    }
    fmt.Printf("configuration file %s test is successful\n", C.Path.Config())
    return
}
```

对比正式启动：`hub.Parse(configBytes, options...)`，失败时 `log.Fatalln("Parse config error: %s", ...)`。

**推论（重要）**：`-t` 与启动的差异仅在于 `-t` 调用 `executor.Parse*`（**纯解析**），启动调用 `hub.Parse`（解析 **+ ApplyConfig**）。因此：

- `-t` 通过 ⇒ **解析阶段**一定通过；
- `-t` 通过 ⇏ 启动成功（listener 绑定、权限、geodata 可用性都在 ApplyConfig 阶段才暴露）。
- `-t` 与启动共用同一 `config.Parse`，故 **`-t` 是"解析期语义门禁"，不是"启动期可行性门禁"**。

### 2.4 日志输出通道（[实测]，易踩坑）

mihomo 的 `log` 组件把**所有**日志（含 `level=fatal`、`level=error`）写入 **stdout**；实测 `stderr` **始终为空**。

- 非法配置启动：`stdout: time="..." level=fatal msg="Parse config error: invalid mode"`，`exit 1`。[实测]
- 非法配置 `-t`：`stdout: level=error msg="invalid mode"` + `configuration file ... test failed`，`exit 1`。[实测]

> Agent 的实现含义：**不要只读 stderr**。捕获 mihomo 输出必须合并 stdout+stderr（`2>&1`），否则会丢失全部错误文本。

---

## 3. `mihomo -t` 校验能力矩阵（逐 case 结果表）

**实验方法** [实测]：对每个 case，在空的独立 `-d` 目录下执行

```bash
mkdir -p <tmpdir> && cd <tmpdir>
/tmp/r02-config/mihomo -t -d <tmpdir> -f /tmp/r02-config/cases/<NN>.yaml
```

`-t` 不监听端口、不启用 TUN 流量，因此该实验无副作用（网络下载除外，见下）。

### 3.1 结果总表

| # | Case | 预期分类 | 实际结果 | 退出码 | 关键输出（stdout） |
|---|---|---|---|---|---|
| 01 | `01-valid.yaml` | 合法基线 | ✅ 通过 | **0** | `... test is successful` |
| 02 | `02-yaml-indent.yaml` | YAML 语法 | ✅ 拦截 | **1** | `yaml: line 2: did not find expected '-' indicator` |
| 03 | `03-yaml-quote.yaml` | YAML 语法 | ✅ 拦截 | **1** | `yaml: line 3: found unexpected end of stream` |
| 04 | `04-unknown-field.yaml` | **未知字段** | ❌ **漏过** | **0** | `... test is successful` |
| 05 | `05-type-error.yaml` | 类型错误 | ✅ 拦截 | **1** | `yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str 'abc' into int` |
| 06 | `06-missing-proxy-provider.yaml` | 语义（引用缺失） | ✅ 拦截 | **1** | `proxy group[0]: PROXY: 'nonexistent-provider' not found` |
| 07 | `07-missing-rule-provider.yaml` | 语义（引用缺失） | ✅ 拦截 | **1** | `rules[0] [RULE-SET,other-provider,REJECT] error: rule set [other-provider] not found` |
| 08 | `08-bad-mode.yaml` | 枚举错误 | ✅ 拦截 | **1** | `invalid mode` |
| 09 | `09-tun.yaml` | **TUN 可用性** | ❌ **漏过** | **0** | `... test is successful` |
| 10 | `10-bad-rule-syntax.yaml` | 语义（规则格式） | ✅ 拦截 | **1** | `rules[0] [THIS-IS-NOT-A-RULE] error: format invalid` |
| 11 | `11-missing-proxy-ref.yaml` | 语义（引用缺失） | ✅ 拦截 | **1** | `proxy group[0]: PROXY: 'does-not-exist' not found` |
| 12 | `12-ruleset-geoip.yaml` | 资源缺失（网络） | ✅ 拦截* | **1** | `can't initial GeoIP: can't download MMDB: context deadline exceeded` |
| 13 | `13-geosite.yaml` | 资源缺失（网络） | ✅ 拦截* | **1** | `can't initial GeoSite: can't download GeoSite.dat: ... dial tcp ...: connect: operation timed out` |
| 14 | `14-provider-listed.yaml` | 资源缺失（provider） | ❌ **漏过** | **0** | `... test is successful` |

\* case 12/13 的拦截依赖**网络不可达**。若上游可达，下载成功则 `-t` 会通过 —— 即**该结果不可复现为"配置错误"，而是环境相关**。

**统计**：14 个 case 中 8 个被拦截、**4 个被漏过**（04 未知字段、09 TUN、14 provider URL 不可达）。

### 3.2 逐个 case 的详细证据

**Case 04 — 未知字段漏过（最重要的负面结论）** [实测]

配置内容：`mixed-portt: 17890`（注意多了一个 `t`，与 `mixed-port` 不同）。

```
exit 0
time="..." level=info msg="Start initial configuration in progress"
time="..." level=info msg="Initial configuration complete, total time: 0ms"
configuration file /tmp/r02-config/cases/04-unknown-field.yaml test is successful
```

原因 [上游源码]：`config.Parse` 使用 `yaml.Unmarshal` 到固定 struct，**未设置 `KnownFields(true)`**（gopkg.in/yaml.v3 的严格模式），未知 key 被静默忽略。因此 `mixed-portt: 17890` 被丢弃，进程用**默认端口 7890** 启动 —— **配置看似生效实则未生效，且无任何告警**。

> 这是 Agent 必须自研 Level 2 校验的**决定性证据**：一个用户把 `mixed-port` 拼错成 `mixed-portt`，`-t` 说 OK，reload 说 204，但实际端口没变。此类"静默失效"在配置管理产品中属于最高危缺陷。

**Case 09 — TUN 配置漏过** [实测]

```yaml
tun:
  enable: true
  stack: system
  auto-route: true
  auto-detect-interface: true
```

`-t` → `exit 0`，无任何 TUN 相关日志。原因 [上游源码]：`-t` 只做 `config.Parse`；TUN 设备的实际创建在 `ApplyConfig → updateTun → updateListeners` 阶段。

对照证据 [实测]：原调研者在 `rl/run.log` 中真正启动了含 `tun.device: utun999` 的配置，日志显示：

```
level=warning msg="[TUN] default interface changed by monitor, => en0"
level=error   msg="Start TUN listening error: configure tun interface: Connect: operation not permitted"
```

即 **TUN 失败在启动期才暴露，且进程不退出**（见 §5）。Agent 若把 `-t` 当作 TUN 可用性门禁，会在无 `CAP_NET_ADMIN` 的 LXC 中把"配置合法"误判为"可以启用 TUN"。

**Case 12 — `-t` 触发真实 geodata 下载** [实测]

```
level=info  msg="Can't find MMDB, start download"          # 13:35:26
# ... 阻塞约 90 秒 ...
level=error msg="can't initial GeoIP: can't download MMDB: context deadline exceeded"   # 13:36:56
level=error msg="rules[0] [GEOIP,CN,DIRECT] error: can't download MMDB: context deadline exceeded"
configuration file ... test failed
exit 1
```

副作用落盘：该 `-d` 目录出现 `geoip.metadb`（4,182,400 字节）。**注意：报文说超时失败，但文件仍然被落盘**，说明下载是流式写入后校验。该文件在后续实验中**无法被复用**（见下）。

**Case 12 复现实验：部分文件被删除** [实测]

在 `-d` 中预置上一步落盘的 `geoip.metadb` 后重跑：

```
level=warning msg="MMDB invalid, remove and download"      # 13:38:26
# ... 阻塞约 75 秒 ...
level=error msg="can't initial GeoIP: can't download MMDB: ... operation timed out"
exit 1
```

结束后该目录中 **`geoip.metadb` 已被删除**（目录回到空）。即 mihomo 对**校验失败**的 geodata 会**主动删除**并重新下载。这对 Agent 的含义：**Agent 无法通过"预置 geodata"来让 `-t` 离线通过**；离线环境下含 GEOIP/GEOSITE 的配置**永远无法通过 `-t`**。

**Case 13** [实测] 同一机制，目标文件为 `GeoSite.dat`，超时约 75 秒。

**Case 14 — `-t` 不校验 provider 可达性** [实测]

配置声明了 `proxy-providers.testprov`，`url: "http://127.0.0.1:1/prov.yaml"`（必然不可达）。`-t` → `exit 0`。

原因 [上游源码]：`config.Parse` 只构造 provider 声明，**实际拉取发生在 `ApplyConfig → loadProvider`**。对照 [实测] `d3/run.log`：

```
level=info  msg="Start initial provider localfile"
level=error msg="initial proxy provider localfile error: fswatch: watch /tmp/r02-config/d3/providers lstat /tmp/r02-config/d3/providers: no such file or directory"
```

即 provider 加载在**启动期**失败，且（同 TUN）进程不退出。

### 3.3 `-t` 的副作用矩阵（重要）

| 副作用 | `-t` 是否触发 | 正式启动是否触发 | 证据 |
|---|---|---|---|
| 创建 `cache.db` | **否** | 是（65536 字节） | [实测] |
| 下载 / 删除 `geoip.metadb`、`GeoSite.dat` | **是**（有 GEOIP/GEOSITE 规则时） | 是 | [实测] |
| 下载 external UI（`external-ui` 指向不存在目录） | 否 | 是（`External UI downloading ...`） | [实测] `d3/run.log` |
| **创建缺失的 `-f` 目标文件** | **是** | 是 | [实测] 见下 |
| 绑定监听端口 | 否 | 是 | [实测] |
| 创建 TUN 设备 | 否 | 是 | [上游源码] + [实测] |

**`-t` 会自动创建缺失配置文件（高危假阳性）** [实测]

```bash
mkdir -p /tmp/r02-supp/tn
/tmp/r02-config/mihomo -t -d /tmp/r02-supp/tn -f /tmp/r02-supp/tn/nope.yaml
# time="..." level=info msg="Can't find config, create a initial config file"
# configuration file /tmp/r02-supp/tn/nope.yaml test is successful
# EXIT=0
```

执行后 `nope.yaml` 被创建，内容为 `mixed-port: 7890`（16 字节）。

**结论**：`-t -f <path>` 在文件不存在时**不会报错**，而是"生成一份初始配置并报告成功"。这是 `config.Init(homeDir)` 的行为（[上游源码] `src_main.go` 在 `-t` 分支之前就调用了 `config.Init`）。

> **Agent 硬性要求**：调用 `mihomo -t` 前**必须由 Agent 自己确认目标文件存在且非空**，绝不能把"`-t` exit 0"直接等同于"我给它的那份配置合法"。否则一次路径拼写错误会让校验器创建一份占位配置并放行。

**`-t` 在已有 `profile.store-selected` 等配置下仍不创建 cache.db** [实测]

用含 `profile: {store-selected: true, store-fake-ip: true}` 的合法配置跑 `-t` → `exit 0`，目录中**只有配置文件本身**，无 `cache.db`。

### 3.4 与启动的等价性交叉验证

原调研者留下的启动失败日志（[实测] `/tmp/r02-config/s1/out.log`、`s2/out.log`）与我的 `-t` 结果**逐字一致**：

| Case | `s*/out.log`（正式启动） | 我的 `-t`（stdout） |
|---|---|---|
| `08-bad-mode` 同款（`s1`） | `level=fatal msg="Parse config error: invalid mode"` | `level=error msg="invalid mode"` |
| `02-yaml-indent` 同款（`s2`） | `level=fatal msg="Parse config error: yaml: line 2: did not find expected '-' indicator"` | `level=error msg="yaml: line 2: did not find expected '-' indicator"` |

错误**文本**完全相同，仅日志前缀不同（`-t` 用 `log.Errorln` + 自定义成功/失败行，启动用 `log.Fatalln`）。**这就是 `-t` 可作门禁的机制性依据**。

---

## 4. Reload 语义与失败行为

### 4.1 上游实现（[上游源码] `src_hub_route_configs.go`）

```go
func updateConfigs(w http.ResponseWriter, r *http.Request) {
	req := struct {
		Path    string `json:"path"`
		Payload string `json:"payload"`
	}{}
	if err := render.DecodeJSON(r.Body, &req); err != nil {
		render.Status(r, http.StatusBadRequest)
		render.JSON(w, r, ErrBadRequest)   // → {"message":"Body invalid"}
		return
	}

	force := r.URL.Query().Get("force") == "true"

	if req.Payload != "" {
		cfg, err = executor.ParseWithBytes([]byte(req.Payload))
		if err != nil {
			render.Status(r, http.StatusBadRequest); render.JSON(w, r, newError(err.Error())); return
		}
	} else {
		if req.Path == "" {                   // 默认路径，不做 safe 检查
			req.Path = C.Path.Config()
		} else {
			if !filepath.IsAbs(req.Path) {
				render.Status(r, http.StatusBadRequest); render.JSON(w, r, newError("path is not a absolute path")); return
			}
			if !C.Path.IsSafePath(req.Path) {
				render.Status(r, http.StatusBadRequest); render.JSON(w, r, newError(C.Path.ErrNotSafePath(req.Path).Error())); return
			}
		}
		cfg, err = executor.ParseWithPath(req.Path)
		if err != nil {
			render.Status(r, http.StatusBadRequest); render.JSON(w, r, newError(err.Error())); return
		}
	}

	executor.ApplyConfig(cfg, force)   // ← 无错误返回；失败只能在日志里看到
	render.NoContent(w, r)             // ← 一律 204
}
```

**三条结构性事实（决定 Agent 设计）**：

1. **解析失败 → 400，且 `ApplyConfig` 完全没被调用** ⇒ 旧配置**零影响**。这是真正的原子性。
2. **`ApplyConfig` 没有返回值**（`func ApplyConfig(cfg *config.Config, force bool)`，无 error）⇒ 应用阶段的任何失败**无法通过 HTTP 状态码反馈**，handler 依然 `render.NoContent`（204）。
3. **`force` 只传给 `updateListeners(general, listeners, force)`**，注释明确：`updateTun(cfg.General) // tun should not care "force"`。即 **`force` 只影响 listener 是否强制重绑**，不影响 TUN。[上游源码]

### 4.2 实测结果矩阵（独立复现）

环境：`mihomo -d /tmp/r02-supp/reload`，基线 `mixed-port: 17902`、`external-controller: 127.0.0.1:19102`、`secret: r02secret`。[实测]

| # | 请求 | HTTP | 响应体 | 应用后 `mixed-port` | 进程存活 |
|---|---|---|---|---|---|
| T1 | `PUT /configs?force=true` body `{"path":".../v2.yaml"}` | **204** | (空) | **17903**（已切换） | 是 |
| T2 | 同上但 path 指向**非法配置** | **400** | `{"message":"invalid mode"}` | **17903**（不变） | 是 |
| T3 | `{"path":"v2.yaml"}`（相对路径） | **400** | `{"message":"path is not a absolute path"}` | 17902 | 是 |
| T4 | `{"path":"/etc/hosts"}`（白名单外） | **400** | `{"message":"path is not subpath of home directory or SAFE_PATHS: /etc/hosts \n allowed paths: [/tmp/r02-supp/reload]"}` | 17902 | 是 |
| T5 | 安全目录内**不存在**的文件 | **400** | `{"message":"stat /tmp/r02-supp/reload/missing.yaml: no such file or directory"}` | 17902 | 是 |
| T6 | **空 body** | **400** | `{"message":"Body invalid"}` | 17902 | 是 |
| T7 | `{}`（无 force） | **204** | (空) | 17902（重载同配置） | 是 |
| T8 | `{}`（`?force=true`） | **204** | (空) | 17902 | 是 |
| T9 | `{"payload":"<good.yaml 全文>"}` | **204** | (空) | **17902**（payload 生效） | 是 |
| T10 | `{"payload":"<非法内容>"}` | **400** | `{"message":"invalid mode"}` | 不变 | 是 |
| T11 | 错误 `Authorization: Bearer wrong` | **401** | `{"message":"Unauthorized"}` | 不变 | 是 |
| T12 | 新配置 `mixed-port: 17909`（**被占用**），`?force=false` | **204** | (空) | **17902**（旧端口继续服务） | 是 |
| T13 | 同上，端口已释放，`?force=false` | **204** | (空) | 17902（**未切换到 17909**） | 是 |
| T14 | 同上，端口**被占用**，`?force=true` | **204** | (空) | **0** ⚠️ | 是（僵尸态） |
| T15 | 僵尸态下 reload 回合法 `good.yaml` | **204** | (空) | **0**（**无法恢复**） | 是 |

### 4.3 核心结论 A：解析失败**保留**旧配置（安全）

T2、T10 直接验证：**非法配置 → HTTP 400，进程存活，旧配置的端口、模式、日志级别全部不变**。

T2 后实测：

```
live config after bad reload: {'mixed-port': 17903, 'mode': 'global', 'log-level': 'debug'}
process alive? YES
proxy port 17903 (v2) still listening? YES
proxy port 17902 (v1) listening? NO
```

通过 17902/17903 实际发起代理请求也成功（`via17902 HTTP=200`）。

**这与 01-mihomo.md §TL;DR 结论 6 一致**，并与原调研者 `rl/resp2.txt`（`{"message":"invalid mode"}`）逐字吻合。

### 4.4 核心结论 B：`force` 的语义是"**是否拆掉旧监听器**"，且失败会摧毁数据面（危险）

这是本次补充实测最重要的发现，**01-mihomo.md §开放问题 6 记录的「`{}` 与 `{}&force=true` 未观察到行为差异」在此被解释并推翻**：

- 当**没有资源冲突**时，`force` 与否都成功，表现为无差异（解释了 R01 的观察）。
- 当**新端口被占用**时，两者行为**截然不同**：

**T12/T13（`force=false`）—— 安全降级**

新配置 `mixed-port: 17909` 被别的进程占用：

```
HTTP=204
  live: {'mixed-port': 17902, ...}          ← 仍是旧端口
  17902(old) open? YES                       ← 旧监听器保留，服务不中断
```

日志**没有**该次 reload 的 bind 错误。mihomo 的 `updateListeners(..., force=false)` 在端口相同时复用，不同且失败时**保持旧 listener**。T13（端口释放后同样 `force=false`）仍为 17902，说明非 force 路径**不会主动切换到新端口**。

**T14（`force=true`）—— 破坏性失败**

同样被占用，但带 `?force=true`：

```
HTTP=204                                        ← ⚠️ 仍报成功
  live: {'mixed-port': 0, ...}                  ← 监听器没了
  alive? YES
  17902 open? NO                                 ← 旧监听器已被拆除
  17909 open? NO                                 ← 新监听器绑定失败
```

日志中唯一的错误线索：

```
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:17909: bind: address already in use"
```

**T15（不可自愈）**：在该僵尸态下 reload 回完全合法的 `good.yaml`，**连续两次**都返回 204，但 `mixed-port` 恒为 `0`、17902 始终不监听：

```
attempt1 HTTP=204 mixed-port=0  17902open=N
attempt2 HTTP=204 mixed-port=0  17902open=N
```

只有**杀掉进程重启**才恢复：

```
new pid=18093 17902open=Y  live: 17902
```

机制解释 [上游源码]：`ApplyConfig` 的执行顺序是 `tunnel.OnSuspend()` → …… → `updateListeners(cfg.General, cfg.Listeners, force)`。`force=true` 时旧 listener 被 `Close()` 后尝试绑定新地址；绑定失败只写日志、不 panic、不恢复、不回滚，进程继续带 `OnSuspend` 状态运行。**这就是"控制器存活、数据面死亡"的来源。**

> **对核心不变量的直接冲击**：项目不变量是"配置更新失败绝不能破坏当前可用配置"。实测证明：**只要 Agent 使用 `force=true`，一次端口占用的 reload 就能打掉数据面且不可自愈**。如果系统目录里恰好已有用户自己的服务占用该端口（LXC 中很常见），这就是生产事故。

### 4.5 `-t` 通过但 reload 仍可能摧毁服务

把 §3 与 §4.4 串起来：

```
-t exit 0  ──▶  reload ?force=true  ──▶  HTTP 204  ──▶  数据面死亡
```

`updateConfigs` 在返回 204 前**不调用 `-t`**，而 `-t` 也**不校验端口可用性**（它根本不绑定端口，见 §3.3）。因此 **`-t` 与 reload 之间存在一个无门禁的"绑定期"**。Agent 必须自行覆盖这一段（见 §8）。

### 4.6 路径与鉴权（复述 + 收紧）

- **`path` 三条件**：绝对路径 + 在 `homeDir` 或 `SAFE_PATHS` 内 + 文件存在。任一不满足 → 400。[实测 T3/T4/T5]
- **`SAFE_PATHS` 解析** [上游源码] `src_constant_path.go`：

```go
for _, safePath := range filepath.SplitList(os.Getenv("SAFE_PATHS")) { ... }
// IsSafePath: 允许 allowUnsafePath（env SKIP_SAFE_PATH_CHECK=1）或 features.CMFA 时为 true
// SafePaths() = append([]string{p.homeDir}, p.safePaths...)   ← homeDir 永远在白名单
```

  Unix 下 `filepath.SplitList` 以 **`:`** 分隔（与 PATH 相同，官方文档亦如此说明 [上游文档] `controller-docs.txt:4077`）。
- **重要例外**：`req.Path == ""`（即 body 为 `{}`）时**跳过全部 safe 检查**，直接重载 `C.Path.Config()`。代码注释：`// default path unneeded any safe check`。[上游源码]
- **`payload` 模式完全绕过路径检查**，直接 `ParseWithBytes`。[上游源码] 这对 Agent 是最便利的通道：**不需要把文件写进 mihomo 的 `SAFE_PATHS`**。
- **官方文档对 `external-ui` 的 SAFE_PATHS 说明** [上游文档]：

  > "Note that if the path is not in the Clash working directory, please manually set the SAFE_PATHS environment variable to add it to the safe path."

- **为空 body 必须发 `{}`**：T6 证实空 body → `{"message":"Body invalid"}`（`render.DecodeJSON` 失败）。与 01-mihomo.md 结论 6 一致。

---

## 5. 启动失败与残留状态

### 5.1 解析失败启动：exit 1，无残留

[实测]（`/tmp/r02-supp/startfail`，配置为 `mixed-port: "NOT_A_PORT"` + `mode: nonsense` + 非法规则）

```
EXIT CODE=1
out.txt: time="..." level=fatal msg="Parse config error: invalid mode"
err.txt: (空)
目录残留: 只有 config.yaml 与我的 out/err 文件 —— 无 cache.db、无 geodata
```

同一行为在原调研者的 `s1`/`s2` 日志中独立复现。[实测]

**要点**：解析失败在 `hub.Parse` 阶段 `log.Fatalln`，**发生在任何 listener 创建与 `cache.db` 写入之前**，因此**失败是干净的** —— 这对 Agent 的"临时启动"策略很有利（§8）。

### 5.2 端口被占用启动：**不退出**，形成无数据面的僵尸进程

[实测]（配置 `mixed-port: 17911`，先用 python 占用该端口）

```
STILL RUNNING -> exit code not yet determined
controller reachable? 200              ← 控制器正常响应 /version
```

日志：

```
level=info  msg="RESTful API listening at: 127.0.0.1:19111"
level=info  msg="Sniffer is closed"
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:17911: bind: address already in use"
level=info  msg="Start initial compatible provider default"
```

**进程不退出、不返回非零码、控制器可用、代理端口不存在。** 这与原调研者 `s3/out.log`、`s4.log`、`s5.log`、`s6/out.log`、`fresh/run2.log` 的观测完全一致（那些日志里都出现 `bind: address already in use`，但进程继续运行直到被外部 kill，最后打印 `level=warning msg="Mihomo shutting down"`）。[实测]

> **Agent 硬性要求**：**不能以"进程存活 + 控制器 200"作为启动成功判据**。必须额外做数据面健康检查（实际连接代理端口或 `/proxies` 探活），否则会把僵尸态判为成功。这正是设计文档中 "Health Check" 环节不可省略的原因。

### 5.3 external-controller 端口被占用：控制器不可用但进程仍存活

[实测] 原调研者 `fresh/run2.log`（第二次在同一目录启动，端口全被第一个实例占用）：

```
level=error msg="External controller listen error: listen tcp 127.0.0.1:19095: bind: address already in use"
level=error msg="Start Mixed(http+socks) server error: listen tcp 127.0.0.1:17895: bind: address already in use"
level=info  msg="Start initial compatible provider default"
level=warning msg="[CacheFile] can't open cache file: timeout"
level=warning msg="Mihomo shutting down"     ← 2 秒后被外部 kill
```

两个端口都失败、进程仍继续启动、最后被 kill。**注意 `[CacheFile] can't open cache file: timeout`** —— 两个实例共用同一 `cache.db` 会产生 SQLite 锁超时。**Agent 必须保证同一 `-d` 目录同时只有一个 mihomo 实例**。[实测]

### 5.4 自动创建初始配置

当 `-d` 目录下找不到配置文件时（原调研者 `s3`/`s4` 的 `config.yaml` 只写了 `mixed-port: 7890`，属于被创建的初始配置）：

```
level=info msg="Can't find config, create a initial config file"
level=info msg="Start initial configuration in progress"
...
level=info msg="Initial configuration complete, total time: 0ms"
```

[实测] 我创建的初始配置内容确认为 `mixed-port: 7890`（16 字节）。**该行为在启动与 `-t` 两条路径上都会发生**（§3.3）。

> Agent 含义：Agent 若把 `mihomo -d <dir>` 当成"用我放在 `<dir>/config.yaml` 的配置启动"，而配置因故缺失，mihomo 会**静默生成默认配置并启动在 7890 端口**，Agent 会误以为部署成功。

### 5.5 TUN / provider 失败同样不致命

[实测] `rl/run.log`：

```
level=warning msg="[TUN] default interface changed by monitor, => en0"
level=error   msg="Start TUN listening error: configure tun interface: Connect: operation not permitted"
level=info    msg="Start initial compatible provider default"
```

TUN 创建失败（macOS 无权限）后进程继续运行，代理端口正常服务，实测 `[TCP] 127.0.0.1:58722 --> example.com:80 match Match using DIRECT`。这与 AGENTS.md 中"TUN 不可用必须是合法降级态"的设计一致，是对该设计原则的**实测支持**。

---

## 6. 目录/文件模型与网络依赖

### 6.1 `-d` 目录模型（[上游源码] `src_constant_path.go`）

`Path` 是一个在包初始化时求值的单例：

```go
var Path = func() *path {
	homeDir, err := os.UserHomeDir()
	homeDir = P.Join(homeDir, ".config", Name)      // Name = "mihomo"
	if _, err = os.Stat(homeDir); err != nil {
		if configHome, ok := os.LookupEnv("XDG_CONFIG_HOME"); ok {
			homeDir = P.Join(configHome, Name)
		}
	}
	...
}()

func (p *path) HomeDir() string { return p.homeDir }
func (p *path) Config() string  { return p.configFile }   // 默认 "config.yaml"
```

派生路径（全部相对 `homeDir`）：

| 方法 | 路径 | 用途 |
|---|---|---|
| `Config()` | `<home>/config.yaml` | 默认配置文件 |
| `Cache()` | `<home>/cache.db` | SQLite 缓存（selected/fake-ip 等） |
| `OldCache()` | `<home>/.cache` | 旧格式缓存 |
| `GeoIP()` | `<home>/GeoIP.dat` | GeoIP dat（大小写不敏感探测已有文件名） |
| `GeoSite()` | `<home>/GeoSite.dat` | GeoSite dat |
| `MMDB()` | `<home>/{Country.mmdb,geoip.db,geoip.metadb}` 探测，兜底 `<home>/geoip.metadb` | GeoIP mmdb |
| `ASN()` | `<home>/ASN.mmdb` | ASN 库 |
| `BundleMRS()` | `<home>/BundleMRS.7z` | 规则集 bundle |
| `GetPathByHash(prefix,name)` | `<home>/<prefix>/<sha256(name)>` | provider 缓存文件（**文件名是名称的哈希**） |
| `GetAssetLocation(file)` | `<home>/<file>` | 通用资产 |

`MMDB()`/`GeoIP()`/`GeoSite()`/`ASN()`/`BundleMRS()` 都会 **`os.ReadDir(homeDir)` 探测真实存在的文件名**，命中后还会**改写包级变量**（如 `GeoipName = fi.Name()`）。这是 mihomo 兼容 `Country.mmdb` / `geoip.db` / `geoip.metadb` 多命名的机制。[上游源码]

### 6.2 实测：首启动产物

| 目录 | config 特征 | 首启动产物 | 证据 |
|---|---|---|---|
| `d2` | 纯代理配置（无 provider/UI） | `cache.db`（65536 B） | [实测] |
| `d3` | 含 `external-ui: ui` + file provider | `cache.db` + 日志 `External UI downloading ...`（**ui 目录未落盘**） | [实测] |
| `fresh` | 最小配置 | `cache.db` | [实测] |
| `rl`/`s3`/`s4`/`s6` | 各类 | `cache.db` | [实测] |
| `run1` | （`-t` 用空目录） | **空** | [实测] |

**结论**：正常启动**唯一必然产生的持久文件是 `cache.db`**（65536 字节的 SQLite）。geodata 与 external-ui **仅在配置需要时才产生**。[实测]

### 6.3 网络依赖与降级

| 资源 | 触发条件 | 上游默认 URL | 失败行为 |
|---|---|---|---|
| `geoip.metadb` | 规则含 `GEOIP` 且 geodata 缺失 | `https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb` | `-t`/启动时报错；**已损坏文件被删除重下**；实例仍可运行（仅该规则不可用） |
| `GeoSite.dat` | 规则含 `GEOSITE` | `.../geosite.dat` | 同上 |
| `GeoIP.dat` | `geodata-mode: true` | `.../geoip.dat` | 同上 |
| `ASN.mmdb` | ASN 相关规则 | `.../GeoLite2-ASN.mmdb` | 同上 |
| external UI | `external-ui` 指向不存在目录 | `https://github.com/MetaCubeX/metacubexd/archive/refs/heads/gh-pages.zip` | 日志 `External UI downloading ...`，失败后无 UI 目录 |
| proxy/rule provider | 配置声明 | 用户提供 | `initial proxy provider <name> error: ...`，实例继续运行 |
| 内核更新 `/upgrade` | 用户触发 | GitHub release API | 见 R01 §TL;DR 9 |

**`geox-url` 可在配置中覆盖** [上游文档]（`controller-docs.txt:4169-4173`），官方示例使用 `https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/...`。实测 `GET /configs` 返回的运行时默认值为（[实测] `rl/cfgs_after.json`）：

```json
"geox-url": {
  "geo-ip":   "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.dat",
  "mmdb":     "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb",
  "asn":      "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/GeoLite2-ASN.mmdb",
  "geo-site": "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geosite.dat"
},
"geo-auto-update": false,
"geo-update-interval": 24,
"geodata-mode": false,
"geodata-loader": "memconservative",
"geosite-matcher": "succinct"
```

> **部署含义（Linux/LXC）**：GitHub release 下载在大陆/受限网络环境**经常不可达**。实测本机（macOS，本可访问 GitHub 网页）在 90 秒内**无法**完成 `geoip.metadb` 下载，说明该 CDN 路径不可靠。Agent 必须：
> ① 提供 geodata 预置/离线注入能力；② 在配置生成阶段**主动探测 geodata 是否存在**，缺失时对"含 GEOIP/GEOSITE 规则"的配置给出**明确的"环境不满足"错误**（而不是让用户看到 90 秒超时后一个模糊的 `test failed`）；③ 把 `geox-url` 作为可配置项暴露给用户（指向自建镜像）。
>
> 另注意 **`geo-auto-update` 默认 `false`**，但 `main.go` 中有 `if updater.GeoAutoUpdate() { updater.RegisterGeoUpdater() }` —— 若用户开启，mihomo 会在后台按 `geo-update-interval` 下载，**这是 Agent 网络策略的盲区**。[上游源码][实测]

### 6.4 `cache.db` 的并发约束

[实测] `fresh/run2.log` 出现 `[CacheFile] can't open cache file: timeout`，确认**同一 `-d` 目录不支持两个实例并发**。Agent 在每个 `-d` 目录上必须有互斥（配合 AGENTS.md 的 per-instance lock 要求）。

### 6.5 `-m` 与 geodata loader

`-m` 是 `set geodata mode` 的 bool flag [实测 `help.txt`]，对应 `geodata.SetGeodataMode(true)` [上游源码]，即在 `GeoIP.dat`(dat) 与 `geoip.metadb`(mmdb) 之间切换。运行时等价配置为 `geodata-mode: true` [上游文档 `controller-docs.txt:4147`]。

`src_component_geodata.go` [上游源码] 显示 loader 是**插件式注册表**：

```go
func RegisterGeoDataLoaderImplementationCreator(name string, loader func() LoaderImplementation)
func GetGeoDataLoader(name string) (Loader, error)   // 找不到 → "unable to locate GeoData loader %s"
```

对应日志 `Geodata Loader mode: memconservative` / `Geosite Matcher implementation: succinct` [实测]。Agent 若写死 `geodata-loader` 值，在未来版本可能遇到 "unable to locate GeoData loader" —— 建议**不指定**，用上游默认。

---

## 7. 版本兼容与 Config Schema（回答 Q001）

### 7.1 Q001 问题原文

> **Q001 — Mihomo Config 是否存在适合机器校验的稳定 Schema？**
> 状态 `OPEN`（等待 R02 结论）；阻塞 Config Validation 的 Level 2 实现方式（自建字段白名单 vs 依赖上游 schema）。

### 7.2 证据与结论

**结论：上游不存在机器可校验的官方 schema。Level 2 只能由 `mihomo -t` + Agent 自建字段白名单共同承担。**

证据（[上游源码]，`/tmp/r02-config/tree.json`，tag `v1.19.30`，commit `ac017cdd...`，`truncated: false`，共 1222 项）：

- 全树中**没有任何** `*.schema.json` / `schema.json` / `.vscode/schema` 文件。所有 `*.json` 命中均为 `test/config/*.json`（代理协议测试夹具，如 `vmess.json`、`trojan.json`），**不是配置 schema**。
- `config/` 目录仅有 4 个文件：`config.go`、`initial.go`、`utils.go`、`utils_test.go` —— **无 schema 生成器、无 schema 输出**。
- `docs/` 仅 `config.yaml` 与 `logo.png`，是文档站点配置，非 schema。
- `.github/` 中无 schema 发布流水线（只有 `build.yml`、`test.yml`、`trigger-cmfa-update.yml`、`release.sh`）。
- [上游文档] `mihomo-config-docs.html` 中 **"schema" 出现 0 次**；`controller-docs.txt` 中同样 0 次。

**"事实 schema"的唯一来源是 Go struct 的 yaml tag** [上游源码] `src_config_config.go`，例如：

```go
AllowOrigins        []string `yaml:"allow-origins" json:"allow-origins"`
AllowPrivateNetwork bool     `yaml:"allow-private-network" json:"allow-private-network"`
...
FallbackFilter      RawFallbackFilter `yaml:"fallback-filter" json:"fallback-filter"`
FakeIPFilterMode    C.FilterMode      `yaml:"fake-ip-filter-mode" json:"fake-ip-filter-mode"`
```

该 struct 通过 `yaml:` tag 完整定义了字段名与类型，**但没有任何机制把它导出为可消费的 schema 文件**。

### 7.3 对 Level 2 校验的裁决

| 校验层 | 手段 | 覆盖率 | 裁决 |
|---|---|---|---|
| Level 1 语法 | YAML parser（Agent 侧或 `-t`） | 完整 | `-t` 足够 |
| **Level 2 语义/字段** | **`mihomo -t`** | **不完整**：漏未知字段（case 04）、漏 TUN（case 09）、漏 provider 可达性（case 14） | **必须 `-t` + Agent 字段白名单互补** |
| Level 3 运行时 | 临时启动 + 健康检查 | — | 只能靠 Agent（§8） |

**Q001 的建议答复**：**不可能依赖上游 schema**（不存在）。Agent 应从 `config.go` 的 yaml tag 维护一份**版本化的字段白名单**，作为 Level 2 的前置检查；`mihomo -t` 负责它擅长的部分（类型、枚举、引用一致性、YAML 语法）。具体地：

- **未知字段检测**：Agent 自建（`-t` 明确不检测，case 04 实测）。
- **类型 / 枚举 / 引用一致性**：交给 `-t`（case 05/06/07/08/10/11 实测有效）。
- **字段白名单的维护成本**：需跟随 mihomo 版本更新。建议以"警告"而非"拒绝"呈现未知字段，因为上游会新增字段，硬拒绝会误伤新配置（**向后兼容策略**）。
- **版本兼容风险**：`v1.19.30` 的字段集与旧版 Clash/Meta 有差异（例如 `geodata-loader`、`geosite-matcher`、`etag-support`、`unified-delay` 均为 Meta 特有）。Agent 应记录其支持的 mihomo 版本区间，并在检测到不认识的字段时降级为警告。

### 7.4 版本兼容的其他实测观察

- `GET /version` → `{"meta":true,"version":"v1.19.30"}` [实测]，`meta: true` 可用于**内核识别**（沿用 R01 结论 1）。
- `GET /configs` 返回的字段集是**运行时快照**，会包含配置中未写的默认值（如 `global-ua: "clash.meta/v1.19.30"`、`geodata-loader: "memconservative"`）。[实测] 因此 **Agent 不能用"`GET /configs` 字段集"反推"用户配置字段集"** —— 前者是合并默认值后的结果。这也可作为一次**权威默认值导出**手段（比读文档更可靠）。

---

## 8. 冻结的 Config Lifecycle（含 Agent 必须自研的部分）

### 8.1 冻结的生命周期

在 Phase 0 架构发现文档给出的骨架基础上，依据本文证据**补全为可执行状态机**：

```text
[1] Generate          Agent         生成候选 YAML（订阅转换/用户编辑）
[2] Syntax            Agent 或 -t   YAML 可解析
[3] Semantic (L2)     Agent + -t    字段白名单(Agent) ⊕ 类型/枚举/引用(-t)
[4] Resource Preflight Agent         geodata 存在性 / provider 可达性 / 端口可用性
[5] Temp Start        Agent         临时目录启动 + 健康检查（真实数据面探活）
[6] Activate          Agent         原子写入 + 版本记录 + active 指针切换
[7] Reload            Mihomo        PUT /configs（payload 或 path）
[8] Health Check      Agent         reload 后真实数据面探活
[9] Commit / Rollback Agent         失败 → 切回旧版本 + 重启（不是 reload）
```

### 8.2 每一步的上游支持度

| 步骤 | 上游原生支持 | Agent 必须自研的部分 | 依据 |
|---|---|---|---|
| [1] Generate | ✗ | 全部 | — |
| [2] Syntax | ✅ `-t` / YAML 解析 | 无 | [实测] case 02/03 |
| [3] Semantic L2 | △ 部分（类型/枚举/引用） | **字段白名单**（未知字段）、错误文本结构化 | [实测] case 04 漏过 |
| [4] Resource Preflight | ✗ | geodata 存在性、provider 可达性、**端口占用检测** | [实测] case 12/13/14 超时 90s；§4.4 端口冲突 |
| [5] Temp Start | △ 有 `-t` 但**不等价**（不绑定、不建 TUN、不加载 provider） | 真实临时启动 + 健康检查 + 专用临时 `-d` | [实测] §3.3、§5.2 |
| [6] Activate | ✗ | 版本化、checksum、原子 rename、active 指针 | AGENTS.md 要求 |
| [7] Reload | ✅ `PUT /configs` | **`force` 策略决策**（见 §4.4） | [实测] |
| [8] Health Check | ✗ | **reload 后必须探活数据面**（204 不代表成功） | [实测] §4.4 T14 |
| [9] Rollback | ✗ | **必须用"重启"而非"reload"来恢复** | [实测] §4.4 T15 |

### 8.3 三个必须明确回答的设计问题

**Q: reload 是否真原子？**

**A: 分两段看，只有第一段是原子的。** [实测] + [上游源码]

- **解析段（原子）**：`ParseWithPath/ParseWithBytes` 失败 → 400，`ApplyConfig` 未被调用 → 旧配置零影响。**这是真正的"all-or-nothing"。**
- **应用段（非原子、无回滚）**：`ApplyConfig(cfg, force)` **无错误返回值**，内部依次 `OnSuspend()` → 逐模块更新 → `updateListeners(force)` → `OnRunning()`。任一步失败（尤其 bind）**不回滚已拆掉的 listener**，HTTP 仍返回 204。实测 `force=true` + 端口占用 → 进程永久僵尸（`mixed-port: 0`，需重启）。

> **因此："reload 是原子的"这句话只在解析失败场景成立，不能作为架构不变量的依据。**

**Q: 是否有 dry-run？**

**A: 没有真正的 dry-run。** 最接近的是 `mihomo -t`，但实测它与启动**不等价**（§3.3）：不绑定端口、不建 TUN、不加载 provider、不写 cache.db，且**会联网下载 geodata**、**会创建缺失的 `-f` 文件**。[实测] 官方 controller 也**没有** config 校验端点（`controller-docs.txt` 中无 validate route）。唯一的"真 dry-run"是 Agent 自己在**独立临时 `-d` 目录**上真正启动一次并探活（步骤 [5]）。[推测]（基于 `-d` 目录隔离实测可行，见 §6.4 每目录单实例约束）

**Q: `-t` 是否可当作可靠的语义门禁？**

**A: 可作"必要不充分"的门禁。** 可靠的场景：YAML 语法、类型、`mode` 等枚举、proxy/provider 引用一致性、规则格式。不可靠的场景（实测漏过）：

1. **未知/拼错字段**（case 04）——`mixed-portt` 静默忽略，`-t` 报成功；
2. **TUN 可用性**（case 09）——需 `CAP_NET_ADMIN`/`/dev/net/tun`，`-t` 完全不检查；
3. **provider 可达性**（case 14）；
4. **端口可用性**——`-t` 不绑定；
5. **不存在文件的假成功**——自动创建初始配置（§3.3）。

**Q: 合法路径与 SAFE_PATHS 的约束？**

**A:** [实测][上游源码]（详见 §4.6）

- `path` 必须绝对、必须在 `homeDir` 或 `SAFE_PATHS` 内、必须存在，否则 400。
- `homeDir`（即 `-d`）**永远**在白名单中，无需额外配置。
- `SAFE_PATHS` 用 `filepath.SplitList`（Unix 用 `:`）分隔。
- `SKIP_SAFE_PATH_CHECK=1` 可完全关闭检查 —— **Agent 不应使用**（削弱纵深防御）。
- **`payload` 模式与 `{}`（默认路径）绕过路径检查**：推荐 Agent **优先使用 `payload`**（把已校验的配置内容直接投递），从而彻底规避 `SAFE_PATHS` 配置错误；`path` 模式作为备选。

### 8.4 `force` 的推荐策略（本调研的关键交付）

基于 §4.4 的实测，**`force=true` 是不可接受的默认值**：

```text
推荐：PUT /configs（不带 force，即 force=false）
理由：实测 force=false 在端口冲突时保留旧监听器 → 服务不中断（安全降级）

并且 Agent 必须：
 1. 在调用 reload 之前，自行检测目标配置所需端口的可用性；
    （若端口被本实例自己占用 → 属于正常重启场景，应走 restart 而非 reload）
 2. reload 后必须做数据面健康检查（真实连接代理端口），
    不能只看 HTTP 204 与 /configs 的 200；
 3. 健康检查失败 → 立即回滚：切回旧配置版本，并 **重启进程**
    （实测 reload 无法从僵尸态恢复，见 §4.4 T15）；
 4. 绝不在同一 -d 目录上并发启动两个实例（cache.db 锁，见 §6.4）。
```

**关于"端口由本实例持有"的处理**：mihomo 监听端口本就是本进程持有，因此 reload 到**相同端口**的不同配置（最常见的场景：只改 rules/proxies）不会有绑定问题；实测 T7/T8（`{}` 重载同配置）返回 204 且端口保持。**风险仅在"新配置要求一个不同的、且已被他方占用的端口"时出现。**

---

## 9. 对 Agent 架构的影响（Config Domain / Port / Adapter）

### 9.1 Config Domain（保持纯净，无 IO）

```rust
// crates/domain/src/configuration/ —— 纯业务规则，无 tokio/reqwest/fs

pub struct ConfigVersionId(u64);              // 单调递增，可排序
pub struct ConfigChecksum(String);            // SHA-256 of YAML bytes（[推测] 算法由 Agent 自定）
pub struct ConfigVersion {
    id: ConfigVersionId,
    checksum: ConfigChecksum,
    source: ConfigSource,                     // Manual | Subscription(SubscriptionId) | Imported
    created_at: Timestamp,
    // 不含 ConfigPath（路径是 infrastructure 细节）
}

pub enum ValidationStage { Syntax, Schema, Semantic, Resource, Runtime }
pub struct ValidationIssue {
    stage: ValidationStage,
    severity: Severity,                       // Error | Warning  ← 未知字段用 Warning
    message: String,
    // 注意：不要把 mihomo 的原始日志行当消息，需归一化
}
pub struct ValidationResult { issues: Vec<ValidationIssue>, /* ... */ }
```

**Domain 不变量（由本调研支撑）**：

1. 一个 `ConfigVersion` 的 `checksum` 一旦生成不可变（配置是不可变版本）。
2. `Activation` 状态机：`Inactive → Validating → Activating → Active → Failed`；**`Failed` 不得使既有 `Active` 变为 `Inactive`**（对应"失败保留旧配置"）。实测依据：解析失败保留旧配置。
3. `ValidationResult` 只要含任一 `Severity::Error` 即不得进入 `Activating`。
4. **`force` 不是 Domain 概念**：它是 Adapter 的传输参数，Domain 不应出现 `force: bool`。理由：实测 `force` 是"是否拆监听器"的底层行为开关，把它提升为 Domain 概念会把基础设施语义泄漏进核心层（违反 AGENTS.md 依赖方向）。

### 9.2 `ConfigRepository` Port（Application 定义）

```rust
#[async_trait]
pub trait ConfigRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<ConfigVersion>>;
    async fn get(&self, id: ConfigVersionId) -> Result<Option<ConfigVersion>>;
    async fn save(&self, config: ConfigVersion) -> Result<()>;
    async fn activate(&self, id: ConfigVersionId) -> Result<()>;
    /// 生成新版本：写入 + 计算 checksum + 返回版本（不激活）
    async fn stage(&self, yaml: &[u8], source: ConfigSource) -> Result<ConfigVersion>;
}
```

实现要点（Infrastructure，[实测] 支撑）：

- 写入顺序必须是 **write temp → fsync → atomic rename**（AGENTS.md 要求）。实测 mihomo 不支持"配置目录里出现半个文件"的任何恢复机制，因此原子 rename 是唯一保护。
- **`active` 用符号链接**（`active -> configs/v004.yaml`）：实测 reload 的 `path` 模式要求绝对路径，符号链接解析后的真实路径必须在 `SAFE_PATHS` 内 —— 由于配置目录通常就在 `-d` 下，`homeDir` 已覆盖 [上游源码]。若 Agent 把配置目录放在 `-d` 之外（AGENTS.md 建议 `/var/lib/proxy-agent/configs`），**必须显式设置 `SAFE_PATHS=/var/lib/proxy-agent/configs`**，否则 reload 会 400。[实测 T4]
- `store-fake-ip`/`store-selected` 等状态不在 YAML 里，而在 `cache.db`。Agent **不要**把 `cache.db` 纳入版本管理（实测它是首启动自动生成物，且同一目录并发会锁超时）。

### 9.3 `ConfigValidator` Port（Application 定义）—— 本文最重要的新 Port

```rust
#[async_trait]
pub trait ConfigValidator: Send + Sync {
    /// Level 1+2：字段白名单 + mihomo -t
    async fn validate_static(&self, yaml: &[u8]) -> Result<ValidationResult>;

    /// Level 3：在隔离的临时 -d 目录真实启动一次并探活，随后回收
    async fn validate_runtime(&self, yaml: &[u8]) -> Result<ValidationResult>;
}
```

**边界建议（全部由实测支撑）**：

1. **`validate_static` 内部必须做两件事，缺一不可**：
   - Agent 侧字段白名单扫描（补 case 04 的漏过）；
   - `mihomo -t`（补 Agent 侧类型/枚举/引用的覆盖）。
2. **调用 `-t` 前必须由 Agent 自己断言"目标文件存在且非空"** —— 否则会命中"自动创建初始配置"的假成功（§3.3）。**建议实现方式**：用 `-f -` 从 stdin 投喂配置 [上游源码]，从而**完全避免**目标文件不存在的问题，也顺带避免污染磁盘。
3. **`-t` 必须设超时**（实测 geodata 下载可阻塞 90s+）。推荐超时 ≤ 15s，超时即判定为 `Warning`（"可能缺少 geodata"），而非 `Error` —— 否则离线环境的合法配置会被误拒（case 12/13 在离线时被拦截，但这**不是配置的错**，是环境问题）。
4. **geodata 缺失应报为"环境不满足"而非"配置非法"**：与 AGENTS.md 的 `CapabilityStatus` 一致，输出 `Unsupported/Unavailable` 而非 `Misconfigured`。
5. **`validate_runtime` 必须用独立的临时 `-d` 目录**（实测每目录单实例约束 §6.4），并在结束时删除临时目录。
6. **端口策略** [未验证]：临时启动若使用配置中的原始端口，会与该端口上的现有服务冲突（实测 §5.2 会导致"启动成功但无数据面"）。因此 `validate_runtime` 应**改写监听端口为 0 或高端空闲端口**再启动 —— 这需要 mihomo 支持端口 0（实测 `mixed-port: 0` 在 `/configs` 中出现过，但那是**失败后的残留值**，不能证明"传 0 表示自动分配"）。**此项列为开放问题。**

### 9.4 `MihomoController` Port 的收紧（相对 R01 的修订）

R01 定义了 `async fn reload(&self, config: &ConfigPath) -> Result<ReloadOutcome>`。基于本文实测，**必须修订**：

```rust
#[async_trait]
pub trait MihomoController: Send + Sync {
    /// 仅投递配置。返回 204 表示"mihomo 接受了"，不代表"数据面已生效"。
    /// 实现必须使用 payload 模式（规避 SAFE_PATHS），且永不使用 force=true。
    async fn reload(&self, payload: &[u8]) -> Result<()>;

    /// 数据面探活：这是判断 reload 是否真正成功的唯一可靠手段。
    async fn health_check(&self) -> Result<HealthStatus>;
}
```

三条修订理由（全部 [实测]）：

1. **`reload()` 返回 `Ok(())` 不能解释为"配置已生效"**：T14 中 HTTP 204 与数据面死亡同时发生。因此必须有独立的 `health_check()`，且 Application 编排**必须**在 reload 后调用它。
2. **默认不用 `force=true`**（§4.4）。若必须用（例如首次启用某端口），Application 层要显式表达该语义并承担回滚责任。
3. **优先 `payload` 模式**：绕过 `SAFE_PATHS` 与文件存在性两个失败点（§4.6）。若必须用 `path` 模式，Agent 的配置目录必须落在 `-d` 下或显式配置 `SAFE_PATHS`（这应作为 `ConfigRepository` 的部署约束文档化）。

### 9.5 Application 用例的失败语义（必须实现）

```text
ActivateConfig(id):
  1. repo.get(id) → 取 YAML
  2. validator.validate_static(yaml)   → 有 Error 则拒绝（不碰运行实例）
  3. （可选）validator.validate_runtime(yaml)
  4. controller.reload(payload)         → 204 或 Err
  5. controller.health_check()          → 失败则进入回滚
  6. 成功：repo.activate(id)

  任一步失败 → Rollback:
     a. 重新指向旧版本 active 指针
     b. **重启 mihomo 进程**（不是 reload）  ← 实测 reload 无法从僵尸态恢复
     c. 再次 health_check；仍失败 → 报 Fatal，需人工介入
```

**关键点**：步骤 [9]/Rollback 的 (b) **必须是 restart**。这是本文对架构最直接的、由实测强制的结论。

### 9.6 与 AGENTS.md 的一致性检查

| AGENTS.md 要求 | 本文证据支持度 |
|---|---|
| "A failed update ... must preserve the last known-good working configuration" | ✅ 解析失败场景成立；⚠️ **`force=true` 场景不成立** → 必须禁用 `force=true` |
| "If activation or reload succeeds partially and health check fails, attempt rollback" | ✅ **实测强烈支持**：reload 的"部分成功"是真实存在的（204 + 数据面死亡） |
| "Never edit the active configuration in place" | ✅ 支持（原子 rename 是唯一保护） |
| "Config update must support: list/show/validate/diff/activate/rollback" | ✅ `-t` 支持 validate；其余 Agent 自研 |
| "Do not put full generated YAML into SQLite" | ✅ 实测 `cache.db` 是 mihomo 自有状态，不应混用 |
| "Do not log ... full sensitive config contents" | ⚠️ **注意**：mihomo 的 `-t` 与启动会把**错误行与配置片段打到 stdout**（如 `cannot unmarshal !!str 'abc' into int`）。Agent 转发这些日志时需评估是否含敏感值 |

---

## 10. 证据与来源

### 10.1 证据目录（原调研遗留，未修改）

`/tmp/r02-config/`：

| 文件/目录 | 内容 |
|---|---|
| `cases/01..14-*.yaml` | 14 个测试配置（完整矩阵） |
| `help.txt` | `mihomo -h` 输出（全部 flag） |
| `mihomo` | 被测二进制（45,643,538 B） |
| `s1/out.log`、`s2/out.log` | 启动期解析失败（`invalid mode` / YAML 缩进） |
| `s3/out.log`、`s4.log`、`s5.log`、`s6/out.log` | 启动期端口占用（`bind: address already in use`），进程存活 |
| `s1..s6`、`run1`、`d2`、`d3`、`fresh` | 各实验的 `-d` 工作目录（含 `cache.db`、`pid`、`occ.log`） |
| **`rl/`** | **reload 实验主证据**：`run.log`、`resp1..12.txt`、`config{,-v2,-invalid,-portconflict,-tun}.yaml`、`cfgs_after.json`、`occ.log`、`pid` |
| `nope.yaml` | 坏配置样例（实测为 `mixed-port: 7890`，16 B —— 实为**自动生成的初始配置**，非人工坏样例） |
| `src_main.go` | `-t` / `-d` / `-f` 解析、`hub.Parse`、SIGHUP 重载 —— **§2.2/§2.3 的直接来源** |
| `src_hub_executor_executor.go` | `Parse`/`ParseWithPath`/`ParseWithBytes`/`ApplyConfig` —— **§4.1 的直接来源** |
| `src_hub_route_configs.go` | `updateConfigs` handler（含 `IsSafePath` 检查）—— **§4.1/§4.6 的直接来源** |
| `src_constant_path.go` | `Path` 单例、`Resolve`、`IsSafePath`、`SafePaths`、`Cache()`、`MMDB()`、`GeoIP()`、`GeoSite()`、`GetPathByHash` —— **§6.1/§4.6 的来源** |
| `src_config_config.go` | `Config` struct 的 yaml tag —— **§7.2 的来源** |
| `src_component_geodata.go` | geodata loader 注册表 —— **§6.5 的来源** |
| `tree.json` | tag-pinned 完整源码树（sha `ac017cdd...`，1222 项，`truncated:false`）—— **§7.2 的核心证据** |
| `latest.json` | GitHub release API 响应（`tag_name: v1.19.30`） |
| `controller-docs.txt` / `.html` | 官方 controller 文档存档（`SAFE_PATHS` 说明在 4077 行；geodata 在 4147–4173 行） |
| `mihomo-config-docs.html` | 官方 config 文档存档（`schema` 出现 0 次） |
| `pid`、`s4.pid` | 遗留进程 PID（已确认非存活） |
| `search1.json` | GitHub 搜索 → `{"status":"401","message":"Requires authentication"}`（**未取到搜索结果**） |
| `t.go`、`src_config_path.go`、`geo_tree.html`、`sc.tmp` | 均为 `404: Not Found` / `Invalid input.` —— **原调研者两次失败的抓取，无有效证据** |

> **注**：`t.go`、`src_config_path.go`、`geo_tree.html`、`sc.tmp`、`search1.json` 内容均为失败响应（404 / 401 / "Invalid input."），说明原调研者在"GitHub API 搜索"与"抓取 config/path.go"上未成功。本文的相关结论**改用 `src_constant_path.go`**（成功抓取）作为替代证据。

### 10.2 本次补充实测清单（全部在 `/tmp/r02-supp/`，已清理）

| 实验 | 命令/方法 | 覆盖结论 |
|---|---|---|
| `-t` 全矩阵（14 case） | `mihomo -t -d <empty> -f <case>` | §3.1、§3.2 |
| `-t` 副作用：geodata 下载 + 落盘 | case 12（约 90s 超时） | §3.2 |
| `-t` 副作用：损坏 MMDB 被删除 | 预置截断的 `geoip.metadb` 后重跑 case 12 | §3.2 |
| `-t` 副作用：不写 `cache.db` | 空目录 `-t`（直接 + 含 `profile.store-*`） | §3.3 |
| `-t` 副作用：**自动创建缺失的 `-f` 文件** | `-t -f <不存在的路径>` → `exit 0` | §3.3 |
| 正式启动生成 `cache.db` | `mihomo -d <dir>` + 3s 后 kill | §3.3、§6.2 |
| reload：合法 v2（`force=true`） | `PUT /configs?force=true {"path":"v2.yaml"}` → 204 | §4.2 T1 |
| reload：**非法配置保留旧配置** | → 400 `{"message":"invalid mode"}`，端口/模式不变 | §4.3 T2 |
| reload：相对路径 / 白名单外 / 文件不存在 | → 400 ×3（含回显 allowed paths） | §4.6 T3/T4/T5 |
| reload：空 body / `{}` / `{}&force` | → 400 / 204 / 204 | §4.2 T6–T8 |
| reload：payload 模式（合法/非法） | → 204 / 400 | §4.2 T9/T10 |
| reload：错误 secret | → 401 | §4.2 T11 |
| reload：**端口冲突 + `force=false`** | → 204，**旧端口继续服务** | §4.4 T12/T13 |
| reload：**端口冲突 + `force=true`** | → 204 但 `mixed-port:0`、**旧 listener 被拆** | §4.4 T14 |
| reload：**僵尸态不可自愈** | 连续 2 次合法 reload 仍 `mixed-port:0` | §4.4 T15 |
| 重启可恢复 | kill + 重启 → 端口恢复 | §4.4 T15 |
| 启动失败：非法配置 | `exit 1`，`level=fatal` 在 stdout，无残留 | §5.1 |
| 启动失败：端口占用 | **进程不退出**，控制器 200，无代理端口 | §5.2 |
| 全部进程清理 | `pkill -f r02-supp`，逐端口 `nc -z` 验证关闭 | — |

**清理确认** [实测]：所有补充实验进程已 kill；端口 `17901/17902/17903/17909/19101/19102/17911/19111/17890/19090` 全部 `closed`；`/tmp/r02-config/` 中无任何文件 mtime 晚于 `2026-09-12 13:34`（即**既有证据未被修改**）。

### 10.3 外部来源

- [Mihomo v1.19.30 release](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30)
- [Mihomo config 文档](https://wiki.metacubex.one/en/config/)（存档 `mihomo-config-docs.html`）
- [Mihomo external controller 文档](https://wiki.metacubex.one/en/config/general/)（存档 `controller-docs.txt`）
- 内部交叉引用：`docs/research/01-mihomo.md`（R01 reload / SAFE_PATHS / `PUT /configs` 结论）、`docs/research/09-linux-runtime.md`（R09 capability 与 TUN 降级）、`docs/phase-0-architecture-discovery.md`（R02 骨架）

---

## 11. 未验证假设与开放问题

### 11.1 明确未验证（不要当作结论使用）

| # | 未验证项 | 原因 | 影响 |
|---|---|---|---|
| U1 | **Linux/LXC 上的全部行为** | 本机为 darwin/arm64；所有实测在 macOS 完成 | TUN、iptables/nftables、listener 权限、`/dev/net/tun` 全部未在目标平台验证 |
| U2 | `force=false` 在**端口冲突**时"保留旧 listener"的**内部机制** | 只从外部行为（端口仍 open、`/configs` 未变）推断，未逐行审阅 `updateListeners` 源码（该文件未抓取） | 结论本身由实测支撑，但机制描述为 `[推测]` |
| U3 | `mixed-port: 0` 是否表示"自动分配端口" | 只在失败残留中观察到 `0`，未主动测试"配置写 0"的语义 | 阻塞 §9.3 第 6 点的 `validate_runtime` 端口策略 |
| U4 | `-t` 从 **stdin（`-f -`）** 读取的可行性 | `main.go` 显示支持 [上游源码]，但本次未实跑 | 影响 §9.3 建议 2 的落地方式 |
| U5 | 上游 geodata 下载在**可达网络**下是否成功 | 本机 90s 内超时，未获得成功样本 | case 12/13 的"拦截"是环境相关，不可复现为配置错误 |
| U6 | `PUT /configs` 在 `payload` 模式下是否有大小限制 | 未测试大 payload（如数 MB 订阅） | 影响 Agent 是否总能走 payload 通道 |
| U7 | reload 期间（`OnSuspend` → `OnRunning` 窗口）的连接行为 | 未测量该窗口内的丢包/连接失败 | 影响"reload 是否会造成瞬时中断"的 SLA 声明 |
| U8 | R01 提到的 `features.CMFA` 对 `IsSafePath` 的短路影响 | `src_constant_path.go` 显示 `if p.allowUnsafePath \|\| features.CMFA { return true }`，但未确认本二进制是否带 CMFA tag | 实测本二进制**确实**执行了 safe 检查（T4 返回 400），故本二进制非 CMFA |
| U9 | `cache.db` 的 schema 与迁移行为 | 只确认了文件大小（65536 B）与并发锁超时 | 影响 Agent 是否需要备份/迁移 cache |
| U10 | 官方 `/configs` 端点对 `mode` 之外的**运行时**变更（`PATCH /configs` 白名单字段） | 属 R01 范围，本文未展开 | 见 R01 |

### 11.2 新增开放问题（建议并入 `docs/research/open-questions.md`）

**Q-R02-1 — reload 的 `force` 策略如何固化？**
实测 `force=true` 会导致不可自愈的数据面死亡（§4.4 T14/T15），而 `force=false` 下"端口变更"不会生效（T13）。问题：Agent 是否需要支持"换端口"这一场景？若需要，是否必须走 `restart`（带短暂中断）而不是 `reload`？建议：**默认 `force=false` + 换端口场景强制 restart**。

**Q-R02-2 — 僵尸态如何被检测？**
实测"进程存活 + 控制器 200 + 无数据面"是可达状态（§5.2、§4.4）。Agent 的 `health_check()` 必须以**数据面**为准（连接代理端口或检查 `/configs` 的 `mixed-port != 0`）。需要确定：健康检查的具体判据与频率？（关联 R03）

**Q-R02-3 — geodata 的分发策略？**
实测上游 GitHub release 下载在 90s 内不可达（§6.3）。Agent 是否需要内置 geodata（体积：`geoip.metadb` ≈ 4.2 MB）、或提供离线注入？（关联 R13 licenses 与 packaging）

**Q-R02-4 — 未知字段白名单的维护策略？**
实测 `-t` 不检测未知字段（case 04），而上游持续新增字段。Agent 是用"硬编码白名单"（需随版本更新，误伤新字段）还是"仅对已知字段做类型校验 + 未知字段 warning"？建议后者，但需确认误报率。

**Q-R02-5 — `validate_runtime` 的端口改写是否可行？**
阻塞于 U3（`mixed-port: 0` 语义）。若不可行，则"临时启动健康检查"必须占用真实端口，会与运行中的实例冲突，需要在架构上改为"只对全新部署做运行时校验"。

**Q-R02-6 — `-t` 的调用是否必须走 stdin？**
为规避"自动创建缺失文件"的假成功（§3.3），建议走 `-f -`。阻塞于 U4。

### 11.3 对 R02 骨架的修订建议

`docs/phase-0-architecture-discovery.md` 的 R02「最终确定」为：

```text
Generated Config → Syntax Validation → Semantic Validation → Temporary Start → Health Check → Activate
```

基于实测，建议修订为（新增 **Resource Preflight**、**Activate** 后的 **Reload + Health Check**、以及 **Rollback**）：

```text
Generate
  → Syntax Validation          (-t 可靠)
  → Semantic Validation        (-t ⊕ Agent 字段白名单)
  → Resource Preflight         (geodata / provider / 端口)   ← 新增
  → Temporary Start            (隔离 -d 目录, 真实数据面探活)
  → Health Check
  → Activate                   (原子 rename + 版本记录)
  → Reload                     (PUT /configs, payload, 不用 force=true)
  → Health Check               (数据面为准, 204 不等于成功)   ← 新增
  → 失败 → Rollback            (切回旧版本 + **restart**)      ← 新增
```

修订的**直接依据**：

- **新增 Resource Preflight**：§3.2 case 12/13（geodata 触发 90s 网络阻塞）、case 14（provider 不校验）、§4.4（端口冲突是唯一的破坏性失败源）；
- **新增 reload 后的 Health Check**：§4.4 T14（204 与数据面死亡并存）；
- **新增 Rollback（且必须是 restart）**：§4.4 T15（reload 无法自愈僵尸态）。
