# R13 — License Matrix

> 状态：已完成初稿 | 调研日期：2026-09-12 | 证据等级：LICENSE 原文核实
> 声明：本文是合规信息收集，不是法律意见。
> 抓取时间：2026-09-12T04:35Z ~ 04:45Z（UTC）。所有 SPDX 标识符均来自上游 LICENSE 文件原文、上游 README/`package.json` 声明或官方 registry 元数据，证据分级见各条标注。
> 证据分级说明：`[上游 LICENSE 原文]` = 直接抓取 LICENSE 文件并核对正文；`[上游文档声明]` = 上游 README / `package.json` / registry 元数据声明；`[推测]` = 基于上述事实的工程推断；`[法律问题-待确认]` = 需要法务或上游维护者确认；`[未验证]` = 本次未取得直接证据。

---

## 1. 结论摘要（TL;DR）

1. **Mihomo（代理内核）是 GPL-3.0，不是 MIT。** 内核代码位于 `MetaCubeX/mihomo` 仓库的 `Meta` 分支与所有 release tag（已核实 `v1.19.30`，2026-08-16 发布），LICENSE 为标准 GPL-3.0 全文（35149 bytes，sha256 `3972dc97…b36986`），README 明确写 "This software is released under the GPL-3.0 license."。**但该仓库的默认分支 `main` 当前放的是一份与本项目无关的 Python Pydantic 库（MIT，1049 bytes，sha256 `2278f74a…6b75a3`），因此 GitHub 的仓库级 license 元数据（以及所有只读默认分支的自动扫描工具）会把它误报为 MIT。** 任何依赖该元数据的合规判断都是错的，必须按 tag/`Meta` 分支核验。`[上游 LICENSE 原文]` + `[上游文档声明]`
2. **Sub-Store 是 AGPL-3.0。** 仓库 `LICENSE` 为 AGPL-3.0 全文（34577 bytes，sha256 `08e3bf9a…e4da90`），`backend/package.json` 声明 `"license": "AGPL-3.0"`（v2.39.6），README 第 166–168 行 "This project is under the AGPL-3.0 LICENSE."。**保持"独立进程/容器 + 仅 HTTP 调用 + 不修改其源码"时，AGPL-3.0 §13（Remote Network Interaction）不触发，我们的 Rust 代码不会被视为衍生作品**；一旦把源码并入本仓库、把构建产物打进 deb/镜像、或直接分发其代码，则触发 AGPL 的源码提供与同许可义务。`[上游 LICENSE 原文]` + `[上游文档声明]`
3. **`sub-store-convert`（npm）标称 MIT，但其发布产物内联了 Sub-Store 的 AGPL 源码。** npm 元数据与包内 `package.json` 均声明 `"license": "MIT"`（v2.36.33），但压缩包内 `index.js`（419813 bytes）含 31 处 `src/vendors/Sub-Store/...` 标记、27 个唯一源文件路径（`backend/src/core/proxy-utils/{parsers,producers,preprocessors}`），且**全包无 LICENSE 文件、无任何 AGPL/版权声明**。这是本次调研中风险最高的一项：**不得把它 Bundled 进我们的分发物，也不得 Source Reuse**。`[上游 LICENSE 原文]`（包内实际内容）`[法律问题-待确认]`
4. **metacubexd 应用代码是 MIT，但其前端依赖里有一项非开源许可证：Highcharts。** 根 `LICENSE` 与 `packages/ui/package.json` 均为 MIT（`Copyright (c) 2023 MetaCubeX`）；`packages/ui` 的 dependencies 含 `highcharts`，npm 登记 license 为 `https://www.highcharts.com/license`，包内 `LICENSE.txt` 原文写明商业使用须遵循 Highsoft Standard License Agreement。若我们内嵌其构建产物并对外分发，**必须就 Highcharts 单独决策（去掉该图表库、或取得 Highsoft 授权）**。另外 UI 通过 `@nuxt/fonts` 从 Google 自托管 Ubuntu 字体（UFL-1.0），并内置 Twemoji flags 字体（Apache-2.0 + CC-BY-4.0 视觉设计），均需在 NOTICE 中署名。`[上游 LICENSE 原文]` + `[上游文档声明]` + `[法律问题-待确认]`
5. **ShellCrash 是 GPL-3.0**（`LICENSE.txt` 35149 bytes，sha256 与 GPL-3.0 全文完全一致；README_CN 第 183–185 行声明 GPL 3.0）。本项目对 ShellCrash 仅做 **feature-level 对标与阅读学习，不复制任何代码**，因此不承担 GPL 义务。`[上游 LICENSE 原文]` + `[上游文档声明]`
6. **Mihomo README 附带一条命名限制**："any downstream projects not affiliated with `MetaCubeX` shall not contain the word `mihomo` in their names." 建议产品名/包名避免包含 `mihomo`（如使用 `proxy-agent` / `proxyctl`），文档中描述性引用 "Mihomo" 属于 nominative use。`[上游文档声明]` `[法律问题-待确认]`
7. **Rust 侧基线依赖全部是宽松许可**（tokio/axum/ratatui 等 MIT；serde/sqlx/clap 等 MIT OR Apache-2.0，逐项见 §5.2）。建议：自研代码 **MIT OR Apache-2.0 双许可**；用 `cargo deny` 做 allow-list（默认拒绝一切未显式允许的 license），**禁止任何 GPL/AGPL/LGPL/SSPL/BUSL/CC-NC 进入 Rust 二进制**。注意：自研代码选择 MIT/Apache-2.0 **不会**让我们能"带着 MIT 去链接 GPL 依赖"——一旦链接 GPL 依赖，整个二进制必须按 GPL-3.0 分发。`[上游文档声明]` + `[推测]`

---

## 2. License Matrix（组件 × 许可证 × 使用方式 × 义务）

### 2.1 组件总表

| # | 组件 | 上游 | 已核实 SPDX | 证据强度 | 本项目建议使用方式 | 触发义务 | 风险 |
|---|------|------|-------------|----------|--------------------|----------|------|
| 1 | **Mihomo**（代理内核） | `MetaCubeX/mihomo` @ `Meta` 分支 / tag `v1.19.30` | `GPL-3.0-only`（保守解释；上游文字为 "GPL-3.0"） | `[上游 LICENSE 原文]` 35149B sha `3972dc97…` | **Dynamic Dependency**（运行时下载，默认）或 **Bundled**（deb 内置，可选） | Bundled：Corresponding Source + license 全文 + 保留版权声明 + 修改声明（无修改则声明无修改）+ 无附加限制；Dynamic：义务落在最终用户侧 | 中（可管理） |
| 2 | **Sub-Store** | `sub-store-org/Sub-Store` @ `master`（backend v2.39.6） | `AGPL-3.0-only` | `[上游 LICENSE 原文]` 34577B sha `08e3bf9a…` + `package.json` `"license": "AGPL-3.0"` | **External Service**（独立进程/容器，仅 HTTP API） | 不分发则无 conveying 义务；若 Bundled 则需 Corresponding Source + AGPL 全文；**修改后对外提供网络交互才触发 §13** | 中（靠进程边界隔离） |
| 3 | **sub-store-convert** | npm `sub-store-convert` v2.36.33 | 标称 `MIT`，实际内联 AGPL-3.0 代码 `[法律问题-待确认]` | `[上游 LICENSE 原文]`（tarball 内 `index.js` 内容） | **不使用 / 不作为分发物**（若必须用，仅作 External Service 且需上游澄清） | 若按 AGPL 认定：Corresponding Source + AGPL 全文 + 修改声明；按 MIT 认定则仅需保留声明 | **高** |
| 4 | **metacubexd**（Dashboard） | `MetaCubeX/metacubexd` @ `main`（monorepo v1.273.1） | 应用 `MIT`；含 `Highcharts`（proprietary）、`UFL-1.0` 字体、`CC-BY-4.0` 字体图形 | `[上游 LICENSE 原文]` 1096B sha `cd0735ba…` + npm/registry 元数据 | **Bundled**（内嵌静态构建产物）或 External Service | MIT：保留版权与许可声明（UI 内 + 发行物 NOTICE）；Highcharts：商业许可决策；字体：署名 | **高**（Highcharts） |
| 5 | **ShellCrash** | `juewuy/ShellCrash` @ `dev` | `GPL-3.0-only` | `[上游 LICENSE 原文]` 35149B sha `3972dc97…`（与 GPL-3.0 全文一致） | **Source Reuse：不使用**。仅 feature-level 对标 | 无（不复制代码即无义务） | 低 |
| 6 | **Rust crates** | crates.io | 见 §5.2（MIT / MIT OR Apache-2.0 等，全部宽松） | `[上游文档声明]`（crates.io registry 元数据） | **Bundled**（静态链接进二进制） | 保留 license 全文 + NOTICE（MIT/Apache-2.0 要求）；Apache-2.0 含专利授权与 NOTICE 传递 | 低 |
| 7 | **前端依赖** | npm（若自研 UI） | 待定，按 allow-list 治理 | `[推测]` | Bundled（打进静态资源） | 按各依赖要求保留声明；禁止 copyleft/非商业许可 | 中 |

### 2.2 逐组件 × 使用方式义务矩阵

图例：`✅ 允许` / `⚠️ 有条件` / `❌ 不建议` / `—` 不适用

| 组件 | Bundled（打包进 deb/镜像） | External Service（独立进程，仅 API） | Dynamic Dependency（运行时下载） | Source Reuse（复制源码） |
|------|---------------------------|--------------------------------------|----------------------------------|--------------------------|
| Mihomo (GPL-3.0) | ⚠️ 需完整履行 GPL-3.0 §4/§5/§6：附 license 全文、保留声明、提供 Corresponding Source（或 §6(b) 书面 offer / §6(d) 同址源码访问） | — | ✅ 推荐。由 agent 代表用户从上游获取，我们不 conveying；义务不落在我们身上 | ❌ 严禁（会把整个 Rust 二进制拖入 GPL-3.0） |
| Sub-Store (AGPL-3.0) | ⚠️ 可行但成本高：需 Corresponding Source + AGPL 全文；且必须确保它是"聚合"而非衍生 | ✅ **推荐**。不改源码 + 仅 HTTP + 进程隔离 → 不触发 §13，不传染 | ✅ 允许（同 Mihomo 逻辑）；但注意用户交互条款 | ❌ 严禁 |
| sub-store-convert (MIT 标称/AGPL 疑云) | ❌ 不建议（AGPL 传染风险） | ⚠️ 仅在上游/法务澄清后 | ⚠️ 同上 | ❌ 严禁 |
| metacubexd (MIT + Highcharts) | ⚠️ 条件性允许：MIT 部分保留声明即可；**Highcharts 必须先解决**；字体需署名 | ✅ 作为独立静态服务可降低内嵌风险 | ✅ 允许 | ⚠️ MIT 允许，但需保留版权声明；Highcharts 部分仍受限 |
| ShellCrash (GPL-3.0) | ❌ | — | — | ❌（仅作 feature 对标） |

---

## 3. 逐组件分析（含 LICENSE 原文摘录与链接）

### 3.1 Mihomo（MetaCubeX/mihomo）— GPL-3.0

**仓库**：<https://github.com/MetaCubeX/mihomo>
**核验对象**：`Meta` 分支与 tag `v1.19.30`（latest release，2026-08-16T10:11:34Z）
**抓取 URL**：<https://raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.30/LICENSE>、<https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/LICENSE>
**抓取日期**：2026-09-12（UTC）
**文件指纹**：35149 bytes，sha256 `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986`
**交叉验证**：jsDelivr `https://cdn.jsdelivr.net/gh/MetaCubeX/mihomo@v1.19.30/LICENSE` 返回同一 GPL-3.0 文本（排除单一 CDN 缓存导致的误判）。

`[上游 LICENSE 原文]` 开头：

```text
                    GNU GENERAL PUBLIC LICENSE
                       Version 3, 29 June 2007

 Copyright (C) 2007 Free Software Foundation, Inc. <https://fsf.org/>
 Everyone is permitted to copy and distribute verbatim copies
 of this license document, but changing it is not allowed.
```

`[上游文档声明]` README（`v1.19.30`，第 97–101 行）：

```markdown
## License

This software is released under the GPL-3.0 license.

**In addition, any downstream projects not affiliated with `MetaCubeX` shall not contain the word `mihomo` in their names.**
```

`[上游文档声明]` `go.mod` 首行 `module github.com/metacubex/mihomo`（确认 `v1.19.30` tag 确为代理内核源码，而非同名 Python 库）。

#### 3.1.1 ⚠️ 关键陷阱：默认分支 `main` 与内核代码不是同一份许可

| 核验对象 | 内容 | LICENSE | 指纹 |
|---|---|---|---|
| `Meta` 分支 / 所有 release tag | Mihomo 代理内核（Go，`github.com/metacubex/mihomo`） | **GPL-3.0**（35149 B） | sha256 `3972dc97…b36986` |
| 默认分支 `main`（截至 2026-09-12） | 与代理内核无关的 Python Pydantic 库（README: "A simple python pydantic model … for Honkai: Star Rail parsed data from the Mihomo API"，指向 `KT-Yeh/mihomo`） | **MIT**（1049 B，`Copyright 2023 KT`） | sha256 `2278f74ad468f0995467b5bd9df3c7bbf1bdfd57a135dac7a9d14c0e366b75a3` |

证据：`https://raw.githubusercontent.com/MetaCubeX/mihomo/main/README.md`、`.../main/LICENSE`，并经 jsDelivr `@main` 交叉验证（两家 CDN 内容一致）。因此 GitHub 的仓库级 license 徽章/API 字段（本次抓取返回 `spdx_id = MIT`）**不代表内核许可**。

**行动要求**：
- 任何自动化合规扫描必须固定到 **tag 或 `Meta` 分支**；不要读取仓库级 license 元数据。
- 版本对应关系以 tag 为准（例如 `v1.19.30`），并记录 tag → commit → 二进制 sha256。
- 该异常（默认分支内容与内核无关）本身的成因 `[法律问题-待确认]`：需向上游确认是否为有意为之，以及仓库级 license 元数据是否会被修正。

#### 3.1.2 使用方式 A — Dynamic Dependency（运行时下载，不修改源码、不静态链接）

事实与推断：
- 我们不是 GPL-3.0 意义上的 "conveyor"（不向用户分发 Mihomo 二进制），二进制由 agent 从上游或镜像获取到用户机器。`[推测]`
- 不修改源码、不静态链接、不把内核作为库链接进 Rust 进程（我们只通过进程调用 + HTTP/Unix socket controller 交互，属独立程序），因此不构成衍生作品。`[推测]`
- 结论：在此模式下，GPL-3.0 的源码提供义务**不由我们承担**；最终用户获得 GPL-3.0 授予的全部权利（可再分发、可索取源码）。`[推测]`

工程侧必须配合的动作（非许可义务，但影响合规可辩护性）：
- 记录精确版本与校验和（tag + sha256），下载源使用官方 release 或明确说明的镜像；
- 不在仓库内再分发 Mihomo 二进制；
- 不修改内核源码；若未来打补丁，必须回到 Bundled 流程处理 GPL 义务。

#### 3.1.3 使用方式 B — Bundled（打进 deb 分发）

一旦 deb 内包含 Mihomo 二进制，我们即成为 GPL-3.0 的 distributor，必须履行：

- **§4 Conveying Verbatim Copies / §5**：随发行物提供 GPL-3.0 全文与版权声明；若修改过源码，必须显著声明修改及日期。`[上游 LICENSE 原文]`
- **§6 Conveying Non-Source Forms** 提供 Corresponding Source。文献原文（LICENSE 第 245 行起）：
  - `§6(b)`：随物理产品附带 **有效期至少三年** 的书面 offer，提供 Corresponding Source（费用不超过实际分发成本），或提供网络服务器访问；
  - `§6(d)`：若通过网络地点分发 object code，可在**同一地点**以同等便利提供 Corresponding Source 的访问（"You need not require recipients to copy the Corresponding Source along with the object code… provided you maintain clear directions next to the object code saying where to find the Corresponding Source. Regardless of what server hosts the Corresponding Source, you remain obligated to ensure that it is available for as long as needed to satisfy these requirements."）。
- **§10 Automatic Licensing of Downstream Recipients**：不得对下游施加额外限制。
- **§7 Additional Terms**：允许的附加条款限于有限类别（如署名、商标）；上游 README 中的命名限制属于商标/命名要求，是否构成 GPL-3.0 §7 意义上的 "additional restriction" `[法律问题-待确认]`。
- **版本对应**：必须能对应到所分发二进制的确切源码（tag + 构建方式 + 补丁集）。本次分发若无补丁，应书面声明 "no modifications"。

**聚合与链接的区分（重要）**：deb 中同时包含 GPL-3.0 的 Mihomo 二进制与 MIT/Apache-2.0 的 Rust agent，若二者是相互独立的程序（仅通过进程/IPC/HTTP 交互），属于 mere aggregation，不要求把 agent 置于 GPL 下。反之，若通过 cgo/静态库/FFI 把内核代码链入我们的二进制，则整体将成为衍生作品，必须按 GPL-3.0 分发。`[推测]`

**Mihomo 内核中内嵌的第三方许可**（Bundled 时需一并满足）：jsDelivr 文件清单显示内核内带独立许可文件 `[上游 LICENSE 原文]`：
- `/transport/kcptun/LICENSE.md`：MIT，`Copyright (c) 2016 xtaci`；
- `/transport/hysteria/conns/faketcp/LICENSE`：`Grabbed from https://github.com/xtaci/tcpraw with modifications`（未包含标准许可正文，`[未验证]` 其完整 SPDX）。

### 3.2 Sub-Store（sub-store-org/Sub-Store）— AGPL-3.0

**仓库**：<https://github.com/sub-store-org/Sub-Store>（默认分支 `master`，`backend` 版本 2.39.6）
**抓取 URL**：<https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/LICENSE>、`.../master/backend/package.json`、`.../master/README.md`
**抓取日期**：2026-09-12（UTC）
**文件指纹**：34577 bytes，sha256 `08e3bf9a0da8b80a8ca22489937661f31d3f851cd37a27033645bb05bbe4da90`

`[上游 LICENSE 原文]` 开头（注意第 4 行存在一行非 FSF 原文的版权行，属上游 LICENSE 文件的瑕疵，不影响条款效力）：

```text
                   GNU AFFERO GENERAL PUBLIC LICENSE
                       Version 3, 19 November 2007

               Copyright (c) 2015 Ayuntamiento de Madrid

 Copyright (C) 2007 Free Software Foundation, Inc. <http://fsf.org/>
```

`[上游 LICENSE 原文]` §13（LICENSE 第 542 行起）：

```text
  13. Remote Network Interaction; Use with the GNU General Public License.

  Notwithstanding any other provision of this License, if you modify the
Program, your modified version must prominently offer all users
interacting with it remotely through a computer network (if your version
supports such interaction) an opportunity to receive the Corresponding
Source of your version by providing access to the Corresponding Source
from a network server at no charge, through some standard or customary
means of facilitating copying of software. …
```

`[上游文档声明]`：
- `backend/package.json`：`"name": "sub-store"`、`"version": "2.39.6"`、`"license": "AGPL-3.0"`；
- README 第 166–168 行：`## LICENSE` / `This project is under the AGPL-3.0 LICENSE.`

#### 3.2.1 §13 是否触发（核心问题）

§13 的条件是 **"if you modify the Program"** 且修改后的版本支持远程网络交互。因此：

| 场景 | §13 触发 | 依据 |
|------|----------|------|
| 部署**未修改**的 Sub-Store，用户通过网络与其交互 | **否**（未修改） | `[上游 LICENSE 原文]` §13 明文以 "if you modify the Program" 为前提 |
| 修改 Sub-Store 源码并对外提供网络服务 | **是**，须向交互用户提供 Corresponding Source | 同上 |
| 仅阅读/调用其 HTTP API | 否 | 不构成修改或衍生 |

#### 3.2.2 External Service 模式（推荐）的义务边界

- **进程隔离 + 仅 HTTP 调用 + 不链接 + 不复制源码** → 我们的 Rust agent 与 Sub-Store 是两个独立程序，通过公开 API 交互，不构成衍生作品，AGPL 不传染。`[推测]`
- 若我们**分发** Sub-Store（deb 依赖/内置镜像/内嵌其前端构建产物），则我们是 conveyor，须提供 Corresponding Source 与 AGPL-3.0 全文。
- **deb 依赖 vs 内嵌源码的差别**：
  - "依赖/可选组件"（用户自行 `docker run` 或从上游安装）：我们不分发其代码 → 主要义务是文档说明与版本兼容，无源码提供义务；`[推测]`
  - "内嵌源码/构建产物"：触发 AGPL 的 conveying 与同许可义务，并且会引入"我们是否已修改"的判断。
- 边界纪律（必须写进代码评审规则）：
  1. 不把 Sub-Store 任何源码/构建产物放进本仓库；
  2. adapter 内只出现公开 API URL 构造，不复制其算法实现（`[推测]`：功能等价的自研实现不受 AGPL 约束，但需避免逐行翻译）；
  3. 不内嵌 Sub-Store 的 frontend 构建产物；
  4. 不 fork 后修改再分发。

### 3.3 sub-store-convert（npm）— 标称 MIT，实际内联 AGPL 代码

**来源**：npm registry `sub-store-convert@2.36.33`（2026-08-10 发布；共 23 个版本，首版 2024-09-23）
**抓取 URL**：<https://registry.npmjs.org/sub-store-convert>、<https://registry.npmjs.org/sub-store-convert/-/sub-store-convert-2.36.33.tgz>
**抓取日期**：2026-09-12（UTC）
**文件指纹**：tarball sha256 `415aa639165966013752de90eb03b2130023df2cd066280b8710cdd1eac73663`

`[上游文档声明]` npm 元数据与包内 `package.json`：

```json
{
  "name": "sub-store-convert",
  "version": "2.36.33",
  "description": "Advanced Subscription Converter for QX, Loon, Surge, Stash and ShadowRocket.",
  "license": "MIT",
  "files": ["index.js"],
  "dependencies": { "js-base64": "^3.7.2", "json5": "^2.2.3", "lodash": "^4.17.21", "peggy": "^2.0.1", "yaml": "^2.9.0" },
  "repository": null,
  "homepage": null
}
```

`[上游 LICENSE 原文]` 对 tarball 实际内容的核查（这是判断"是否 vendor Sub-Store"的直接证据）：

- 包内只有两个文件：`index.js`（419813 bytes）与 `package.json`（447 bytes）；**无 LICENSE 文件**，`files` 仅声明 `index.js`。
- `index.js` 是 esbuild 风格的单文件 bundle，**首行即为内联代码**（无任何版权/许可 banner）：
  ```js
  // src/vendors/Sub-Store/backend/src/utils/index.js
  var IPV4_REGEX = /^((25[0-5]|(2[0-4]|1\d|[1-9]|)\d)(\.(?!$)|$)){4}$/;
  ```
- 出现 `src/vendors/Sub-Store` 前缀 **31 次**，共 **27 个唯一源文件路径**，全部指向 Sub-Store backend 的 `proxy-utils`：
  ```
  src/vendors/Sub-Store/backend/src/core/proxy-utils/parsers/index.js
  src/vendors/Sub-Store/backend/src/core/proxy-utils/parsers/peggy/{loon,qx,surge,trojan-uri}.js
  src/vendors/Sub-Store/backend/src/core/proxy-utils/preprocessors/index.js
  src/vendors/Sub-Store/backend/src/core/proxy-utils/producers/{clash,clashmeta,egern,index,loon,qx,shadowrocket,sing-box,stash,surfboard,surge,surgemac,uri,utils,v2ray}.js
  src/vendors/Sub-Store/backend/src/core/proxy-utils/{transport-path,vmess-security,xhttp-utils,ech-utils}.js
  ```
- 全包 **0 处** 匹配 `AGPL` / `GNU Affero` / `Permission is hereby granted` / `Copyright (c)` / `@license`。

结论：
- 该包把 Sub-Store（**AGPL-3.0**）的 `proxy-utils` 源码内联进产物，却以 `"license": "MIT"` 对外声明，且未附任何许可/版权声明。`[上游 LICENSE 原文]`
- 若按上游源码的实际许可认定，该 bundle 属于 AGPL-3.0 覆盖的衍生/组合作品；分发它需要 Corresponding Source 与 AGPL-3.0 全文；按 MIT 分发则与上游条款不一致。这属于**上游声明与内容的冲突**，不能由我们单方面解释。`[法律问题-待确认]`
- npm 元数据中 **没有 repository/homepage 字段**，无法定位其源码仓库以核实构建来源与许可证选择意图。`[未验证]`

**本项目处理建议**（合规优先）：
1. **不把 `sub-store-convert` 打进 deb 或镜像**，不在仓库内 vendor 其产物；
2. 若 MVP 需要"轻量转换"，优先 N-API/自研 `NativeConverter`，或复用已部署的 Sub-Store HTTP API；
3. 若确需使用，把它当作**独立部署的外部服务**并在文档中标注 AGPL 来源与版本，同时把 §8 的确认项发往上游/法务；
4. AGENTS.md 中的 `SubStoreConvertAdapter` 在合规澄清前不应作为默认路径。

### 3.4 metacubexd（MetaCubeX/metacubexd）— MIT（应用） + Highcharts（专有）+ 字体

**仓库**：<https://github.com/MetaCubeX/metacubexd>（默认分支 `main`；当前为 monorepo，`metacubexd-monorepo` v1.273.1）
**抓取 URL**：`.../main/LICENSE`、`.../main/package.json`、`.../main/packages/ui/package.json`、`.../main/pnpm-workspace.yaml`、`.../main/packages/ui/nuxt.config.ts`
**抓取日期**：2026-09-12（UTC）
**文件指纹**：`LICENSE` 1096 bytes，sha256 `cd0735ba06f26a0008bbca399890c7ca87fe129aacc302c2e33fb03e60a4e8c3`

`[上游 LICENSE 原文]`（MIT，注意其中包含一段"包括下一段"的额外表述）：

```text
MIT License Copyright (c) 2023 MetaCubeX

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without restriction, …

The above copyright notice and this permission notice (including the next paragraph) shall be included in all
copies or substantial portions of the Software.
```

`[上游文档声明]`：`packages/ui/package.json` → `"name": "@metacubexd/ui"`、`"license": "MIT"`、`"description": "Mihomo Dashboard, The Official One, XD"`。UI 技术栈为 **Nuxt 4 + Vue 3**（`nuxt`、`vue`、`pinia`、`monaco-editor` 等），根 `package.json` 为 pnpm workspace（`packages/*`、`apps/*`）。

**结构变化提示**：当前 `main` 已不是单一 SolidJS SPA，而是 `packages/ui`（Nuxt/Vue）+ `apps/server` + `apps/desktop` 的 monorepo。旧的 `src/*.tsx` 布局在 `main` 上已不存在（`src/App.tsx` → HTTP 404）。`[上游文档声明]`

#### 3.4.1 ⚠️ Highcharts：非开源许可（本次调研最高风险项）

`[上游文档声明]` npm registry `highcharts@13.0.2`：`"license": "https://www.highcharts.com/license"`，author `Highsoft AS`。
`[上游 LICENSE 原文]` 包内 `LICENSE.txt`（348 bytes，经 jsDelivr 与 unpkg 双源一致）：

```text
Commercial use of this software is governed by the Highsoft Standard License Agreement
found at: https://www.highcharts.com/license

Non-commercial use of this software is governed by the Highsoft End-User License Agreement (EULA)
found at: https://www.highcharts.com/license-eula

By installing or using this software, you agree to these terms.
```

`packages/ui/package.json` 的 `dependencies` 中明确包含 `"highcharts": "catalog:"`（catalog 解析为 `^13.0.0`）。`[上游文档声明]`

影响：
- 若我们内嵌 metacubexd 的构建产物并对外分发（deb / 镜像 / 自建 apt 源），Highcharts 会随产物一起被 conveyed，**商业使用许可问题不再是"终端用户"的问题，而是我们分发行为的问题**。
- 我们的产品是否属于 Highsoft 定义的 "Commercial use" 取决于产品性质（是否收费、是否为商业实体内部使用、是否随商业产品分发）。这必须由法务判断。`[法律问题-待确认]`
- 可执行的工程规避：构建 Dashboard 时**移除/替换 Highcharts**（上游 UI 用其绘制流量图表），或在没有 Highcharts 的情况下构建并接受功能降级；需要验证构建配置是否可通过 alias/exclude 去掉该依赖。

#### 3.4.2 字体与静态资源（内嵌产物必须署名）

`[上游文档声明]` `packages/ui/nuxt.config.ts` 第 143–152 行：

```ts
  // Fonts configuration - using Ubuntu font
  fonts: {
    families: [
      { name: 'Ubuntu', provider: 'google', weights: [300, 400, 500, 700], styles: ['normal', 'italic'] },
    ],
```

- `@nuxt/fonts`（MIT，`[上游文档声明]`）的定位是构建期拉取并自托管 webfont（README："Plug-and-play custom web font optimization and configuration for Nuxt apps"，内置 `google/bunny/fontshare/fontsource/adobe/npm/local` providers），因此**构建产物中会包含 Ubuntu 字体文件**。`[推测]`（本次未实际构建，构建产物内容 `[未验证]`）
- Ubuntu 字体许可：**UFL-1.0**（`[上游 LICENSE 原文]`，<https://raw.githubusercontent.com/google/fonts/main/ufl/ubuntu/UFL.txt>，4673 bytes，sha256 `2f0015108d68627bd788d313f529c21ff4da2c2c42a5e1f3883acc83480f9002`）：
  ```text
  -------------------------------
  UBUNTU FONT LICENCE Version 1.0
  -------------------------------

  PREAMBLE
  This licence allows the licensed fonts to be used, studied, modified and
  redistributed freely. The fonts, including any derivative works, can be
  bundled, embedded, and redistributed provided the terms of this licence
  ```
  → 允许 bundle/embed，但需满足 UFL 的署名与许可随附条件（分发时附 UFL-1.0 全文）。
- 仓库内置字体资源：`/packages/ui/assets/fonts/TwemojiMozilla-flags.woff2`（`[上游文档声明]`，来自 jsDelivr `@1.273.1` 文件清单）。
  上游 `mozilla/twemoji-colr` 的 `LICENSE.md`（2172 bytes，sha256 `64419edc28e9163204c3be73f835a8dfc34cd6c9b8f7d067bb685f119f839a99`）`[上游 LICENSE 原文]` 声明：
  ```text
  ## License for the Code
  Copyright 2016-2018, Mozilla Foundation
  Licensed under the Apache License, Version 2.0 …

  ## License for the Visual Design
  The Emoji art in the twe-svg.zip archive comes from Twemoji … and is used and redistributed
  under the CC-BY-4.0 license terms offered by the Twemoji project.
  ```
  → 代码部分 Apache-2.0，**视觉设计部分 CC-BY-4.0（要求署名 + 许可链接）**。
- 注意：metacubexd 仓库内**没有**字体层面的 NOTICE/许可副本文件（jsDelivr 清单中仅有根 `LICENSE`），因此字体许可信息必须由我们在自己的 NOTICE 中补齐。`[上游文档声明]` + `[推测]`

### 3.5 ShellCrash（juewuy/ShellCrash）— GPL-3.0（仅作参考）

**仓库**：<https://github.com/juewuy/ShellCrash>（默认分支 `dev`）
**抓取 URL**：<https://raw.githubusercontent.com/juewuy/ShellCrash/dev/LICENSE.txt>、`.../dev/README_CN.md`
**抓取日期**：2026-09-12（UTC）
**文件指纹**：`LICENSE.txt` 35149 bytes，sha256 `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986` —— 与 Mihomo 的 GPL-3.0 文本 **逐字节一致**，即标准 GPL-3.0 全文。`[上游 LICENSE 原文]`
注意：仓库根目录没有 `LICENSE`（HTTP 404），许可文件名为 `LICENSE.txt`；GitHub 仓库级元数据登记为 GPL-3.0。`[上游文档声明]`

`[上游文档声明]` README_CN 第 183–185 行：

```markdown
## :scroll: 许可协议

本项目采用[GNU通用公共许可证第3.0版](LICENSE.txt)授权。
```

**"阅读代码学习功能" 与 "复制代码" 的界线（本项目自我约束声明）**：

- **允许（不产生 GPL 义务）**：阅读其 Shell 实现以理解**功能行为与运维语义**（例如安装/升级流程、iptables/nftables 规则的意图、服务生命周期管理），并以**独立设计**方式在我们的 Rust 代码中实现同等 feature。
- **禁止（产生 GPL-3.0 义务）**：复制、逐行翻译（translation 亦属 derivative work）、粘贴其脚本片段/规则模板/注释，或把其代码纳入本仓库、测试夹具、文档示例。
- **我们的正式立场（feature-level 对标声明）**：
  > 本项目将 ShellCrash 仅作为**功能与运维行为的外部参考**，不复制、不改编、不翻译其源代码。任何从 ShellCrash 获得的知识仅停留在 feature/行为层面，并将在 Rust 中以独立实现表达；若未来需要复用其代码，必须先按 GPL-3.0 走许可与源码提供流程并更新本文与相关 ADR。
- 建议落地：在仓库中加入 `.github/CONTRIBUTING` 或评审清单条目——"任何来自 ShellCrash 的代码片段一律拒绝"；对来自该项目的设计参考，在 PR 描述中记录来源链接以示区分。

### 3.6 Rust 依赖策略

见 §5。要点：`cargo deny`（EmbarkStudios，`0.20.2`，`MIT OR Apache-2.0` `[上游文档声明]`）做 allow-list + ban，`cargo about`（`0.9.2`，`MIT OR Apache-2.0`）生成 NOTICE，`cargo license`（`0.7.0`，`MIT`）做快速巡检。

### 3.7 前端依赖

见 §6。要点：MIT/Apache-2.0/BSD/ISC/0BSD/CC0 白名单；GPL/AGPL/LGPL/SSPL/BUSL/CC-NC 与 Highcharts 类专有许可进入需审查/拒绝清单。

---

## 4. 四种使用方式的义务差异

本节是文档核心表：把"我们怎么用"映射到"我们必须做什么"。

### 4.1 义务对照表

| 义务维度 | Bundled（打包进 deb/镜像） | External Service（独立进程/容器，仅 API 调用） | Dynamic Dependency（运行时下载） | Source Reuse（复制源码进本仓库） |
|---|---|---|---|---|
| **是否构成 conveying / distribution** | 是 | 否（仅当我们也分发该服务时才转为是） | 否（由用户从上游获取） | 是（源码散布） |
| **许可证全文随附** | 必须（GPL/AGPL/MIT/Apache-2.0 均要求） | 不需要（我们未分发） | 不需要 | 必须 |
| **版权声明保留** | 必须 | 不要移除上游声明（我们本就不改上游） | 不适用 | 必须 |
| **NOTICE / THIRD-PARTY 清单** | 必须（Apache-2.0 §4(d) 与工程实践要求） | 建议（文档层面标注依赖） | 建议（记录版本来源） | 必须 |
| **Corresponding Source 提供** | GPL/AGPL 必须 | 不分发则不需要 | 不需要 | AGPL/GPL 必须 |
| **修改声明** | 有修改则必须（GPL-3.0 §5(a)、AGPL §5(a)） | 仅当修改了该服务 | 不适用 | 有修改则必须 |
| **网络交互条款（AGPL §13）** | 修改 + 网络交互才触发 | **未修改即不触发**（§13 以 "if you modify" 为前提） | 不适用 | 修改后提供网络服务则触发 |
| **传染范围** | 同一作品内合并的代码受 copyleft 约束；**独立程序同介质分发 = mere aggregation**（不传染） | 无传染（进程边界 + 公开 API） | 无传染 | 直接传染（衍生作品） |
| **对自研代码许可的压力** | 若与 GPL 代码链接/合并 → 整体须 GPL；仅聚合 → 无压力 | 无 | 无 | 被复用部分须同许可 |
| **专利授权** | Apache-2.0 含明示专利授权；MIT 无 | 不适用 | 不适用 | 按来源许可 |
| **典型落地动作** | `/usr/share/doc/<pkg>/copyright` + `licenses/` + `source-offer` | adapter 内仅 URL 构造 + 部署文档 | 下载器 + 版本/校验和记录 + 镜像来源说明 | 更新本文 + ADR + NOTICE |

### 4.2 本项目采用的映射（Decision-ready）

| 组件 | 采用方式 | 触发的义务（要点） |
|---|---|---|
| Mihomo | **主：Dynamic Dependency**；**备选：Bundled**（提供 `proxy-agent-mihomo` 子包或下载器） | Dynamic：无我方 GPL 义务，但需版本/校验和记录与镜像来源说明。Bundled：GPL-3.0 全文 + Corresponding Source（指向 tag 的源码 tarball 或 §6(d) 同址访问）+ 无修改声明 + 无附加限制 |
| Sub-Store | **External Service** | 不分发：仅需部署文档 + 版本兼容声明 + 不修改源码的纪律；若改为 Bundled：AGPL 全文 + Corresponding Source |
| sub-store-convert | **不使用**（澄清前） | 无；若未来使用则按 AGPL 处理（见 §8） |
| metacubexd | **Bundled 静态产物**（前端资源内嵌） | MIT 版权与许可声明保留（UI 内可访问的 about/licenses 页面 + 发行物 NOTICE）；Highcharts 单独决策；UFL-1.0 与 CC-BY-4.0 字体署名 |
| ShellCrash | **不复制**（仅 feature 对标） | 无 |
| Rust crates | **Bundled**（静态链接） | 每个依赖保留 license 文本；Apache-2.0 依赖需保留 NOTICE（若有）；由 `cargo about` 生成 THIRD-PARTY-NOTICES |
| 前端依赖 | **Bundled** | 仅允许宽松许可；保留声明；禁止 copyleft 与 CC-NC |

---

## 5. Rust 依赖与自研代码许可证建议（含 deny.toml 要点）

### 5.1 治理方法：怎么做

1. **工具**
   - `cargo deny`（0.20.2，`MIT OR Apache-2.0`）：license allow-list + ban + 来源校验 + 安全公告，CI 必跑。`[上游文档声明]`
   - `cargo about`（0.9.2，`MIT OR Apache-2.0`）：生成 `THIRD-PARTY-NOTICES.html/txt`，直接产出可随 deb 分发的 NOTICE。`[上游文档声明]`
   - `cargo license`（0.7.0，MIT）：开发者本地快速巡检。`[上游文档声明]`
2. **在 CI 中固定命令**：
   ```bash
   cargo deny check licenses bans sources advisories
   cargo about generate -o packaging/deb/THIRD-PARTY-NOTICES.html about.hbs
   ```
3. **原则**：新增依赖前先跑 license 检查（对应 AGENTS.md「Dependency Policy」第 2 条"Verify license"）；`cargo deny` 采用 **allow-list** 语义——不在 `allow` 中的许可默认**拒绝**，因此"禁止 GPL/AGPL"无需专门写 deny（但建议显式 deny 以便阅读者在配置里看到意图，见 §5.3 的注意事项）。

### 5.2 MVP 计划依赖的实际许可（逐项核实，2026-09-12，crates.io registry 元数据）

| crate | 最新 stable | license（registry 声明） | crate | 最新 stable | license |
|---|---|---|---|---|---|
| tokio | 1.53.1 | `MIT` | clap | 4.6.6 | `MIT OR Apache-2.0` |
| axum | 0.8.9 | `MIT` | ratatui | 0.30.2 | `MIT` |
| tower | 0.5.3 | （见下注） | crossterm | 0.29.0 | `MIT` |
| tower-http | 0.7.1 | `MIT` | tracing | 0.1.44 | `MIT` |
| reqwest | 0.13.5 | `MIT OR Apache-2.0` | tracing-subscriber | 0.3.23 | `MIT` |
| serde | 1.0.229 | `MIT OR Apache-2.0` | async-trait | 0.1.92 | `MIT OR Apache-2.0` |
| serde_json | 1.0.151 | `MIT OR Apache-2.0` | thiserror | 2.0.20 | `MIT OR Apache-2.0` |
| toml | 1.1.6 | `MIT OR Apache-2.0` | sqlx | 0.9.0 | `MIT OR Apache-2.0` |
| serde_yaml | 0.9.34+deprecated | `MIT OR Apache-2.0` | anyhow | 1.0.104 | `MIT OR Apache-2.0` |
| serde_yml | 0.0.13 | `MIT OR Apache-2.0` | chrono | 0.4.45 | `MIT OR Apache-2.0` |
| uuid | 1.26.1 | `Apache-2.0 OR MIT` | dashmap | 6.2.1 | `MIT` |
| moka | 0.12.16 | `(MIT OR Apache-2.0) AND Apache-2.0` | notify | 8.2.0 | `CC0-1.0` |

注：`tower` 0.5.3 的 `license` 字段在本次查询中未返回（registry 响应的 version 级字段抽取失败），需在 `cargo deny` 落地时以实际解析结果为准；`tower-rs` 系列历史上为 MIT。`[未验证]`

复用示例（证明 allow-list 的必要性）：`gpgme` crate 当前声明 **`LGPL-2.1`**——这类依赖一旦进入 Rust 二进制，需要重新评估（LGPL 的链接要求使纯静态链接 Rust 二进制很难合规）。`[上游文档声明]`

### 5.3 `deny.toml` 建议要点

配置文件语法依据 cargo-deny 官方文档（<https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html>，2026-09-12 抓取）：`allow` 之外的一切许可默认被拒；`deny` / `copyleft` / `allow-osi-fsf-free` / `default` 字段**已被移除并会报错**；自 0.18.4 起 GNU 系许可按 SPDX 精确匹配（`GPL-2.0` 与 `GPL-2.0-only` 不互相匹配）。

```toml
# deny.toml —— 建议要点（本项目 Rust workspace）
[graph]
all-features = true
targets = [
  { triple = "x86_64-unknown-linux-gnu" },
  { triple = "aarch64-unknown-linux-gnu" },
]

[advisories]
version = 2
yanked = "deny"

[licenses]
# 语义：未列出的 license 一律拒绝（因此 GPL/AGPL/SSPL/BUSL/CC-NC 自动被拒）
confidence-threshold = 0.95
unused-allowed-license = "deny"          # allow 列表里有未使用的许可 → 报错，保持列表精简
allow = [
  "MIT",
  "Apache-2.0",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "0BSD",
  "Zlib",
  "Unicode-3.0",
  "CC0-1.0",
]
# 需要逐 crate 审批的许可（例如 MPL-2.0 的 file-level copyleft）：不放 allow，走例外
[[licenses.exceptions]]
crate = "<crate-name>"
allow = ["MPL-2.0"]

# 自动探测器识别不了的 crate（如 ring 类多许可组合）用 clarify 显式固定
# [[licenses.clarify]]
# crate = "ring"
# expression = "MIT AND ISC AND OpenSSL"
# license-files = [
#   { path = "LICENSE", hash = 0xbd0eed23 },
# ]

[bans]
multiple-versions = "warn"
wildcards = "deny"
# 明确禁止已知会引入 copyleft / 重量级 C 依赖的 crate
deny = [
  { name = "gpgme" },     # LGPL-2.1（本次核实）
]

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

要点说明：
- **"禁止 GPL/AGPL 进入 Rust 二进制"的实现方式就是 allow-list**：不把 GPL/AGPL/LGPL/SSPL/BUSL/CC-BY-NC 写进 `allow`。若希望在配置里显式表达意图，可在注释中列出"explicitly denied families"，但不要使用已被移除的 `deny`/`copyleft` 字段（会报错）。
- `unused-allowed-license = "deny"` 保证 allow 列表不会悄悄膨胀。
- `[sources]` 阻止从非 crates.io / 未登记的 git 源引入依赖（同时防止"意外 vendor"）。
- `include-dev` 默认 `false`、`include-build` 默认 `true`——build-dependencies 会影响构建产物，保持默认开启。`[上游文档声明]`
- `[graph] targets` 至少覆盖 `x86_64-unknown-linux-gnu` 与 `aarch64-unknown-linux-gnu`（对应 MVP 目标架构），避免把仅在其它平台生效的依赖漏检。

### 5.4 自研代码建议的许可证：`MIT OR Apache-2.0`

- **建议**：Rust workspace 与自研前端统一采用 **`MIT OR Apache-2.0`** 双许可（SPDX 表达式 `MIT OR Apache-2.0`），与 Rust 生态事实标准一致（tokio/axum/serde/clap/sqlx 等均为此模式）。`[推测]`
- **兼容性**：
  - 与 MIT/Apache-2.0/BSD/ISC 依赖：完全兼容；下游可选择任一许可。
  - 与 Apache-2.0 依赖：Apache-2.0 的专利授权与 NOTICE 传递义务照常履行（我们自己的 NOTICE 中汇总第三方 NOTICE）。
  - 与 **GPL-3.0 依赖**：**不兼容于"保持 MIT"的目标**。若二进制链接/合并 GPL-3.0 代码，整个组合作品必须按 GPL-3.0 分发（GPL-3.0 §5），我们的 MIT OR Apache-2.0 只能覆盖我们自己的那部分代码，不能覆盖合并后的二进制。→ 因此策略是**架构层面隔离**（Mihomo 走进程边界，绝不链接）。
  - 与 **AGPL-3.0**：同理且更强（含网络条款）。Sub-Store 必须停留在进程边界之外。
  - 与 LGPL-2.1 依赖（如 `gpgme`）：Rust 静态链接场景下合规成本极高 → 直接禁止。
- 若未来决定把整个产品改为 GPL-3.0（例如为了深度集成某 GPL 组件），那是一次需要 ADR 的许可策略变更，不在本文结论内。

---

## 6. 前端资源与 Dashboard 内嵌的合规要求

### 6.1 建议的 license 白名单（自研 UI 与第三方前端依赖）

**默认允许（白名单）**：`MIT`、`Apache-2.0`、`BSD-2-Clause`、`BSD-3-Clause`、`ISC`、`0BSD`、`CC0-1.0`、`Unlicense`、`Zlib`、`Unicode-3.0`。

**允许但需署名/附加条件**：
- `OFL-1.1`（SIL Open Font License，字体）：允许内嵌与再分发，需随附 OFL 全文与保留字体名称（Reserved Font Name）规则；
- `UFL-1.0`（Ubuntu Font Licence，本次已核实用于 metacubexd 的 Ubuntu 字体）：允许 bundle/embed，需随附许可并署名；
- `CC-BY-4.0`（如图形/emoji 资产，本次已核实用于 Twemoji flags 字体）：允许商用与再分发，**须署名 + 提供许可链接 + 说明是否修改**；
- `MPL-2.0`：file-level copyleft，未修改的依赖通常可接受，但要求"修改的文件须回馈"→ 建议逐项走 `exceptions`，不做全局白名单。

**必须审查 / 默认拒绝**：
| 许可 | 处理 |
|---|---|
| `GPL-2.0*` / `GPL-3.0*` / `AGPL-3.0*` | 拒绝（前端 bundle 会与自研代码合并成同一作品） |
| `LGPL-2.1*` / `LGPL-3.0*` | 拒绝（前端打包/压缩后无法满足动态链接豁免） |
| `SSPL-1.0`、`BUSL-1.1`、`Elastic-2.0`、`Commons-Clause` | 拒绝 |
| `CC-BY-NC-*`、`CC-BY-ND-*` | 拒绝（非商业 / 禁止演绎） |
| 专有/商用许可（如 **Highcharts** `https://www.highcharts.com/license`、`SEE LICENSE IN …`） | 逐项法务审查；默认拒绝 |
| 无 `license` 字段的包 | 视为 `[未验证]`，需人工确认后才能引入 |

### 6.2 内嵌 metacubexd 构建产物的具体动作

1. **保留版权与许可声明**（MIT 义务）：在发行物中包含 metacubexd 的 `LICENSE`（`Copyright (c) 2023 MetaCubeX`）原文；在 UI 中提供可访问的 "About / Licenses" 入口或 `licenses.txt` 静态文件。MIT 原文要求 "The above copyright notice and this permission notice (including the next paragraph) shall be included in all copies or substantial portions of the Software." `[上游 LICENSE 原文]`
2. **解决 Highcharts**（最高优先）：
   - 方案 A：构建时排除/替换 Highcharts，接受图表功能降级或自研轻量图表；
   - 方案 B：取得 Highsoft 的商业授权（成本与条款需法务评估）；
   - 方案 C：不内嵌 UI 构建产物，改为 External Service（用户自行部署 Dashboard）——可把许可风险转移出去，但会降低开箱体验。
   - 无论哪个方案，都需在 `frontend/metacubexd` 的集成脚本里记录"Highcharts 是否存在于产物中"的检查（例如构建后 grep 产物中 Highcharts 特征字符串）。`[推测]`
3. **字体署名**：为 Ubuntu 字体附 UFL-1.0 全文；为 Twemoji flags 附 Apache-2.0 与 CC-BY-4.0 声明（含 Twemoji 项目署名与许可链接）。
4. **依赖声明清单**：为前端产物生成 npm 层的 `THIRD-PARTY-NOTICES`（例如 `license-checker` / `pnpm licenses list`），并与 Rust 侧 NOTICE 合并为 deb 内的单一 NOTICE 文件。
5. **版本固定**：记录 metacubexd 的版本与 commit（当前 `1.273.1`），不要跟随 `main` 浮动；MIT 允许修改，但一旦我们 patch 了 UI，需要在 NOTICE 中声明修改。
6. **不要**把 metacubexd 仓库整体 vendor 进本仓库：以"build script + 固定 commit + 产物落在 `frontend/`"的方式管理，避免把上游许可与依赖树混入我们的 license 范围。`[推测]`

### 6.3 自研 UI 时的额外要求

- 每个新前端依赖在 PR 中声明 SPDX 并对照 §6.1 白名单；
- 禁止 `postinstall` 拉取运行时远程资源（如构建期外的 CDN 字体/脚本），否则运行时会引入不受控的许可与隐私风险；
- 图标/插画/字体等资产必须记录来源与许可（ICON 库许可常与代码库不同）。

---

## 7. 分发模型的可执行合规清单

> 目标分发模型：deb 包（Debian/Ubuntu）+ systemd，可能内嵌前端静态资源；Mihomo 默认运行时下载，可选内置。

### 7.1 deb 包内必须放置的文件

```text
/usr/share/doc/proxy-agent/copyright                     # DEP-5 机器可读版权文件（Debian 约定）
/usr/share/doc/proxy-agent/THIRD-PARTY-NOTICES.txt       # cargo about + npm license 汇总
/usr/share/doc/proxy-agent/licenses/GPL-3.0.txt          # 若 Bundled Mihomo（或 ShellCrash 之外的 GPL 组件）
/usr/share/doc/proxy-agent/licenses/AGPL-3.0.txt         # 仅当 Bundled Sub-Store / sub-store-convert
/usr/share/doc/proxy-agent/licenses/MIT-metacubexd.txt   # 内嵌 Dashboard 时
/usr/share/doc/proxy-agent/licenses/UFL-1.0.txt          # Ubuntu 字体
/usr/share/doc/proxy-agent/licenses/CC-BY-4.0.txt        # Twemoji 图形资产（+ 署名信息）
/usr/share/doc/proxy-agent/licenses/MIT.txt              # 自研代码选 MIT 时附
/usr/share/doc/proxy-agent/licenses/Apache-2.0.txt       # 自研代码选 Apache-2.0 时附
/usr/share/doc/proxy-agent/source-offer.txt              # GPL/AGPL Corresponding Source 获取方式（若适用）
/usr/share/doc/proxy-agent/BUILD-INFO.txt                # 版本、commit、构建环境、补丁集（无补丁则声明）
```

### 7.2 Mihomo 源码提供（Bundled 场景）

选择其一并在 `source-offer.txt` 中写明：

- **方案 1（推荐，§6(d)）**：在发布 .deb 的同一网络位置（GitHub Release / 自建 apt 源的同目录）提供与二进制版本严格对应的源码归档（`mihomo-v1.19.30-source.tar.gz`，取自上游 tag，附我们的构建脚本），并在 deb 内写清访问地址；
- **方案 2（§6(b)）**：在 deb 内附**有效期至少三年**的书面 offer，说明可索取 Corresponding Source（费用不超过实际分发成本）；
- 无论哪种，都必须声明"未修改上游源码"或附带我们的补丁（若有）；
- 若走 **Dynamic Dependency**（推荐默认），则只需在文档中说明下载来源与校验和，不产生源码提供义务。`[推测]`

### 7.3 避免 AGPL 传染的强制约束（Sub-Store 及 sub-store-convert）

1. **进程边界**：Sub-Store 永远以独立进程/容器运行；Rust agent 只通过 HTTP/Unix socket 调用其公开 API，不做 FFI、不链接、不 dlopen。
2. **不修改源码**：不 fork、不打补丁、不改其前端；若必须修，则该实例的运营方需向网络用户提供 Corresponding Source（AGPL §13）。
3. **不内嵌其前端构建产物**：把 Sub-Store 的 Web UI 打进我们的 deb 会使其成为我们分发物的一部分。
4. **不复制算法实现**：不在本仓库写"逐行翻译"的等价代码；如需本地转换，走独立的 `NativeConverter`（自研，注意不要基于 sub-store-convert 的 bundle 反编译/改写）。
5. **不使用 `sub-store-convert` 作为分发物**：其 bundle 内联 AGPL 代码且缺少许可声明（§3.3）；在 §8 的确认项闭环前，它不应出现在 deb/镜像/依赖里。
6. **文档与 UI 归属**：在 UI/文档中把 Sub-Store 标注为独立第三方组件及其许可，避免用户误认为它由我们分发的同一作品。

### 7.4 自研代码许可与 NOTICE 流程

- 自研 Rust/前端：`MIT OR Apache-2.0`（§5.4），仓库根放 `LICENSE-MIT` 与 `LICENSE-APACHE`；
- CI 门禁：`cargo deny check` + 前端 license 扫描 + `cargo about` 产物更新检查（产物变化需在 PR 中可见）；
- 任何新增 Bundled 组件必须在本文 §2 的矩阵中新增一行，并补齐 §7.1 的文件。

### 7.5 命名与商标

- 产品名、deb 包名、二进制名避免包含 `mihomo`（上游 README 的命名要求）；建议 `proxy-agent` / `proxyctl` / `proxy-manager`。`[上游文档声明]` `[法律问题-待确认]`
- 文档中描述性使用 "Mihomo"、"Sub-Store"、"metacubexd" 指代上游项目属正常引用；不要使用其 Logo 造成官方背书印象（metacubexd 的品牌资源同样受商标约束）。`[推测]`

---

## 8. 待法务/维护者确认清单

> 以下问题本文**不下结论**，需法务或上游维护者明确。

| # | 问题 | 对象 | 为何重要 | 建议行动 |
|---|------|------|----------|----------|
| Q1 | `sub-store-convert` 以 MIT 声明发布，但其 bundle 内联了 Sub-Store（AGPL-3.0）的 `proxy-utils` 源码，且无任何许可声明。我们把它作为依赖/分发物是否合法？若合法，须履行哪些义务？ | 上游维护者 + 法务 | 直接决定 MVP 的 `SubStoreConvertAdapter` 能否落地 | 向上游（npm 包作者 / Sub-Store 团队）询问来源与许可；在澄清前禁止 Bundled |
| Q2 | 通过第三方镜像（如 `ghproxy`/`ghfast.top`）下载 Mihomo 二进制再分发给用户，是否构成 GPL-3.0 意义上的 conveying？镜像站是否为上游授权的分发渠道？ | 法务 + Mihomo 维护者 | 影响默认下载路径的合规性 | 优先使用官方 GitHub Release；若必须用镜像，记录其来源与校验和并取得法律意见 |
| Q3 | Mihomo README 的 "shall not contain the word `mihomo` in their names" 是否构成 GPL-3.0 §7 意义上的 additional restriction？对我们包名/产品名的约束边界在哪里（deb 包名、二进制名、文档标题是否都算）？ | 法务 + Mihomo 维护者 | 影响产品命名与 deb 包名 | 采用不含 `mihomo` 的产品名以规避；如需使用，先取得书面许可 |
| Q4 | `MetaCubeX/mihomo` 默认分支 `main` 当前承载与本项目无关的 MIT Python 库，导致仓库级 license 元数据为 MIT。这是有意的吗？是否会被修正？ | Mihomo 维护者 | 合规工具会误判许可，用户也可能误以为内核是 MIT | 我们侧固定 tag/`Meta` 分支；同时向上游反馈该元数据问题 |
| Q5 | 内嵌 metacubexd 构建产物时，Highcharts 的使用是否落入 Highsoft 的 "Commercial use"？我们的产品若以开源/免费方式分发，是否满足其非商业 EULA？ | 法务 + Highsoft | 决定 Dashboard 能否原样内嵌 | 优先移除/替换 Highcharts；否则评估商业授权或改为 External Service |
| Q6 | 是否允许在我们分发的 deb 中附带 `LICENSE`/`NOTICE` 之外的 Highcharts 声明？Highsoft 对再分发（redistribution）的具体要求是什么？ | Highsoft | 若保留 Highcharts，需要满足其再分发条款 | 索取 Highsoft 官方说明 |
| Q7 | 构建期由 `@nuxt/fonts` 从 Google 自托管的 Ubuntu 字体，其 UFL-1.0 是否对"随 deb 分发字体文件"有额外交付要求（保留字体名称、署名位置）？ | 法务 | 影响字体资产的合规声明 | 按 UFL-1.0 原文执行并在 NOTICE 中署名；构建后核实产物内实际字体文件清单 |
| Q8 | 若我们以程序化方式为用户下载并安装 Sub-Store（而非让用户自行获取），是否构成 AGPL-3.0 的 conveying？ | 法务 | 决定"一键部署 Sub-Store"功能是否可行 | 在功能设计前确认；保守做法是提供文档引导用户从上游获取 |
| Q9 | 若我们对 Sub-Store 源码打了任何补丁（哪怕一行），AGPL-3.0 §13 即触发，须向网络用户提供 Corresponding Source。我们能否承诺"永不修改"？如何在工程上强制？ | 产品/架构 + 法务 | 关系 §13 是否长期不触发 | 写进架构规则（未修改的上游镜像 + 不允许 patch）；CI 校验镜像 digest |
| Q10 | GPL-3.0 的 "only" 与 "or later"：Mihomo/ShellCrash 的 LICENSE 未声明 "or any later version"，README 仅写 "GPL-3.0"。我们应按 `GPL-3.0-only` 处理吗？ | 法务 | 影响 SPDX 标识符与下游授权范围 | 保守按 `GPL-3.0-only`；在文档中同时记录上游原文 |
| Q11 | Mihomo 内核内嵌的 `transport/hysteria/conns/faketcp/LICENSE` 仅有一句话（指向 `xtaci/tcpraw`），未含完整许可正文。Bundled 时我们是否需要补全该组件的许可？ | 法务 + Mihomo 维护者 | 影响 Bundled 场景的 NOTICE 完整性 | 向上游确认该文件的适用许可；在 NOTICE 中按上游 LICENSE 文件原文如实转述 |
| Q12 | 自研代码选择 `MIT OR Apache-2.0` 是否符合项目长期商业计划（是否考虑未来闭源分发、专利防御）？ | 项目所有者 + 法务 | 许可一旦发布难以收回 | 在首个 release 前确定 |
| Q13 | deb 分发 GPL-3.0 组件时，Debian 政策要求源码在同一 archive 中可得。我们的发布渠道（自建 apt 源 / GitHub Release）能否满足 Corresponding Source 的可获得性与保存期限？ | 法务 + 发布/运维 | 直接决定 Bundled 方案是否可长期维持 | 设计发布流程时把源码归档纳入产物 |
| Q14 | 若最终决定 Bundled 分发 Mihomo，是否需要在 deb 中提供与二进制完全对应的构建脚本与依赖锁定（可复现构建）以满足 §6 的 Corresponding Source 定义？ | 法务 + 构建 | Corresponding Source 包含"生成、安装、运行所需的脚本" | 采用可复现构建并在 source 包中提供构建脚本 |
| Q15 | `@metacubexd/config-editor` 等 workspace 私有包没有 `license` 字段，是否由根 MIT 覆盖？我们内嵌其产物时的许可依据是什么？ | metacubexd 维护者 + 法务 | 影响 NOTICE 的完整性声明 | 以根 LICENSE 为准并在 NOTICE 中说明；向上游确认 |
| Q16 | 前端资产（favicon、PWA 图标、Twemoji flags 字体）的品牌与商标使用边界是什么？ | 法务 + 上游 | 避免商标/背书风险 | 保留原始来源标注，不做品牌改造 |
| Q17 | 本文所有许可判断是否需要在正式发布前由法务出具书面意见？ | 项目所有者 | 本文仅为信息收集 | 在首次对外发布前安排法务审阅 |

**待确认项数量：17 项**（Q1–Q17）。其中 **Q1、Q2、Q5 为阻塞级**（分别阻塞 `sub-store-convert` 的使用、默认下载路径、Dashboard 内嵌）。

---

## 9. 证据与来源

### 9.1 抓取记录（全部为 2026-09-12 UTC）

| 组件 | 抓取 URL | 文件大小 | sha256 | 证据分级 |
|---|---|---|---|---|
| Mihomo | `https://raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.30/LICENSE` | 35149 B | `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986` | `[上游 LICENSE 原文]` |
| Mihomo | `https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/LICENSE` | 35149 B | 同上（逐字节一致） | `[上游 LICENSE 原文]` |
| Mihomo（默认分支，异常） | `https://raw.githubusercontent.com/MetaCubeX/mihomo/main/LICENSE` | 1049 B | `2278f74ad468f0995467b5bd9df3c7bbf1bdfd57a135dac7a9d14c0e366b75a3`（MIT） | `[上游 LICENSE 原文]` |
| Mihomo | `.../v1.19.30/README.md`（§License）、`.../v1.19.30/go.mod` | — | — | `[上游文档声明]` |
| Mihomo（内嵌第三方） | `.../v1.19.30/transport/kcptun/LICENSE.md`、`.../transport/hysteria/conns/faketcp/LICENSE` | — | — | `[上游 LICENSE 原文]` |
| Sub-Store | `https://raw.githubusercontent.com/sub-store-org/Sub-Store/master/LICENSE` | 34577 B | `08e3bf9a0da8b80a8ca22489937661f31d3f851cd37a27033645bb05bbe4da90` | `[上游 LICENSE 原文]` |
| Sub-Store | `.../master/backend/package.json`（`"license": "AGPL-3.0"`，v2.39.6）、`.../master/README.md`（L166-168） | — | — | `[上游文档声明]` |
| sub-store-convert | `https://registry.npmjs.org/sub-store-convert`；`.../-/sub-store-convert-2.36.33.tgz` | tarball 66658 B | `415aa639165966013752de90eb03b2130023df2cd066280b8710cdd1eac73663` | `[上游 LICENSE 原文]`（包内 `index.js` 内容） |
| metacubexd | `https://raw.githubusercontent.com/MetaCubeX/metacubexd/main/LICENSE` | 1096 B | `cd0735ba06f26a0008bbca399890c7ca87fe129aacc302c2e33fb03e60a4e8c3` | `[上游 LICENSE 原文]` |
| metacubexd | `.../main/package.json`、`.../main/packages/ui/package.json`、`.../main/pnpm-workspace.yaml`、`.../main/packages/ui/nuxt.config.ts` | — | — | `[上游文档声明]` |
| Highcharts | `https://cdn.jsdelivr.net/npm/highcharts@13.0.2/LICENSE.txt`（与 `https://unpkg.com/highcharts@13.0.2/LICENSE.txt` 一致） | 348 B | — | `[上游 LICENSE 原文]` |
| Ubuntu 字体 | `https://raw.githubusercontent.com/google/fonts/main/ufl/ubuntu/UFL.txt` | 4673 B | `2f0015108d68627bd788d313f529c21ff4da2c2c42a5e1f3883acc83480f9002` | `[上游 LICENSE 原文]` |
| Twemoji 字体 | `https://raw.githubusercontent.com/mozilla/twemoji-colr/master/LICENSE.md` | 2172 B | `64419edc28e9163204c3be73f835a8dfc34cd6c9b8f7d067bb685f119f839a99` | `[上游 LICENSE 原文]` |
| ShellCrash | `https://raw.githubusercontent.com/juewuy/ShellCrash/dev/LICENSE.txt` | 35149 B | `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986` | `[上游 LICENSE 原文]` |
| ShellCrash | `.../dev/README_CN.md`（L183-185 许可协议） | — | — | `[上游文档声明]` |
| Rust crates | `https://crates.io/api/v1/crates/<name>`（license 字段取自 version 级元数据） | — | — | `[上游文档声明]` |
| 前端依赖 | `https://registry.npmjs.org/<pkg>`（`license` 字段） | — | — | `[上游文档声明]` |
| cargo-deny 配置语法 | `https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html` | — | — | `[上游文档声明]` |

### 9.2 主要链接

- Mihomo：<https://github.com/MetaCubeX/mihomo> ・ release `v1.19.30`（2026-08-16）
- Sub-Store：<https://github.com/sub-store-org/Sub-Store>（backend v2.39.6）
- sub-store-convert：<https://www.npmjs.com/package/sub-store-convert>（v2.36.33，**无 repository 字段**）
- metacubexd：<https://github.com/MetaCubeX/metacubexd>（v1.273.1）
- ShellCrash：<https://github.com/juewuy/ShellCrash>
- Highcharts license：<https://www.highcharts.com/license> ・ <https://www.highcharts.com/license-eula>
- Ubuntu Font Licence：<https://ubuntu.com/legal/font-licence>
- Twemoji：<https://github.com/twitter/twemoji>（CC-BY-4.0）
- cargo-deny：<https://github.com/EmbarkStudios/cargo-deny> ・ <https://embarkstudios.github.io/cargo-deny/>
- cargo-about：<https://github.com/EmbarkStudios/cargo-about>

### 9.3 环境与限制（影响证据强度）

- `api.github.com` 未认证额度已耗尽（本次抓到 `remaining: 0`），因此仓库级元数据仅使用了早前的少量请求；所有关键判断均已改用 `raw.githubusercontent.com` 原文 + jsDelivr/unpkg 第二来源交叉验证。
- `github.com` 直连超时，`git clone` 未能完成；本报告的许可判断不依赖 clone。
- 所有临时实验文件位于 `/tmp/r13-lic/`，任务结束后清理；本仓库仅新增 `docs/research/13-licenses.md` 一个文件。
- 未构建 metacubexd 产物，因此"构建产物内实际包含哪些字体/图表库文件"为 `[推测]`，需在实现阶段用产物扫描验证。

---

## 10. 未验证假设与开放问题

### 10.1 未验证假设（`[未验证]` / `[推测]`）

| # | 假设 | 分级 | 验证方式 |
|---|------|------|----------|
| A1 | `MetaCubeX/mihomo` 的 `main` 分支异常是上游当前真实状态（而非缓存污染）——已用 raw + jsDelivr 双源验证，但未使用 git 协议核验分支指针 | `[上游 LICENSE 原文]`（双源一致） | 网络条件允许时用 `git ls-remote` 核验 `refs/heads/main` |
| A2 | Mihomo 内核源码在 `Meta` 分支与所有 tag 上均为 GPL-3.0 | `[推测]`（已抽检 `v1.18.0`、`v1.19.0`、`v1.19.11`、`v1.19.14`、`v1.19.15`、`v1.19.30`，均为 GPL-3.0） | 对历史 tag 全量抽检或核对 tag 签名 |
| A3 | sub-store-convert 的 bundle 是"内联源码"而非仅保留路径注释 | `[推测]`（首行注释后紧跟实际代码，27 个唯一源码路径，bundle 419 KB，无 sourcemap） | 与 Sub-Store 对应文件做片段比对；向上游确认 |
| A4 | `@nuxt/fonts` 在构建期把 Google Ubuntu 字体自托管进产物 | `[推测]`（基于模块定位与 `nuxt.config.ts` 配置） | 实际执行 `pnpm build:ui` 后扫描产物中的字体文件 |
| A5 | 内嵌 metacubexd 产物时 Highcharts 会被打包进 dist | `[推测]`（`highcharts` 在 `dependencies` 中） | 构建后扫描产物中的 Highcharts 特征字符串 |
| A6 | 运行时的 deb 里 mihomo 二进制与我们的 Rust 二进制是独立程序（mere aggregation） | `[推测]` | 在构建/manifest 层面确认无链接关系并写入文档 |
| A7 | `tower` 0.5.3 的 license 为 MIT 系 | `[未验证]` | 用 `cargo deny`/`cargo metadata` 在临时工程中解析确认 |
| A8 | `@metacubexd/config-editor` 无 `license` 字段，由根 MIT 覆盖 | `[未验证]` | 向上游确认或查 pnpm license 输出 |
| A9 | 本文未覆盖 `Mihomo` 全部第三方内嵌组件 | `[未验证]` | 对内核 `go.mod` 做全量依赖许可扫描（`go-licenses`）后再更新本文 |

### 10.2 开放问题（不阻塞当前结论但需跟踪）

1. metacubexd 从 SolidJS SPA 迁移到 Nuxt/Vue monorepo 后，**内嵌方式与体积**是否需要重新设计（`apps/server` 的存在意味着上游也提供独立服务形态，可能更适合 External Service 模式）？
2. 若最终采用 `NativeConverter`，如何确保其实现与 Sub-Store 的功能等价性来自**行为规格**而非**代码阅读**（避免衍生作品争议）？
3. 是否需要为"用户自建 Sub-Store"提供官方镜像/Compose 模板？提供模板是否会被认定为 promoting/redistributing AGPL 软件（`[法律问题-待确认]`）？
4. deb 的 DEP-5 `copyright` 文件是机器可读格式，需要为每个第三方组件生成结构化条目；是否引入自动化生成（`cargo about` + npm license 扫描 + 手工条目）？
5. 若未来支持更多架构或发行版，`[graph] targets` 与许可扫描矩阵需同步扩展。
6. 是否需要为 `sub-store-convert` 准备一个"仅调用其 HTTP 服务"的降级路径，以便在 Q1 结论不利时快速切换？
7. 上游许可变更监控机制：是否需要定期（如每季度）复跑本报告的抓取脚本，并在 license 指纹变化时告警？（建议把 sha256 作为基线）

---

*本文档为 R13 的初稿产出，后续实现阶段若引入新的 Bundled 组件，必须同步更新 §2 矩阵、§7 清单与 §8 待确认项。*
