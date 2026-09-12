# ADR-004 — 配置生命周期与回滚模型

| 字段 | 值 |
|---|---|
| Status | Accepted（Phase 0 定稿） |
| Date | 2026-09-12 |
| Related | ADR-001、ADR-002、ADR-003、`docs/research/01-mihomo.md`、`02-mihomo-config.md`、`09-linux-runtime.md`、`13-licenses.md` |

---

## 1. Context

核心不变量（`AGENTS.md`）：

> 失败的更新、转换、reload、能力探测，只能导致"能力降级"，绝不能导致"当前可用配置被破坏"。

### 1.1 需求约束

REQ-CONFIG-001~010：每份激活配置有版本 + checksum；失败不得替换激活配置；原子写入；list/show/validate/diff/activate/rollback；三层校验；激活流程含失败回滚；版本不可变；来源可追溯。

### 1.2 调研结论

| # | 结论 | 证据 |
|---|---|---|
| C1 | **解析失败时 reload 是安全的**：非法 YAML → HTTP 400，**实例不受影响、旧配置继续生效**（11 个边界用例复现） | R01 §1、R02 §4（实测） |
| C1b | **⚠️ 但 reload 不是事务性的**：若新配置**解析通过而 listener 绑定失败**，`ApplyConfig` 无返回值、失败不回滚、HTTP 仍返回 204。`force=false` → 旧端口继续监听（安全降级）；**`force=true` → 旧监听器被拆除、新端口绑定失败、`/configs` 变成 `mixed-port: 0`，且后续合法 reload 无法恢复，必须重启进程** | R02 §1 C5/C6（实测，本 ADR 的关键修正） |
| C2 | **空 body 会 400 `Body invalid`**，必须发 `{}` 或带 `payload`/`path` 的 JSON | R01 §1 |
| C3 | `path` 必须是绝对路径且在 home 或 `SAFE_PATHS` 内，否则 400（报错回显允许路径）；`SAFE_PATHS` 以 `:` 分隔，homeDir 自动加入 | R01 §1、R02 §1 C7 |
| C4 | `GET /configs` **仅 33 字段**，不含 `dns`/`proxies`/`rules`/`providers` → **不能作为配置真相** | R01 §1 |
| C4b | **`mihomo -t` 有两个高危副作用**：① 含 `GEOIP`/`GEOSITE` 规则时会**真实联网下载 geodata**（实测阻塞 90s/75s，且损坏的 `geoip.metadb` 会被删除重下）；② **`-t -f <不存在的文件>` 返回 exit 0 并"成功"**（mihomo 自动创建初始配置）→ **最危险的校验假阳性** | R02 §1 C3（实测；本会话已独立复现） |
| C4c | **`-t` 不检测未知字段**：`mixed-portt`（拼写错误）**exit 0 通过**，配置被静默忽略 → Level 2 **必须** 由 `mihomo -t` **⊕** Agent 字段白名单共同承担 | R02 §1 C2（实测） |
| C4d | **上游不存在任何官方机器可校验 schema**（1222 项完整树中无 schema 文件；官方文档 "schema" 出现 0 次）；"事实 schema"只有 `config.go` 的 yaml tag | R02 §7（上游源码；回答 Q001） |
| C5 | 上游**没有**"激活后健康检查失败自动回滚"的编排；这正是我们要自研的部分 | R01、R15 §1 |
| C5b | **Rollback 必须用 restart 而非 reload**：实测 reload 无法从"僵尸态"（绑定失败后）恢复 | R02 §1 C5（实测） |
| C6 | ShellCrash 的对照：只有单份 `config.yaml.bak`；启动失败写 `.start_error` 后**禁止自启**（自我锁死），不是回滚到 known-good | R08 §1 |
| C7 | metacubexd 的对照：profile compose/activate + **单槽 `.bak`**，无版本历史 | R07 §1 |
| C8 | 竞品普遍缺少 `list/show/diff/activate <id>/rollback <id>` 这套历史版本接口 → 我们最硬的差异化 | R15 §1 |
| C9 | `mihomo -t` 是内核自身校验入口，可作为语义门禁；ShellCrash 也是"替换前先 `-t`" | R02、R08 §1 |
| C10 | `/configs/geo` 是 fire-and-forget，204 不代表成功 → geodata 更新不可作为激活成功的判据 | R01 §1 |
| C11 | 配置更新与内核更新必须分离（两套失败域） | 设计文档 §39、R01 §1 |
| C12 | 配置目录需持久化且权限受控；`RuntimeDirectory` 只放 socket | R09 C8/C10 |

---

## 2. Decision

### D1. 配置是不可变的版本对象

```text
/var/lib/proxy-agent/
├── configs/
│   ├── v001.yaml            # 不可变
│   ├── v002.yaml
│   └── v003.yaml
├── state/
│   ├── active -> configs/v003.yaml     # 原子切换的符号链接或状态记录
│   └── active.json                     # {version_id, checksum, source, activated_at}
└── database.sqlite                      # 元数据（不存完整 YAML）
```

- 每份版本有 `ConfigVersionId` + `ConfigChecksum` + `ConfigSource` + `created_at` + `activated_at`。
- **已生成的版本不可修改**；"修改"永远是"生成新版本"。
- 回滚 = **激活一个旧版本**，不改写历史（REQ-CONFIG-007）。
- 完整 YAML 放文件系统，SQLite 只存元数据（`AGENTS.md` Persistence）。

### D2. 三层校验 + 临时验证 + 资源预检

```text
L0 资源预检（新增，见 C1b/C4b）
    - 目标端口是否可用（避免 force 场景下的僵尸态）
    - geodata 是否已存在（离线环境不得触发下载）
    - provider 可达性（若配置引用远端 provider）
L1 YAML 语法                （纯解析，无副作用）
L2 语义校验                 （mihomo -t  ⊕  Agent 字段白名单）
L3 运行期验证               （激活 + reload + 分层健康检查；失败即回滚）
```

- **L2 必须由 `mihomo -t` 与 Agent 自建字段白名单共同承担**：
  - `-t` **不检测未知字段**（`mixed-portt` 拼写错误 exit 0 通过，C4c）→ Agent 必须从 `config.go` 的 yaml tag 生成白名单做拼写/未知字段检查。
  - 上游**没有官方 schema**（C4d），不得等待或依赖它。
- **L2 的 `-t` 调用有硬性约束**（C4b）：
  1. **必须在隔离的临时 `-d` 目录中执行**，避免污染真实工作目录。
  2. **必须传入真实存在的配置文件**——否则 `-t` 会因 mihomo 自动创建初始配置而**返回 exit 0 假成功**；实现上必须先确认文件存在且非空。
  3. **含 `GEOIP`/`GEOSITE` 规则的配置会触发真实 geodata 下载**（实测阻塞 90s/75s）→ 离线环境必须**预置 geodata** 或跳过 L2 并降级为告警（不能因校验本身导致激活失败）。
  4. `-t` 不写 `cache.db`，但不保证零残留 → 校验后清理临时目录。
- `-t` 无法覆盖运行期问题（端口占用、权限、TUN 不可用）→ 由 L0 + L3 兜底。

### D3. 激活流程（唯一权威路径）

```text
生成候选配置（Agent 生成，见 ADR-002 D3）
        ↓
写入临时文件 + flush/fsync
        ↓
原子 rename 到 configs/vNNN.yaml      （不可变版本落盘）
        ↓
L1 语法校验
        ↓
L2 语义校验（mihomo -t ⊕ Agent 字段白名单；隔离目录 + 文件必须存在）
        ↓
激活：写 active 指针（原子 rename）
        ↓
reload：PUT /configs  ⚠ 禁止 force=true；必须带 JSON body；204 不代表生效
        ↓
L3 分层健康检查（进程/Controller/配置/端口）
        ↓
   ┌────┴────┐
  成功       失败
   ↓          ↓
记为 active   自动回滚：**重启进程**（而非 reload）激活上一版本 → 健康检查
（写审计）     （回滚也失败 → 标记 Degraded + 告警，保留现场）
```

**关键点**：

1. **禁止 `?force=true`**（C1b，本 ADR 相对初稿的最重要修正）。实测：解析通过但端口绑定失败时，`force=true` 会**拆除旧监听器**、留下 `mixed-port: 0` 的**僵尸态**，后续合法 reload 连续无法恢复、必须重启进程 —— 直接违反核心不变量。若确需强制作业，必须走人工确认路径并前置 L0 端口预检。
2. **reload 后必须做数据面健康检查**（C1b）：`ApplyConfig` 无返回值，HTTP 204 与真实生效解耦。
3. **回滚必须用 restart 而非 reload**（C5b）：reload 无法从僵尸态恢复。
4. **失败绝不停机**：任何一步失败都不得停止 Mihomo（REQ-SUB-003 / REQ-CONFIG-002）。
5. **reload 优先 payload 模式**：绕过 `SAFE_PATHS` 与文件存在性两个失败点；若用 `path` 模式，配置目录必须显式加入 `SAFE_PATHS`。
6. **必须发 JSON body**（C2）——空 body 返回 400，需单元测试固化。
7. **`GET /configs` 不作为真相**（C4）：激活确认靠"我们写入的版本 + checksum"与分层健康检查。
8. **geodata 更新不参与成功判定**（C10）。
9. **L0 资源预检是新增必需步骤**：离线环境含 `GEOIP` 规则会导致 `-t` 卡满超时后 fatal（R14 新增实测），必须在预检阶段拦截。

### D4. 健康检查门禁（激活后判据）

```text
L1 进程存活
L2 GET /version == 200（含 secret 鉴权成功）
L3 GET /configs 返回 200（且 mode/ports 与期望一致，仅比对可得字段）
L4 mixed-port TCP connect 成功        ← 兜底 bind 失败（R01 C11）
```

L4 是必需的：Mihomo 在 listener bind 失败时**不致命**、仅记 error，`/version` 仍 200（C11/R01）。只检查 API 会把"代理端口没起来"误判为成功。

### D5. 回滚模型

| 触发 | 行为 |
|---|---|
| L0/L1/L2 失败 | **不激活**，旧配置继续运行（无副作用）；若 L0 判定端口被占用，须先解决占用而非强推 |
| 激活后 reload 失败（400 等） | 自动回滚：**重启进程**激活上一版本 → 健康检查。**不用 reload 回滚**（C5b：reload 无法从僵尸态恢复） |
| 健康检查失败 | 同上；若回滚也失败 → `Degraded` + 告警，**不停机、不删除版本** |
| 人工 `proxyctl config rollback <id>` | 走同一流程（含校验与健康检查），同样以**重启**方式落地 |

**回滚必须复用同一套激活代码路径**，不允许有"快速回滚"旁路（否则回滚本身成为未验证路径）。唯一差异是回滚以 **restart** 而非 reload 落地。

> **为什么回滚必须 restart**：实测（R02 C5）在"解析通过但绑定失败 + `force=true`"的情形下，Mihomo 会进入 `mixed-port: 0` 的僵尸态，**后续多次合法 reload 都无法恢复**；只有重启进程能回到 known-good。

### D6. 与内核更新的分离（不可合并）

```text
Config Update  = 生成配置 → 校验 → 激活 → reload → 健康检查   （本 ADR）
Kernel Update  = 下载 → 校验 → 原子替换 → 重启 → 健康检查      （ADR-003 D2）
```

- 二者是**两条独立链路**、两个独立 Use Case、两个独立 API/CLI 命令。
- UI 不得出现单一的 "Update" 按钮（设计文档 §39）。
- 两者都遵循同一失败哲学：失败 → 保留/恢复 known-good。

### D7. 保留策略

- 版本数上限可配置（默认保留最近 N 份，含所有曾被激活过的版本）。
- **active 版本永不被清理**（REQ-CONFIG-010）。
- 清理是显式操作，且写审计。

---

## 3. Alternatives

| 方案 | 拒绝理由 |
|---|---|
| 原地编辑激活配置（`sed -i` 风格） | 违反不可变原则；崩溃/断电即损坏（ShellCrash 的教训，R08） |
| 只用单份 `.bak` 备份 | metacubexd/ShellCrash 的做法；无版本号/无 diff/无来源追溯，竞品已落后（C7/C8） |
| 用 SIGHUP reload | 无错误反馈（ADR-003 D2） |
| 把完整 YAML 存 SQLite | `AGENTS.md` 明确反对；且 diff/回滚语义变差 |
| 激活失败即停止 Mihomo"以保证一致性" | 违反核心不变量；正确做法是保留旧配置 |
| 依赖 `GET /configs` 回显做激活确认 | 字段不全（C4），会漏掉 `dns`/`rules` 差异 |

---

## 4. Consequences

### 4.1 正面

- 核心不变量被编码进唯一激活路径，可被 Application 测试直接覆盖（注入三步失败）。
- 支持 `list/show/validate/diff/activate/rollback`，且与 CLI/TUI/Web 共享同一实现。
- 审计与来源追溯天然（每份版本自带 source + checksum）。

### 4.2 负面 / 成本

- 需要 `-t` 作为外部进程调用（在 Infrastructure 层），增加一次进程启动开销（可接受，秒级）。
- 临时验证在 TUN 场景下不能真起 TUN（应跳过，只做 `-t` + 端口级验证），需明确文档化。
- 版本目录需要磁盘配额与清理策略。

### 4.3 必须遵守

```text
1. 激活只有一个代码路径；回滚复用同一路径（D5）
2. reload 必须带 JSON body（C2）；必须有 L4 端口检查（C11）
3. 失败不得停止 Mihomo、不得删除旧版本
4. 完整 YAML 只在文件系统；SQLite 只存元数据
5. 内核更新与配置更新不得合并为单一操作（D6）
6. 三层校验必须逐层可单独测试（REQ-CONFIG-005）
```

---

## 5. Evidence

- `docs/research/01-mihomo.md` §1（reload 语义、空 body 陷阱、SAFE_PATHS、`GET /configs` 字段限制、bind 失败不致命、geo fire-and-forget）
- `docs/research/02-mihomo-config.md`（`mihomo -t` 覆盖范围、目录/geodata 行为、schema 结论）
- `docs/research/08-shellcrash.md` §1（单份 `.bak`、自启锁死、覆盖式升级的教训）
- `docs/research/07-metacubexd.md` §1（单槽 `.bak` 回滚的能力上限）
- `docs/research/15-competitors.md` §1（"不可变版本库 + 任意历史回滚"是行业空白）
- `docs/research/09-linux-runtime.md` C10（数据目录与权限）
- 设计文档 §16、§17、§39–§42（原生命周期设计）
