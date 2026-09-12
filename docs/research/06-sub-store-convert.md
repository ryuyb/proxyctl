# R06 — sub-store-convert 评估

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：`[实测]` + `[上游源码]` + `[上游文档]` + `[上游元数据]`
> 关键结论一句话：**Rejected —— sub-store-convert 是官方 Sub-Store `proxy-utils` 的"薄抽取 + 重新打包"，能力是 Sub-Store 的严格子集（无 operator/script/process、无 mergeSources、无完整配置产出），维护上是 2 stars / 1 人 / 0 issue / 0 tag / 0 GitHub Release 的单人项目（2025-03～2025-09 有 8 个月静默），且其 npm 产物把 Sub-Store 的 AGPL 源码内联打包却只标称 MIT、不含任何许可声明；它相对 Sub-Store 唯一的优势（体积小）也被官方 Release bundle（3.0 MiB、零依赖、130 ms 冷启动）抵消。当前证据下不进入 MVP 的 Converter Adapter 集合。**

---

## 1. 结论摘要（TL;DR）

1. `[上游元数据]` **仓库定位**：<https://github.com/tbxark/sub-store-convert>（**不是** `xream/sub-store-convert`，后者 404；也不是 `sub-store-org/*`）。作者 `tbxark`，npm 包名 `sub-store-convert`（unscoped，只有核心库被发布；`@sub-store-convert/core|cli|app` 在 npm 上均为 404）。
2. `[实测]` **能力是 Sub-Store 的严格子集**：它只复用 Sub-Store 的 `parsers` / `producers` / `preprocessors` 三个入口（`packages/core/src/index.js:1-3`），**完全没有** operator / script filter / process pipeline（Sub-Store 对应实现在 `core/proxy-utils/index.js:123-233`），也**没有** `mergeSources`。已发布 bundle 里 `operator` 出现 0 次、`mergeSources` 出现 0 次。
3. `[实测]` **输出只有节点列表，不是完整配置**：`target=mihomo` 的产物是 `proxies:\n  - {...}`，**没有** `proxy-groups` / `rules` / 完整配置骨架。这决定了它无法替代 Sub-Store 的 "produce 完整配置" 语义。
4. `[实测]` **API 不兼容 Sub-Store**：它提供的是 subconverter 风格的 `GET /sub?target=&url=`（多个 URL 用 `|` 分隔），而 Sub-Store 是 `GET /download/sub?url=&ua=&content=&mergeSources=&ignoreFailedRemoteSub=&produceType=&resultFormat=&noCache=&prettyYaml=&platform=...`。参数面、语义、路径三者都不同，**adapter 不能共用 URL 构造代码**。
5. `[实测]` **失败面严重违反本项目的核心不变量**：远端 404 时无状态码校验（把 body 当订阅内容）；网络不可达时 `loadRemoteData()` 吞掉异常返回 `[]`，`convert()` 仍然 **resolve 成功并输出 `proxies:\n`（9 bytes，0 个节点）**。调用方无法区分"订阅为空"与"拉取失败"——直接威胁 "失败的更新绝不能破坏/覆盖当前可用配置"。
6. `[实测]` **还存在一个与上游行为不一致的回归**：同样 4 行明文 URI 列表，Sub-Store 的 `preprocess()` 在无 preprocessor 命中时 `return raw`（`core/proxy-utils/index.js:50-62`），而 sub-store-convert 的 `loadRemoteData()` 返回 `[]`（`packages/core/src/index.js:46`）→ 明文订阅（非 base64、非 Clash YAML）转换结果为**空**。已验证该 4 行经 `parseProxyLines()` 可解析出 4 个节点（801 bytes），即缺陷在 wrapper 层而非 parser 层。
7. `[上游元数据]` **维护画像**：2 stars / 1 fork / 1 watcher / 0 open issues / 0 GitHub Releases / 0 git tag / 70 commits（其中 22 条是 dependabot）/ 2 contributors（`tbxark` 48 + `dependabot[bot]` 22）/ 27 个 PR 全部来自 dependabot / **从未有过任何 issue**。npm 月下载 121 次、周下载 8 次。
8. `[实测]` **优点是真实的，但不足以构成引入理由**：安装极轻（npm 5.1 s、9 个包、`node_modules` 7.6 MB）、导入 ~50 ms、HTTP 服务 ~200 ms 可服务、转换 0.6–11 ms、无 DB、无 geodata/MMDB 依赖。但官方 Sub-Store 的 Release bundle 路径更轻（3.0 MiB、`npm: []`、无需 `node_modules`、冷启动 130 ms，见 R05），且没有许可问题。
9. `[上游源码]` **跟随上游靠人工**：Sub-Store 以 git submodule 固定在 `packages/core/src/vendors/Sub-Store`，靠维护者手工执行 `TAG=<version> pnpm run update:vendor`（`packages/core/scripts/update.sh`）同步。当前 pin 在 `2.36.33`（2026-08-10），而上游已到 `2.39.6`（2026-09-11）——**落后约 1 个月 / 3 个 minor**。
10. `[上游产物]` **许可与再分发**：仓库 `LICENSE` 是 MIT（`Copyright (c) 2024 tbxark`），但发布的 tarball 只有 `index.js` + `package.json`，**不含 LICENSE、不含任何 AGPL/版权声明**，而 `index.js` 内联了 27 个 Sub-Store（AGPL-3.0）源文件（31 处 `// src/vendors/Sub-Store/...` 路径标记，我是独立抓取核对，与 `13-licenses.md` 的 sha256 `415aa63…` 一致）。→ **不得 Bundled、不得作为分发物**（详见 §8 与 `docs/research/13-licenses.md`）。

---

## 2. 仓库客观数据（抓取日期 + 原始数字）

**抓取日期：2026-09-12**（本机 macOS Darwin arm64；`github.com` 直连超时，GitHub REST API 未认证配额被并行调研耗尽，故数据来源为：npm registry 直连、`raw.githubusercontent.com` 直连、`ghfast.top` 代理 git clone、`ungh.cc` 只读 API 代理；已注明来源）。

### 2.1 仓库身份与静态元数据

| 项目 | 值 | 来源 |
| --- | --- | --- |
| 仓库 | `tbxark/sub-store-convert` | `[上游元数据]` GitHub API `/repos/tbxark/sub-store-convert`（2026-09-12 12:34 抓取，成功）+ `[上游元数据]` ungh.cc 复核 |
| 仓库 URL | <https://github.com/tbxark/sub-store-convert> | 同上 |
| 描述 | "A tool that runs the node conversion logic independently from the Sub-Store." | 同上 |
| `created_at` | **2024-11-15T08:08:48Z** | 同上 |
| `pushed_at` | **2026-09-11T06:38:41Z** | 同上 |
| `updated_at` | **2026-09-11T06:38:40Z** | 同上 |
| HEAD commit | `2e0635ff3fb0e97d7295098617e590c5b9968b4f`，2026-09-10T00:31:38Z，`dependabot[bot]`，`chore(deps): bump hono …` | `[实测]` 本地 `git log` |
| `stargazers_count` | **2** | `[上游元数据]` |
| `forks_count` | **1** | `[上游元数据]` |
| `subscribers_count` | **1** | `[上游元数据]` |
| `open_issues_count` | **0**（`has_issues: true`，即 issue 功能开启但为 0） | `[上游元数据]` |
| `archived` / `disabled` | **false / false** | `[上游元数据]` |
| `default_branch` | **master** | `[上游元数据]` |
| `language` | JavaScript | `[上游元数据]` |
| `size` | 383（GitHub 计 KB 字段） | `[上游元数据]` |
| `topics` | `["sub-store"]` | `[上游元数据]` |
| `homepage` | `https://www.npmjs.com/package/sub-store-convert?activeTab=readme` | `[上游元数据]` |
| `license` | GitHub 识别为 **MIT**（spdx `MIT`） | `[上游元数据]` |

补充说明：`xream/sub-store-convert` → **HTTP 404**（`[实测]` 2026-09-12）；GitHub 仓库搜索 `sub-store-convert` 全站只有 `tbxark/sub-store-convert` 一个相关仓库（`[上游元数据]` search API，total_count=12，其余 11 条为无关项目）。

### 2.2 License 原文

`[上游源码]` `https://raw.githubusercontent.com/tbxark/sub-store-convert/master/LICENSE`（2026-09-12 抓取，1063 bytes）：

```text
MIT License

Copyright (c) 2024 tbxark
...
```

同仓库 `packages/{core,app,cli}/package.json` 均为 `"license": "MIT"`。**注意**：npm 发布产物内**没有** LICENSE 文件（tarball 仅 2 个文件，见 §2.4）。

### 2.3 提交活跃度（本地 git clone 全历史统计）

`[实测]` 通过 `https://ghfast.top/https://github.com/tbxark/sub-store-convert.git` clone（含完整历史）。

| 指标 | 值 |
| --- | --- |
| 总 commit 数 | **70** |
| 首个 commit | `d6c1950`，2024-11-15 16:07:25 +0800，tbxark，"Initial commit" |
| 作者分布 | `tbxark <tbxark@outlook.com>` **48**；`dependabot[bot]` **22** |
| contributors 列表长度 | **2**（`tbxark` 48 / `dependabot[bot]` 22） |
| merge commit | **0**（全部直接推到 master，无 PR review 流程） |
| 最近 30 天 commit | **1**（dependabot 的 hono bump） |
| 最近 90 天 commit | **14** |
| 最近 180 天 commit | **27** |
| 最近 365 天 commit | **60** |

按月分布（`git log --date=format:%Y-%m`，仅列有提交的月份）：

```text
2024-11: 7   2024-12: 1   2025-01: 1   2025-02: 1
2025-03 ~ 2025-09: 0  ← 连续 8 个月零提交
2025-10: 15  2025-11: 1
2026-01: 7   2026-02: 4   2026-03: 7   2026-04: 3
2026-05: 4   2026-06: 6   2026-07: 8   2026-08: 4   2026-09: 1
```

最近 20 次 commit 的形态（`[实测]`，节选 12 条，时间为 commit date）：

| 日期 | 作者 | 提交 |
| --- | --- | --- |
| 2026-09-10 | dependabot[bot] | chore(deps): bump hono … |
| 2026-08-10 | tbxark | chore: Bump version to 2.36.33 and update Sub-Store submodule |
| 2026-08-04 | dependabot[bot] | chore(deps): bump hono … |
| 2026-08-04 | dependabot[bot] | chore(deps): bump the npm_and_yarn group … |
| 2026-08-02 | tbxark | chore: Bump version to 2.36.27 and reformat core build code |
| 2026-07-29 | tbxark | chore: Bump dependencies to latest versions |
| 2026-07-29 | tbxark | chore: Bump version to 2.36.25 and update Sub-Store submodule |
| 2026-07-24 | tbxark | chore: update dependencies across packages |
| 2026-07-24 | dependabot[bot] | chore(deps): bump the npm_and_yarn group … |
| 2026-07-24 | tbxark | feat: update to v2.36.20 with shadow-tls enhancements and Surfboard TUIC support |
| 2026-07-18 | tbxark | chore: update Sub-Store vendor and sync package versions to 2.36.10 |
| 2026-07-02 | tbxark | chore: update Sub-Store vendor and bump version to 2.33.1 |

**关于"最近 20 次 commit 的作者与时间分布"**：20 条中人造提交 10 条、dependabot 10 条；人造提交几乎全部是 `chore: update Sub-Store vendor / Bump version`，真正含功能描述的是极少数（如 2026-07-24 的 `feat: update to v2.36.20 with shadow-tls enhancements and Surfboard TUIC support`，其内容也来自上游 Sub-Store）。**结论：主要维护行为 = 定期把上游 submodule 往前推一格。**

### 2.4 Release / tag / npm 发布

| 指标 | 值 | 来源 |
| --- | --- | --- |
| git tag 数 | **0** | `[实测]` `git tag \| wc -l` |
| GitHub Releases | **0**（`{"releases":[]}`） | `[上游元数据]` `https://ungh.cc/repos/tbxark/sub-store-convert/releases` |
| npm 包名 | `sub-store-convert`（unscoped） | `[上游元数据]` registry |
| npm `created` | 2024-09-23T13:12:30Z（比 GitHub 仓库首个 commit 早约 2 个月 `[推测]` 曾用别的仓库/来源） | `[上游元数据]` |
| npm `modified` | 2026-08-10T16:01:24Z | 同上 |
| 版本总数 | **23** | 同上 |
| `dist-tags` | `latest: 2.36.33`，`beta: 2.20.30-b` | 同上 |
| 最新版本发布时间 | **2026-08-10T16:01:23Z** | 同上 |
| `@sub-store-convert/core` / `cli` / `app` | npm 上**均 404（未发布）** | `[实测]` registry 查询 |
| 月下载量 | **121**（2026-08-12 ~ 2026-09-10） | `[上游元数据]` `api.npmjs.org/downloads/point/last-month` |
| 周下载量 | **8**（2026-09-04 ~ 2026-09-10） | 同上 |
| 发布产物 | tarball 66,658 B；解包 420,260 B；`fileCount: 2`（仅 `index.js` 419,813 B + `package.json`）；sha256 `415aa639165966013752de90eb03b2130023df2cd066280b8710cdd1eac73663` | `[实测]` 下载 tarball 校验 |
| 运行期依赖 | `js-base64@^3.7.2`、`json5@^2.2.3`、`lodash@^4.17.21`、`peggy@^2.0.1`、`yaml@^2.9.0`（5 个） | `[上游源码]` `packages/core/package.json` |
| npm 元数据缺陷 | 无 `repository`、无 `homepage`、无 `engines` 字段 | `[实测]` registry |

最近 8 次 npm 发布时间（`[实测]` registry `time` 字段）：

```text
2.24.7   2026-06-06T10:21:01Z
2.24.19  2026-06-13T08:54:26Z
2.24.22  2026-06-17T10:18:29Z
2.33.1   2026-07-02T07:13:05Z
2.36.10  2026-07-18T07:24:36Z
2.36.20  2026-07-24T01:20:08Z
2.36.27  2026-08-02T07:13:51Z
2.36.33  2026-08-10T16:01:23Z
```

→ 近期发布节奏约 **每 1–3 周 1 次**，节奏由 Sub-Store 上游发版驱动，而非本项目自身需求。

### 2.5 Issue / PR（用户侧反馈面）

`[上游元数据]` GitHub search API（独立配额，2026-09-12 可用）：

| 查询 | `total_count` |
| --- | --- |
| `repo:tbxark/sub-store-convert type:issue` | **0** |
| `repo:tbxark/sub-store-convert type:issue is:closed` | **0** |
| `repo:tbxark/sub-store-convert type:pr` | **27** |
| `repo:tbxark/sub-store-convert type:pr is:merged` | **23** |

- **open/closed issue 比例：0 / 0 —— 该项目历史上从未有过任何 issue。**
- **无法举出"长期无人回复的 issue"实例，因为不存在 issue**；这本身就是更强的负面信号：没有公开支持渠道、没有用户反馈闭环、没有已知缺陷清单。
- 27 个 PR **全部由 `dependabot[bot]` 提交**（逐一核对标题，均为 `chore(deps): bump …`），23 merged / 4 closed-unmerged（被后续 bump 取代）。**没有任何来自外部贡献者的功能或修复 PR。**

### 2.6 与官方 Sub-Store 的对照数据（同期抓取，2026-09-12）

| 指标 | `tbxark/sub-store-convert` | `sub-store-org/Sub-Store` |
| --- | --- | --- |
| stars | **2** | **10,451** |
| forks | **1** | **1,359** |
| watchers | **1** | **91** |
| contributors | **2** | **30**（`xream` 1099 / `Peng-YM` 393 / …） |
| created_at | 2024-11-15 | 2020-08-19 |
| pushed_at | 2026-09-11 | 2026-09-12 |
| 最新版本 | npm `2.36.33`（2026-08-10） | Release `2.39.6`（2026-09-11T13:51:54Z） |
| 发布频率 | npm 每 1–3 周 1 次 | **32 天 30 个 Release ≈ 0.94 次/天**（feed 区间 2026-08-10 ~ 2026-09-11） |
| License | 自称 MIT（见 §8） | `LICENSE` 原文 = **AGPL-3.0**；`backend/package.json` 却写 `"license": "GPL-3.0"`（元数据不自洽，以 LICENSE 原文为准） |

来源：`[上游元数据]` `https://ungh.cc/repos/sub-store-org/Sub-Store{,/releases,/contributors}`（2026-09-12）。

**版本落后量化**：sub-store-convert pin 的上游版本 = `2.36.33`；上游最新 = `2.39.6`。**落后 3 个 minor、约 1 个月、期间上游发了 30 个 Release。**

---

## 3. 技术形态与能力覆盖

### 3.1 形态：库 + CLI + HTTP 服务 + Worker + Docker（但只有库被发布）

`[上游源码]` 这是一个 pnpm workspace（`pnpm-workspace.yaml` → `packages/*`），共 3 个包 + 1 个 Dockerfile：

| 包 | 形态 | 发布状态 |
| --- | --- | --- |
| `@sub-store-convert/core` | **转换库**（ESM，`main/module → ./build/index.js`），导出 `convert()` 等 | 源码内构建产物 `build/package.json` 改名为 `sub-store-convert` 后 `npm publish`（`packages/core/package.json` 的 `publish` 脚本：`cd build && npm publish`）→ **只有它被发布** |
| `@sub-store-convert/app` | **HTTP 服务**（Hono + `@hono/node-server`；同一 `app.fetch` 也用于 Cloudflare Workers，`wrangler.jsonc`） | npm 404，**未发布** |
| `@sub-store-convert/cli` | **CLI**（`bin: { "sub-store-convert": "./src/index.js" }`，`node:readline/promises` 交互提示 target） | npm 404，**未发布**（`bin` 里声明的命令名与 npm 包同名，但包没发，属于不可安装状态） |
| Dockerfile | 两阶段：`node:22-alpine` 构建 → 运行时 **`oven/bun:1-alpine`**，`CMD ["bun", "./packages/app/src/server.js"]`，`EXPOSE 3000` | 镜像 `ghcr.io/tbxark/sub-store-convert:latest`，`[未验证]`（见 §5.4） |

`[上游文档]` README 自述："runs the node conversion logic from Sub-Store as a standalone library, CLI, HTTP service, and Cloudflare Worker"。

### 3.2 支持的目标格式（从 producers 源码确认的权威枚举）

`[上游源码]` `packages/core/src/vendors/Sub-Store/backend/src/core/proxy-utils/producers/index.js` 的导出键（即 `loadProducer()` 的合法取值，大小写不敏感匹配）：

| 目标 | 接受的别名 | 实现文件 |
| --- | --- | --- |
| Quantumult X | `qx`、`QX`、`QuantumultX` | `producers/qx.js` |
| Surge | `surge`、`Surge` | `producers/surge.js` |
| Surge Mac | `SurgeMac` | `producers/surgemac.js` |
| Loon | `Loon` | `producers/loon.js` |
| Clash | `Clash` | `producers/clash.js` |
| **Mihomo / Clash.Meta** | `meta`、`clashmeta`、`clash.meta`、`Clash.Meta`、`ClashMeta`、`mihomo`、`Mihomo` | `producers/clashmeta.js` |
| URI | `uri`、`URI` | `producers/uri.js` |
| V2Ray | `v2`、`v2ray`、`V2Ray` | `producers/v2ray.js` |
| JSON | `json`、`JSON`（`type: 'ALL'`，输出全部节点数组） | 内联函数 |
| Stash | `stash`、`Stash` | `producers/stash.js` |
| Shadowrocket | `shadowrocket`、`Shadowrocket`、`ShadowRocket` | `producers/shadowrocket.js` |
| Surfboard | `surfboard`、`Surfboard` | `producers/surfboard.js` |
| sing-box | `singbox`、`sing-box` | `producers/sing-box.js` |
| Egern | `egern`、`Egern` | `producers/egern.js` |

共 **13 个独立 producer 文件 + 1 个内联 JSON producer = 14 个 target 键族**（`producers/` 目录下共 15 个 `.js`，其中 `index.js` 为汇总入口、`utils.js` 为工具模块，均非 producer）。`[实测]` 逐一调用 15 个 target 字符串，全部返回成功（内容正确性见 §5.2）。

**未包含**：`Nezha`（Sub-Store 的 `/download/sub` 里有 Nezha 相关分支）、完整配置产出（见 §3.3）、任何订阅管理/组合/file/artifact 形态。

### 3.3 能力覆盖：**没有** operator / script / process / mergeSources

`[上游源码]` `packages/core/src/index.js` 的全部 vendor 引用只有 3 行：

```js
import parsersImport from  './vendors/Sub-Store/backend/src/core/proxy-utils/parsers/index.js'
import produceImport from './vendors/Sub-Store/backend/src/core/proxy-utils/producers/index.js'
import preprocessorsImport from './vendors/Sub-Store/backend/src/core/proxy-utils/preprocessors/index.js'
```

`[实测]` 已发布 bundle `index.js` 的关键字计数：

```text
operator              0
mergeSources          0
Script Operator       0
ResolveDomainOperator 0
导出符号: buildProxyServer, convert, loadProducer, loadRemoteData,
          parseProxyLines, parsers, preprocessors, produce,
          produceOutput, tryParseProxy
```

对照 Sub-Store 自身：`core/proxy-utils/index.js` 里有 `process(context)` 与 `operators` 数组（第 123–233 行），支持 operator 链、script filter（`// process script` 注释处）等。**这些在 sub-store-convert 中不存在，不是"没暴露"，而是根本没被 import 进 bundle（esbuild tree-shake / 未引用即不打包）。**

`[实测]` `mergeSources` 也不存在：`/sub` 只接受 `target` + `url`，`url` 用 `|` 分隔（`packages/core/src/index.js:158` `const urls = url.split('|')`），语义是"多个远端 URL 的节点简单拼接"，**不是** Sub-Store 的 `mergeSources`（合并本地/远端订阅并保留其 operator 配置）。

`[实测]` **输出不是完整配置**：`convert('…','mihomo')` 的输出全文以 `proxies:` 开头且仅含该键：

```yaml
proxies:
  - {"type":"ss","skip-cert-verify":false,"udp":true,"server":"1.2.3.4","port":"8388","cipher":"aes-256-gcm","password":"password123","name":"SS-Node-A"}
  - {"name":"VMess-Node-B","type":"vmess","server":"5.6.7.8","port":443,"cipher":"auto","uuid":"11111111-2222-3333-4444-555555555555","alterId":0,"tls":true,"network":"ws","ws-opts":{"path":"/vmess","headers":{"Host":"example.com"}},"udp":true}
  - {"type":"trojan","password":"trojanpass","server":"9.9.9.9","port":443,"name":"Trojan-Node-C","sni":"example.org","udp":true}
  - {"type":"vless","name":"VLESS-Node-D","server":"10.0.0.1","port":443,"uuid":"11111111-2222-3333-4444-555555555555","udp":true,"tls":true,"skip-cert-verify":false,"packet-encoding":"xudp","network":"ws","ws-opts":{"path":"/vless"},"encryption":"none","servername":"t.example.com"}
```

即：**它输出的是 proxy 列表片段**（注意 `clashmeta` producer 的每个节点被序列化成单行 JSON flow-style，而不是常规 YAML block style）。对我们的 Agent 而言，这意味着仍需自己拼装 `proxy-groups` / `rules` / 其他顶层键（这本来也是 Agent 的职责，见 `docs/architecture.md` 的配置模板思路）。

### 3.4 核心 API（库层）

`[上游源码]` `packages/core/src/index.js`：

```js
export async function convert(url, target, opts = {}) -> Promise<string>
// url: 远端订阅 URL，多个用 '|' 分隔
// target: producers 的键（大小写不敏感）
// opts: 合并进每个 proxy 对象（{...proxy, ...opts}，name 会 trim）
```

`[上游源码]` `opts` 的合并语义是**逐节点对象展开**（`buildProxyServer(proxy, opts)`），不是"转换选项"对象——例如传 `{udp:true}` 会给所有节点强制加 `udp: true`，传 `{target:…}` 之类无意义键也会被原样写进节点（HTTP 层已 `delete opts.target/url`，库层没有）。这会导致**节点级字段被覆盖**，与 Sub-Store 的 options 语义不同。

`[上游源码]` `produceOutput()`：`producer.type === 'ALL'`（即 `json`）走整体产出，其他 producer **逐个节点 `try/catch`，不支持的节点被静默丢弃**（`catch { /* skip proxies the target producer does not support */ }`）。→ **能力缺失永远不会报错，只会少节点**，这对"配置正确性校验"是隐藏风险。

### 3.5 运行依赖、体积、启动时间

`[实测]` 环境：macOS Darwin arm64，Node **v25.2.1**，npm **11.6.2**，pnpm 12.3.4（Docker 可用但 registry 不可达，见 §5.4）。

| 指标 | 实测值 | 方法 |
| --- | --- | --- |
| Node 版本要求 | README 写 "Node.js 22 or later is recommended"；`package.json` **无 `engines` 字段**（无强制校验） | `[上游文档]` + `[实测]` registry |
| Bun 是否必需 | **不**。`app` 的 start 脚本是 `bun ./src/server.js \|\| node ./src/server.js`；Docker 运行时镜像才用 Bun | `[上游源码]` `packages/app/package.json`、`Dockerfile` |
| `npm install sub-store-convert` | **5.1 s**（npm 自报 real），9 个包（含自身） | `/usr/bin/time -p`、`npm ls --all --parseable \| wc -l` |
| `node_modules` 体积 | **7.6 MB**（其中 `sub-store-convert/` 416 KB） | `du -sh` |
| 库导入冷启动 | **~0.05 s** ×3 次一致 | `/usr/bin/time -p node -e "import('sub-store-convert')"` |
| HTTP 服务启动到首个 200 | **~0.2 s** | 起进程后每 100 ms 轮询 `/sub` |
| 转换耗时（本地 4 节点 base64 订阅） | mihomo 首次 **10.7 ms**，之后 **0.6–4.0 ms** | `performance.now()` 包裹 |
| HTTP 端到端（含 fetch 订阅） | **33.7 ms** | `curl -w '%{time_total}'` |
| 是否需要 geodata / MMDB | **不需要**。bundle 内 `maxmind` / `mmdb` / `geoip` 出现次数均为 **0** | `[实测]` grep bundle |
| 运行期网络需求 | **仅拉取订阅 URL**（无状态、无 DB、无外部服务、无 MMDB 下载）；若订阅在远端则需要外网 | `[实测]` 纯本地 HTTP 订阅即可完成转换 |
| 持久化 | **无**（无 DB、无数据目录、无配置文件） | `[上游源码]` 全仓库无 DB/配置文件读写 |

`[实测]` 体积对照：官方 Sub-Store 的 Release bundle 路径为 3.0 MiB 单文件 + 零 `node_modules`（见 `docs/research/05-sub-store-deployment.md` §3），**而 sub-store-convert 走 npm 安装需要 7.6 MB `node_modules`；走源码自建还需要 pnpm install + esbuild 构建**。所以"更轻"这个卖点并不成立。

### 3.6 HTTP API

`[上游源码]` `packages/app/src/index.js`（Hono）：

```js
app.get('/', c => c.redirect('https://github.com/tbxark/sub-store-convert', 302))
app.get('/sub', async c => { … })
```

- 必需参数：`target`、`url`；缺失 → `400 "Missing target or url"`。
- `url` 多个用 `|` 分隔（须 URL-encode 成 `%7C`）。
- 其余 query 参数作为 `opts` 透传；**字符串 `'true'`/`'false'` 转 boolean，纯数字转 Number**，其他保持字符串。
- 未知 target → 抛错 → `500`，body 为 `Unknown target: <x>`。
- 成功 → `text/plain` 返回转换结果。
- `[上游源码]` `packages/app/src/server.js`：默认 `PORT=3000`，**`hostname: '0.0.0.0'` 硬编码**（默认监听所有接口）；无任何认证/Token/CORS 限制；无缓存、无 `/download/sub`、无 `/api/*`。
- `[实测]` 端点行为逐条验证（本地跑通，见 §5.3）：`GET /sub?target=mihomo&url=…&udp=true` → `200`，`/sub` → `400`，`/sub?target=bogus` → `500`，`/` → `302`。

### 3.7 与 Sub-Store `/download/sub` 的参数兼容性：**不兼容**

`[上游源码]` Sub-Store `backend/src/restful/download.js:149-167` 中 `downloadSubscription()` 解构的 query 参数包括：

```text
url, ua, content, mergeSources, ignoreFailedRemoteSub, produceType,
includeUnsupportedProxy, resultFormat, proxy, noCache, _fakeNode, fakeSub,
prettyYaml, platform / target, name …
```

其中 `url` 在 Sub-Store 语境下可以是**订阅名、分享链接、base64 资源引用**（`download.js:242-245`：非 `http(s)://` 时把 `url` 当 `content`），而 sub-store-convert 的 `url` **只能是裸的远端 URL**。

结论（`[实测]` + `[上游源码]`）：

| 维度 | Sub-Store `/download/sub` | sub-store-convert `/sub` |
| --- | --- | --- |
| 路径 | `/download/sub` | `/sub` |
| 定位方式 | `url` = 订阅名 / 分享链接 / base64 资源 / 远端 URL | `url` = 远端 URL（`\|` 分隔多个） |
| 合并 | `mergeSources` | 无 |
| 失败容忍 | `ignoreFailedRemoteSub` | 无 |
| 产出控制 | `produceType` / `resultFormat` / `includeUnsupportedProxy` / `prettyYaml` | 仅 `target` + 节点级 opts |
| 认证/缓存 | Token、`noCache` | 无 |

→ **Adapter 必须两套 URL 构造逻辑，绝不能复用。** 这直接支持 AGENTS.md 中 "Never spread external API URL construction throughout the codebase" 的约束。

---

## 4. 与 Sub-Store 的关系与上游跟随机制

### 4.1 关系：**不是独立实现，是 Sub-Store 的"薄抽取 + 重新打包"**

`[上游源码]` `packages/core/src/index.js` 全文约 170 行，实质内容 = 从 vendor 目录 import 三组对象 + 自己写的一段 `loadRemoteData / parseProxyLines / produceOutput / convert` 编排逻辑。它**没有**重新实现任何 parser 或 producer。

`[实测]` 已发布 bundle 内保留了 esbuild 的源路径注释，共 **31 处** `// src/vendors/Sub-Store/...` 标记，对应 **27 个唯一的 Sub-Store 源文件**：

```text
core/proxy-utils/parsers/index.js
core/proxy-utils/parsers/peggy/{surge,loon,qx,trojan-uri}.js
core/proxy-utils/producers/{index,utils,clash,clashmeta,surge,surgemac,loon,qx,stash,shadowrocket,surfboard,sing-box,egern,uri,v2ray}.js
core/proxy-utils/preprocessors/index.js
core/proxy-utils/{ech-utils,transport-path,vmess-security,xhttp-utils}.js
utils/{index,yaml}.js
```

bundle 首行即 `// src/vendors/Sub-Store/backend/src/utils/index.js` —— **源码内联的直接证据**（这也是 `13-licenses.md` §3.3 的核心依据，我独立复核得同样的 31 / 27 与同样的 sha256）。

### 4.2 适配层还做了两处"打桩"，会改变上游行为

`[上游源码]` `packages/core/esbuild.config.js` 配置了两个 alias：

```js
alias: { '@/core/app': './src/core/app' }        // 指向 src/core/app/index.js
alias({ 'ip-address': path.resolve(__dirname, 'src/pkg/ip-address/index.js') })
```

而 `packages/core/src/core/app/index.js` 全文是：

```js
export default console;
```

→ Sub-Store 的全局 `$`（日志、`$.utils`、`$.env`、`$.read` 等）被替换成 `console`；`ip-address` 被替换成 `export {}`（**空实现**）。

`[上游源码]` grep 结果表明，parsers/producers 里实际用到的 `$.` 只有 `$.error`（15）、`$.warn`（6）、`$.info`（5）、`$.log`（2），以及 peggy 语法里的局部 `$.ip` / `$.headers` / `$.username`（这些是 peggy 的 action 上下文，不是 Sub-Store 全局），**当前未触发 `ip-address` 缺失**（grep 计数为 0）。但这意味着：**上游一旦在 parsers/producers 中新增对 `$.utils` / `$.env` / `ip-address` 的依赖，这里会静默失效或抛错，且没有任何测试/CI 能发现**（仓库内没有测试：全仓库无 `*.spec.js` / test 目录 / CI 测试工作流，唯一 workflow 是 `docker.yaml` 构建推镜像）。

### 4.3 跟随上游的机制：git submodule + 维护者手工跑脚本

`[上游源码]` `.gitmodules`：

```ini
[submodule "packages/core/src/vendors/Sub-Store"]
	path = packages/core/src/vendors/Sub-Store
	url = https://github.com/sub-store-org/Sub-Store.git
```

`[实测]` 当前 pin：commit `744941cdce150258b0e5cb5deba5cacc6c2f02a8`（2026-08-10 20:10:10 +0800，"feat: Egern tfo 字段格式规范化"），其 `backend/package.json` 的 `version` = **2.36.33**（与 npm 包版本一一对应）。

`[上游源码]` 更新机制 = 手工执行 `TAG=<version> pnpm run update:vendor` → `packages/core/scripts/update.sh`，脚本做 4 件事：

1. `TAG` 未给时，调 GitHub API `releases/latest` 取最新 tag；
2. `git -C $VENDOR fetch --tags && git -C $VENDOR checkout tags/$TAG`，然后 `git add` submodule 指针；
3. 把所有 workspace 包的 `version` 同步成该 TAG（`npm pkg set version=$TAG`）；
4. 按 vendor `backend/package.json` 的依赖版本同步 core 的依赖版本，最后 `pnpm build:core`。

→ **不是 CI 自动同步，不是自动合并上游 commit，是"人看到上游发版 → 本地跑脚本 → push → npm publish"**。仓库唯一 workflow（`.github/workflows/docker.yaml`）只在 push 到 master 时构建推送 Docker 镜像，**没有** vendor 同步或测试。

`[实测]` 该机制的实际产出节奏（vendor 同步类 commit）：

```text
2026-05-25  chore: update Sub-Store vendor and bump version to 2.23.22
2026-06-06  chore: update Sub-Store vendor and bump version to 2.24.7
2026-06-17  chore: update Sub-Store vendor
2026-07-02  chore: update Sub-Store vendor and bump version to 2.33.1
2026-07-18  chore: update Sub-Store vendor and sync package versions to 2.36.10
2026-07-29  chore: Bump version to 2.36.25 and update Sub-Store submodule
2026-08-10  chore: Bump version to 2.36.33 and update Sub-Store submodule
```

即 **约每 1–4 周同步一次**；截至 2026-09-12，pin 在 2.36.33，上游已 2.39.6 → **滞后约 1 个月 / 3 个 minor / 上游已发 30 个 Release**。

**对 Agent 的含义**：`[推测]` 若把 sub-store-convert 作为订阅转换实现，其"上游版本"会长期落后于官方 Sub-Store；一旦某个订阅依赖上游新修的 parser bug 或新节点类型，转换结果会与官方行为不一致，且**不报错**（不支持的节点被静默丢弃，见 §3.4）。

---

## 5. 实测记录（成功或失败原因）

### 5.1 实验环境与产物

```text
[实测] 主机：macOS (Darwin arm64)，Node v25.2.1，npm 11.6.2
[实测] 目录：/tmp/r06-convert/（实验用临时目录，结束后清理）
[实测] 安装：npm install sub-store-convert@2.36.33（--cache 指向临时目录以规避本机 ~/.npm 权限问题）
[实测] 输入：自造假节点订阅（ss / vmess(ws+tls) / trojan / vless(ws+tls)，4 个节点），
       分别以 ① base64 订阅 ② 明文 URI 列表 ③ Clash YAML 三种形态由本地 HTTP 服务提供
[实测] 另跑通上游 HTTP 服务：把 packages/app 的 index.js/server.js 拷出、把
       `@sub-store-convert/core` 改成 `sub-store-convert` 后以 ESM 启动
```

### 5.2 转换结果

| 输入形态 | target | 结果 | 备注 |
| --- | --- | --- | --- |
| base64 订阅（4 节点） | `mihomo` | ✅ **成功**，822 bytes，4 个节点 | 冷 10.7 ms，热 0.6 ms |
| base64 订阅 | `surge` | ✅ 成功，350 bytes | |
| base64 订阅 | `sing-box` / `loon` / `qx` / `json` / `stash` / `surfboard` / `shadowrocket` / `egern` / `v2ray` / `uri` / `SurgeMac` / `clashmeta` | ✅ 全部返回成功（无异常） | 未逐字符校验语义等价性 |
| Clash YAML（2 节点） | `mihomo` | ✅ 成功，290 bytes | 4.0 ms |
| Clash YAML | `surge` | ✅ 成功，200 bytes | |
| **明文 URI 列表（4 节点）** | `mihomo` | ❌ **空输出**：`"proxies:\n"`（9 bytes，0 节点） | 见下 |
| 明文 URI 列表 | `surge` | ❌ 空输出（0 bytes） | |
| 未知 target | — | ❌ 抛错 `Unknown target: nonsense-target` | 预期行为 |

**明文订阅失败归因（`[实测]`，已定位到行）**：

```text
preprocessor 匹配结果：HTML=false, Clash=false, Base64=false, SSD=false, FullConfig=false, FallbackBase64=false
loadRemoteData(http://…/plain) -> []          ← packages/core/src/index.js:46 `return []`
parseProxyLines(同样这 4 行, {}) -> 4 proxies  ← 直接调 parser 层，正常解析
produceOutput(mihomo, 上述 4 proxies) -> 801 bytes
```

即 parser/producer 完全有能力处理明文 URI 列表，但 `loadRemoteData()` 在"没有任何 preprocessor 命中"时直接 `return []`，**缺少 Sub-Store 的 `return raw` 兜底**（`[上游源码]` 对照 `core/proxy-utils/index.js:50-62`）。这是与上游行为的**实质性回归**。

### 5.3 失败面（最重要的一组实测）

| 场景 | 实测结果 | 影响 |
| --- | --- | --- |
| 远端返回 **404** | `loadRemoteData()` 返回 `["not found"]`（**把错误页 body 当订阅内容**）；`convert()` 输出 `"proxies:\n"`（9 bytes） | 无 `res.ok` 校验；若错误页恰好含 `proxies:` 字样，可能被 Clash preprocessor 解析 |
| 远端**连接失败**（端口不可达） | 异常被 `catch` → `console.error('Failed to load remote data:', …)` → `return []`；`convert()` **resolve 成功**，输出 `"proxies:\n"`（9 bytes） | **调用方无法区分"订阅为空"与"网络失败"**；这是本项目"失败必须显式、绝不能覆盖当前可用配置"不变量的直接威胁 |
| 目标格式不支持某节点 | `produceOutput()` 内 `try/catch` 静默 `skip` | 节点静默丢失，不影响成功状态 |
| 未知 target | `throw new Error('Unknown target: X')` | ✅ 唯一的显式失败路径 |

`[实测]` 关键源码位置（`packages/core/src/index.js`）：

```text
:29  export async function loadRemoteData(url)
:31  const response = await fetch(url)      ← 无 response.ok 检查
:43  console.error('Preprocessor error:', error)
:46  return []                              ← 无 preprocessor 命中 → 空数组（上游是 return raw）
:48  console.error('Failed to load remote data:', error)
:49  return []                              ← 网络异常被吞
:151 export async function convert(url, target, opts = {})
:158 const urls = url.split('|')
```

### 5.4 未完成/失败的实验项

| 项目 | 状态 | 原因 |
| --- | --- | --- |
| Docker 镜像 `ghcr.io/tbxark/sub-store-convert:latest`（体积、内存、容器内行为） | `[未验证]` | `[实测]` 本机 Docker（OrbStack，Server 29.4.0）daemon 可用，但 `docker pull node:22-alpine` 在 15 s 后失败：`Get "https://registry-1.docker.io/v2/": context deadline exceeded` → **Docker Hub 不可达**，无法拉取 Dockerfile 依赖的 `node:22-alpine` / `oven/bun:1-alpine`，容器实验放弃（与 `05-sub-store-deployment.md` §3 记录一致） |
| Cloudflare Worker 部署 | `[未验证]` | 需 wrangler + CF 账号，本任务范围外；源码上只是同一 Hono `app.fetch`，`[推测]` 行为与本地一致 |
| Bun 运行时下的行为/内存 | `[未验证]` | 本机无 Bun；本次全部在 Node 25 下测试 |
| 大规模订阅（数千节点）性能/内存 | `[未验证]` | 本次只用 4 节点自造输入 |
| 与上游 Sub-Store 同输入的**逐字节输出一致性** | `[未验证]` | 需同时跑通 Sub-Store 后端做对照（R05 已有 Sub-Store 直跑环境，可作为后续对照实验） |
| 明文订阅失败是否影响真实世界用例 | `[推测]` | 真实订阅多为 base64 或 Clash YAML，明文 URI 列表较少见；但"404 当内容 / 网络失败静默成功"影响所有形态 |

---

## 6. 三方案对比表

> Sub-Store 列的数字来自本次同期抓取（§2.6）与 `docs/research/05-sub-store-deployment.md` 的实测；`NativeConverter` 列是目标设计（`[推测]`/设计意图，尚未实现）。

| 维度 | Sub-Store（官方，外部服务） | sub-store-convert | NativeConverter（本项目自研，目标态） |
| --- | --- | --- | --- |
| **部署成本** | `[实测]`(R05) 官方 Release `sub-store.bundle.js` 3.0 MiB 单文件、零 npm 依赖、Node ≥22 直跑；或 Docker 镜像（镜像体积 `[未验证]`）。需数据目录（不存在则启动崩溃）、需监听地址纠正 | `[实测]` npm 一行安装 5.1 s / 9 包 / 7.6 MB；HTTP 服务未发布到 npm，需 clone 源码 + pnpm + esbuild 构建（`pnpm run build:core` + `pnpm run start`）；无 DB、无数据目录 | `[推测]` 随 Agent 二进制分发，零额外进程、零运行时；开发成本转移到我方 |
| **资源占用** | `[实测]`(R05) 冷启动 130 ms，空闲内存 60 MB；数据目录纯 JSON | `[实测]` 导入 50 ms、服务 200 ms 可服务、转换 0.6–11 ms；`node_modules` 7.6 MB；空闲内存 `[未验证]`（`[推测]` 与 Sub-Store 同量级，Node 常驻 40–80 MB） | `[推测]` 与 Agent 同进程，增量内存可忽略 |
| **能力覆盖** | 全量：13 类 target + operator + script filter + process + mergeSources + 订阅/组合/file/artifact 管理 + 完整配置产出 + 缓存/Token | **严格子集**：13 类 target（同源 producer）+ 只读 parsers/preprocessors；**无** operator/script/process/**mergeSources**/**完整配置产出**（只有 `proxies:` 片段） | MVP 目标：URI/Clash YAML → Mihomo 配置的**最小可用子集**（明确不做全量 Sub-Store） |
| **可替换性** | 高：HTTP `/download/sub` 契约稳定、文档完备；adapter 只需一个 `base_url` + 认证 | 中偏低：接口极简（`/sub`）好包，但语义/参数与 Sub-Store 不同 → **必须独立 adapter**；无认证、默认 `0.0.0.0` → 只能本机使用 | 最高：进程内 Port 实现，无外部依赖 |
| **许可证影响** | AGPL-3.0（`LICENSE` 原文；`backend/package.json` 误写 GPL-3.0）。**外部服务方式使用不构成分发**，但不得 vendor/修改后对外提供网络服务 | `[实测]` npm 产物**自称 MIT，实际内联 27 个 AGPL 源文件且不含任何许可声明** → 与 `13-licenses.md` 的 Q1 同源风险：**不得 Bundled / 不得作为分发物**（deb、镜像、子模块都不行）；作为用户自建的外部服务运行则在许可上可行但需澄清 | 自主版权，无外部传染（须避免"逐行翻译"AGPL 实现，见 `13-licenses.md` §7.3） |
| **跟随上游速度** | 上游本体：`[实测]` 32 天 30 个 Release（≈0.94/天），30 位贡献者 | `[实测]` 单人手工同步：约每 1–4 周一次，**当前落后上游 3 个 minor / 约 1 个月**（2.36.33 vs 2.39.6）；无测试、无 CI 校验同步正确性 | 不依赖上游节奏；自主决定支持范围 |
| **失败面** | 有 Token/错误语义/`ignoreFailedRemoteSub` 等显式控制；数据目录缺失会**显式崩溃**（fail loud） | ❌ **最弱**：404 无状态码校验、网络失败静默返回空、不支持节点静默丢弃、无 preprocessor 命中时静默返回空 → **"成功的空配置"是最危险的一类失败** | 由我方定义：类型化错误、非空校验、可回滚（符合 AGENTS.md 失败流） |
| **社区/支持** | `[实测]` 10,451 stars / 1,359 forks / 30 contributors | `[实测]` **2 stars / 1 fork / 2 contributors（1 人 + bot）/ 0 issue（历史从未有 issue）/ 0 Release / 0 tag** / npm 月下载 121、周下载 8 / 2025-03~2025-09 静默 8 个月 | 内部维护 |

---

## 7. 判定与启用条件

### 7.1 判定：**Rejected**

**不作为 MVP 的 Primary / Secondary / Optional 转换实现（不写 `SubStoreConvertAdapter`，不进入 Adapter 集合）。**

### 7.2 理由（按权重排序）

1. **能力是严格子集，无法承担"可替换"角色。** `[实测]` 它与 Sub-Store 共用同一份 producer 代码，却没有 operator / script / process / mergeSources / 完整配置产出。这意味着"从 Sub-Store 切到 sub-store-convert"**不是等价替换**：依赖 Sub-Store operator/script 的订阅会产出**不同且不报错**的结果。作为 Secondary/Fallback 时，这种"静默降级"比直接失败更危险。
2. **失败面违反本项目的核心不变量。** `[实测]` 网络失败 → `convert()` 成功返回 `"proxies:\n"`；404 → 错误 body 当内容。若 Agent 直接采用，会把**空配置**当成合法转换结果，进而可能 activate 一个没有任何代理的配置，破坏 "failed update must never destroy the currently active config"。
3. **许可证不清（阻塞级）。** `[实测]` 分发产物自称 MIT 却内联 27 个 AGPL-3.0 源文件、零许可声明。`13-licenses.md` 已将"能否使用"列为阻塞级 Q1。在澄清前，它既不能进 deb/镜像/依赖，也不能作为我方源码基础。
4. **bus factor = 1，社区为零，且有过 8 个月静默。** `[实测]` 2 stars、1 fork、2 contributors、**0 issue（从未有）**、0 Release、0 tag、27 个 PR 全部来自 dependabot、npm 周下载 8。任何对它的依赖都是对单个人的依赖。
5. **"更轻量"这个唯一优势不成立。** 官方 Sub-Store 自身的 Release bundle 已经是 3.0 MiB / 零依赖 / 130 ms 冷启动（R05 实测），比 sub-store-convert 的 npm 路径（7.6 MB `node_modules`）更轻，且许可清晰、社区活跃。
6. **跟随上游落后且无自动化。** `[实测]` 手工 submodule 同步、落后 1 个月、无测试、无 CI 校验；而"上游改动导致打桩 alias（`ip-address`、`@/core/app`）失效"的风险无法被自动发现。
7. **API 与 Sub-Store 不兼容**（`/sub` vs `/download/sub`，`url` 语义不同）→ 引入它也**不能**复用 `SubStoreConverter` 的任何 URL 构造/参数映射逻辑，adapter 成本是新增一整套，而不是替换一个 `base_url`。

### 7.3 重新评估门槛（若未来证据变化）

只有当**以下全部条件**成立时，才值得重新提案（并需要 ADR）：

1. 上游维护者/作者就 §8 的许可问题给出书面澄清，且结论允许我们以**不修改、不内联、单独进程**的方式使用；
2. 维护者补齐"显式失败"语义（HTTP 状态码校验 + fetch 失败向上抛错），或我们接受"只作为外部服务 + 我方 adapter 强制非空/可解析校验"的额外责任；
3. 项目出现第二个长期维护者，或至少出现非 dependabot 的外部 PR 被合并（当前 0）；
4. 跟随上游的滞后收敛到 ≤ 1 个 minor，并加入自动化（CI 定时同步 + 冒烟测试）；
5. 出现**明确的、Sub-Store 无法覆盖的部署场景**（例如"完全无法运行 Node 但可以运行 Bun 的 32 MB 级 LXC"）——注意该场景目前由 `NativeConverter` 覆盖，需先证明 Native 不足。

在门槛满足前，**不需要为它预留任何代码结构**；`SubscriptionConverter` Port 的存在已经足够保证未来可插入。

---

## 8. 许可证影响

> 本节与 `docs/research/13-licenses.md` §3.3 / §7.3 结论一致（我独立复核了其关键数字）。

| 对象 | 声明 | 实际 | 证据 |
| --- | --- | --- | --- |
| `tbxark/sub-store-convert` 仓库 | MIT | 仓库根 `LICENSE` 确为 MIT 全文，`Copyright (c) 2024 tbxark` | `[上游源码]` `raw.githubusercontent.com/.../LICENSE`（1063 B，2026-09-12 抓取） |
| npm 产物 `sub-store-convert@2.36.33` | `package.json`: `"license": "MIT"` | 包内**只有** `index.js`（419,813 B）+ `package.json`，**无 LICENSE、无 NOTICE、无 AGPL/版权声明**；`index.js` 内联 27 个 Sub-Store 源文件（31 处路径标记） | `[实测]` tarball 解包 + grep；sha256 `415aa639165966013752de90eb03b2130023df2cd066280b8710cdd1eac73663` |
| 被内联的 Sub-Store | repo `LICENSE` = **AGPL-3.0** 全文；`backend/package.json` 写 `"license": "GPL-3.0"`（不自洽） | 属 AGPL-3.0 作品 | `[上游源码]` vendored `packages/core/src/vendors/Sub-Store/{LICENSE,backend/package.json}` |

**关键结论**：

1. `[推测]` "MIT 声明 + AGPL 内联 + 无声明文件"三者并存的合法解释只有两种：作者认为 bundle 不是 derivative work（法律上极难成立，AGPL 明确覆盖 modified/derived 的 object code 再分发），或纯粹疏忽。**无论哪种，风险在我方。**
2. **不得 Bundled / 不得 Source Reuse / 不得作为分发物**：不塞进 deb、不进容器镜像、不 vendor、不做"逐行翻译的等价实现"。这与 `13-licenses.md` 的强制约束（§7.3 第 4/5 条）一致。
3. 若未来仅作为**用户自行部署的独立外部 HTTP 服务**调用（我们不分发、不修改），则不构成我方分发；但 AGPL §13 的"网络交互提供源码"义务落在**部署者**（用户）身上，**我们必须在文档中明确告知用户这一义务**——这也是把它排除在 MVP 默认路径之外的又一理由。
4. 对比：官方 Sub-Store 同样是 AGPL-3.0，但**许可声明完整、来源清晰、可追溯到上游仓库**，风险可管理 → 本项目 R05 的"用户自建外部服务"路径在许可上明显优于 sub-store-convert。

---

## 9. 对 Agent 架构的影响（Converter Adapter 规划）

1. **MVP 不新增 `SubStoreConvertAdapter`。** `SubscriptionConverter` Port（AGENTS.md 已定义）保持不变；MVP 的实现集合为：
   ```text
   SubscriptionConverter
   ├── SubStoreConverter        // 官方 Sub-Store 外部服务（可选，用户自建；见 R05）
   └── NativeConverter          // 纯 Rust 最小实现（兜底，无外部依赖）
   ```
   （`SubStoreConvertAdapter` 仅作为"未来可插入"的设计余量存在，现在不写代码。）
2. **Adapter 的硬约束（即使未来实现，也必须遵守）**：
   - 端口/路径/参数构造**完全封装在 adapter 内**，Application 不得出现 `/sub`、`target=`、`|` 分隔符等字样（AGENTS.md "Integration rule"）。
   - **不得与 `SubStoreConverter` 共用任何 URL 构造或参数映射代码**（§3.7 证明两者参数面不兼容）。
   - **强制输出校验**：转换结果必须解析成功、`proxies` 非空、节点数 ≥ 1，否则视为 `InfrastructureError`，**绝不返回"空但成功"**（针对 §5.3 的实测缺陷）。
   - **不信任 HTTP 状态**：必须自己检查 `2xx`、内容类型与长度；把 `500`/空 body/HTML 当失败（针对 §5.3 的 404 缺陷）。
   - 若采用 HTTP 形态，**只允许连 `127.0.0.1`**（上游默认 `hostname: '0.0.0.0'` 且无认证，见 §3.6），并在 `doctor` 中检查用户是否把它暴露到公网。
   - 禁止把其 `index.js` 打进 Agent 二进制或分发包（§8）。
3. **配置兼容性提示**：若未来支持它，必须在 UI/CLI 明确提示"不支持 Sub-Store operator/script/mergeSources，结果可能与 Sub-Store 不同"——否则用户会以为两者等价。
4. **对 `ConvertRequest` 的启示**：本次调研再次确认 Port 需要能表达"来源类型"与"能力声明"（例如转换器是否能产出完整配置、是否支持 operator），以便 Application 在缺少能力时**提前拒绝**而不是产出错误配置。建议在 `ConvertRequest`/`ConvertedSubscription` 中保留 `capabilities` 或 `converter_id` 字段（`[推测]`，具体设计属 ADR 范围）。

---

## 10. 证据与来源

### 10.1 抓取/实测清单（全部为 2026-09-12 本次执行）

| # | 内容 | 方式 | 证据等级 |
| --- | --- | --- | --- |
| 1 | 仓库元数据（created/pushed/updated/stars/forks/issues/archived/license/branch） | `https://api.github.com/repos/tbxark/sub-store-convert`（12:34 抓取成功） | `[上游元数据]` |
| 2 | 仓库元数据复核（stars/watchers/forks/defaultBranch） | `https://ungh.cc/repos/tbxark/sub-store-convert` | `[上游元数据]` |
| 3 | GitHub Releases = `[]` | `https://ungh.cc/repos/tbxark/sub-store-convert/releases` | `[上游元数据]` |
| 4 | contributors = 2（tbxark 48 / dependabot 22） | `https://ungh.cc/repos/tbxark/sub-store-convert/contributors` | `[上游元数据]` |
| 5 | issue / PR 计数（0 / 0 / 27 / 23） | `https://api.github.com/search/issues?q=repo:tbxark/sub-store-convert+…` | `[上游元数据]` |
| 6 | 全量 git 历史（70 commits、作者、月分布、0 tag、0 merge） | `git clone https://ghfast.top/https://github.com/tbxark/sub-store-convert.git` | `[实测]` |
| 7 | README / LICENSE / package.json 原文 | `https://raw.githubusercontent.com/tbxark/sub-store-convert/master/…` | `[上游源码]`/`[上游文档]` |
| 8 | Sub-Store 子模块 pin 与内容 | `git submodule update --init`（经 ghfast 代理） | `[实测]` + `[上游源码]` |
| 9 | npm 元数据（23 版本、发布时间、deps、dist-tags） | `https://registry.npmjs.org/sub-store-convert` | `[上游元数据]` |
| 10 | npm 产物内容（仅 2 文件、419,813 B、31/27 处 vendor 标记、导出符号、关键字计数） | 下载 tarball 解包 + grep | `[实测]` |
| 11 | npm 下载量（月 121 / 周 8） | `https://api.npmjs.org/downloads/point/{last-month,last-week}/sub-store-convert` | `[上游元数据]` |
| 12 | `@sub-store-convert/{core,cli,app}` 未发布（404） | registry 查询 | `[实测]` |
| 13 | Sub-Store 对照数据（10,451 stars / 1,359 forks / 30 contributors / 30 releases 32 天 / 2.39.6） | `https://ungh.cc/repos/sub-store-org/Sub-Store{,/releases,/contributors}` | `[上游元数据]` |
| 14 | 安装体积/时间、导入与服务启动、转换耗时 | 本地 `npm install` + `/usr/bin/time` + `performance.now()` | `[实测]` |
| 15 | 三形态输入（base64/明文/Clash YAML）→ 多 target 转换结果 | 本地 Node 脚本 + 本地 HTTP 订阅服务 | `[实测]` |
| 16 | 失败面（404、连接失败、不支持节点、未知 target） | 本地 Node 脚本 | `[实测]` |
| 17 | HTTP API 行为（`/sub` 200/400/500、`/` 302） | 本地启动上游 `packages/app` 并 `curl` | `[实测]` |
| 18 | Docker 路径不可用 | `docker pull node:22-alpine` → `registry-1.docker.io … context deadline exceeded` | `[实测]`（负面） |

### 10.2 链接

- 仓库：<https://github.com/tbxark/sub-store-convert>
- 该仓库 LICENSE：<https://raw.githubusercontent.com/tbxark/sub-store-convert/master/LICENSE>
- 该仓库 README：<https://raw.githubusercontent.com/tbxark/sub-store-convert/master/README.md>
- 上游跟新脚本：<https://raw.githubusercontent.com/tbxark/sub-store-convert/master/packages/core/scripts/update.sh>
- 核心抽取逻辑：<https://raw.githubusercontent.com/tbxark/sub-store-convert/master/packages/core/src/index.js>
- HTTP 服务：<https://raw.githubusercontent.com/tbxark/sub-store-convert/master/packages/app/src/index.js>
- 子模块：<https://github.com/sub-store-org/Sub-Store>（pin `744941cdce150258b0e5cb5deba5cacc6c2f02a8`，v2.36.33）
- npm：<https://www.npmjs.com/package/sub-store-convert>（v2.36.33，**无 repository 字段**）
- Sub-Store 对照：<https://github.com/sub-store-org/Sub-Store>

---

## 11. 未验证假设与开放问题

### 11.1 未验证假设

| # | 假设 | 状态 | 备注 |
| --- | --- | --- | --- |
| A1 | Docker 镜像的构建可用性、体积、容器内内存与默认监听 | `[未验证]` | Docker Hub 不可达（§5.4）；Dockerfile 明示运行时是 `oven/bun:1-alpine`、`EXPOSE 3000` |
| A2 | Bun 运行时下的行为与内存 | `[未验证]` | 本机无 Bun；全部测试在 Node 25 |
| A3 | 空闲内存占用 | `[未验证]` | 未测 RSS；`[推测]` Node 常驻 40–80 MB，与 Sub-Store 同量级 |
| A4 | 与官方 Sub-Store 在**同一输入**下的输出是否逐字节一致 | `[未验证]` | 需并行跑 Sub-Store 后端做对照（R05 已有直跑环境，可作为后续实验） |
| A5 | 大规模订阅（数千节点）的性能与兼容性 | `[未验证]` | 本次仅 4 节点 |
| A6 | 明文订阅失败的实际影响面 | `[推测]` | 真实订阅以 base64 / Clash YAML 为主，明文较少见；但失败面 §5.3 与形态无关 |
| A7 | npm 包 `2024-09-23` 首版早于 GitHub 仓库 `2024-11-15` 的原因 | `[推测]` | 可能曾用其他仓库/来源，或先发包后建仓 |
| A8 | `sub-store-convert` 与 `Sub-Store` 官方是否有协作/授权关系 | `[未验证]` | 从 README 与代码看是**非官方**第三方抽取（README 明确说 "extracted from Sub-Store"，未提及官方授权） |
| A9 | 上游作者对打包 AGPL 代码的态度 | `[未验证]` | 见开放问题 Q1 |
| A10 | Node 22 是否为硬下限 | `[推测]` | `package.json` 无 `engines`；README 只说 "recommended"；实现在 Node 25 通过，未回归测 Node 22/20 |

### 11.2 开放问题

| # | 问题 | 需要谁回答 | 影响 |
| --- | --- | --- | --- |
| Q1 | npm `sub-store-convert` 的 bundle 内联 AGPL-3.0 源码却只声明 MIT，作为外部依赖/分发物是否合法？若合法需履行哪些义务？（与 `13-licenses.md` Q1 同源） | 上游作者 + 法务 | 决定它能否出现在任何分发物中（当前判定：不能） |
| Q2 | 是否需要为"仅调用其 HTTP 服务"的降级路径预留 adapter 接口骨架（零实现）？ | 架构决策 | 影响 `SubscriptionConverter` 周边是否有空壳代码；本文建议**不预留**，Port 本身已足够 |
| Q3 | 若用户自行部署了 sub-store-convert，Agent 是否应当在 `doctor` 中**警告**其默认 `0.0.0.0` 监听与无认证？ | 产品/安全 | 与 R05 对 Sub-Store 的同类告警一致，`[推测]` 应一致处理 |
| Q4 | 明文 URI 列表的回归是否已在上游最新版（≥2.37）修复？ | 上游 | 本调研 pin 在 2.36.33；若已修复，需重测（但不足以改变总体判定） |
| Q5 | 是否值得做一个"sub-store-convert 作为 Sub-Store 的等价替代"的**行为差异对照实验**（同输入、同 target、逐字节 diff）？ | 我方 | 只有在 Q1 澄清、且出现真实部署诉求时才有价值 |
