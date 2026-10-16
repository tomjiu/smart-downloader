# 文档一致性审计报告 — docs/research/xunlei/ 状态类文档

> **审计性质**: 只读审计。未修改任何被审文档；本报告为新增产物。
> **审计范围**: 任务指定的 15 个状态类文档（含 3 个大型归档文件的头部区段）。
> **基准目录**: `E:\Code\ai\smart-downloader-latest`（以下路径均相对该目录）
> **日期分层基线**: 08-16 沙箱推断层 / 08-17 真实样本验证层 / 08-27 PHub v3 勘误层 / 08-30 跨平台层

---

## 0. 执行摘要

审计发现的核心问题**不是"两份文档在某事实上不一致"**，而是一个**两层分裂**：

| 层 | 时间 | 自称状态 | 实际状态 |
|---|---|---|---|
| L1 沙箱推断层 | 2026-08-16 | "研究到极限，暂停等样本" | **已被 08-17 真实样本推翻，但绝大多数文件未回改** |
| L2 真实样本层 | 2026-08-17 | "验证完成，格式已确认" | 正确，且是唯一权威描述 |
| L3 PHub v3 勘误层 | 2026-08-27 | "v2 公式作废" | 勘误横幅存在，但**文内正文仍保留被证伪叙述** |
| L4 跨平台层 | 2026-08-30 | 引用 D28/D36 决策 | 引用的决策**不在 DECISIONS.md 中** |

**统计（仅任务指定文档）**：
- 一致性问题总数：**24 项**
- 其中"**真矛盾**"（两份文档同时声称相反事实，无任何修订标记）：**9 项**
- 其中"**合法被取代但缺勘误横幅**"（后文档正确，前文档无标记，属缺陷）：**12 项**
- 其中"**文档内部自相矛盾**"（同一文件头尾冲突）：**3 项**

**最关键结论**：`.xlbt.cfg` magic 与 `.bt.xltd` 布局两项核心事实，在**只有 2 个文件**（`RESEARCH_STATE.md`、`spec_pending_validation.md`）中被修正；其余 **6 个同目录状态文档 + 824KB 归档**仍以 A 级/已破解口径断言被推翻的结论。`README.md` 作为**目录索引**仍是 08-16 口径，是误导面最大的单点。

---

## 1. `.xlbt.cfg` magic 矛盾：`XLBTCFG\x00` vs `XDLCTX\x00\x00`

### 1.1 主张 A：magic = `XLBTCFG\x00`（已被推翻，但仍存活于 6 个文件）

**`docs/research/xunlei/FINAL_REPORT.md`** — 标题即断言"已破解"，等级标 A：

```
74: ### 1.5 .xlbt.cfg 文件 magic 已破解 (A 级, 反汇编)
...
79: movabs rax, 0x47464354424c58        ; rax = "XLBTCFG\x00" (小端整数)
...
97: +0x00 (8B):  magic = "XLBTCFG\x00"
...
246:   magic:          b'XLBTCFG\x00'  (OK)
```
该文件另在 `L158`、`L384`（`.xlbt.cfg 文件格式可逆向?" → A 级: magic + 头部结构 + section 数组反汇编`）继续加固此错误结论，且**全文件 494 行无任何勘误/待验证横幅**。

**`docs/research/xunlei/OPEN_QUESTIONS.md`** — 以"已破解"列于 Q12 状态：

```
10: - ✅ 文件 magic 已破解 (A10: "XLBTCFG\x00")
```
标题 `L1: # Open Questions (Round 4 后)`，末尾 `L85: 2026-08-16 14:50 UTC+8`，**无勘误横幅**。

**`docs/research/xunlei/FINDINGS.md`** — 可行性评估表中列为 A 级：

```
52: | .xlbt.cfg magic | ✅ 已破解 (XLBTCFG) | A |
53: | .xlbt.cfg 头部结构 | ✅ 已破解 (40 字节 + sections) | A |
```
末尾 `L108: 2026-08-16 14:50 UTC+8`，**无勘误横幅**。

**`docs/research/xunlei/README.md`**（目录索引，误导面最大）：

```
12: 2. **迅雷 BT 占位文件不通用**:`.bt.xltd`(纯 piece 数据 sparse file,B 级)+ `.xlbt.cfg`(私有任务配置,头部 A 级、section 映射 C/D 级)。
65: | .xlbt.cfg 头部(40B + 20B entry) | ✅ A 级反汇编证据 |
67: | section_id → 内容映射 | ❌ C/D 级猜测(**禁止写死进生产代码**) |
```
同时 `L5: > **状态**: 研究完成 · PoC 合成验证通过 · 真实样本验证待用户提供` —— 与 08-17 已完成的真实样本验证直接冲突（见 §3）。

**`docs/research/xunlei/sample_collection_guide.md`**（审计范围外的种子文件，但仍是被引用入口）：

```
150:    - 验证 .xlbt.cfg magic = "XLBTCFG"
```

### 1.2 主张 B：magic = `XDLCTX\x00\x00`（真实样本 A 级，权威）

**`docs/research/xunlei/spec_pending_validation.md`**：

```
1: # .xlbt.cfg / .bt.xltd 格式规范 — 真实样本验证版 (A 级)
3: > **状态**: ✅ **已验证** — 基于真实样本 `C5AA149AE0776344A270EAFEE49FDADB43FF6097`
5: > **验证日期**: 2026-08-17
...
26: | 0x00 | 8B | magic | `XDLCTX\x00\x00` | **注意: 非旧推测的 "XLBTCFG"** |
...
39: **已推翻的旧假设** (反汇编推断 vs 真实样本):
40: - ~~magic = "XLBTCFG\x00"~~ → 真实 `XDLCTX\x00\x00`
41: - ~~block_count / block_size 头部字段~~ → 不存在; 4096 对齐只体现在 .bt.xltd 尺寸
42: - ~~section_count / section 数组 (20B/entry)~~ → 不存在; 真实结构为 tag-02/tag-04 TLV 记录
...
124: | V1 | cfg 结构 (magic + infohash 字段) | D | **A** | magic=XDLCTX\0\0, infohash@0x3c ASCII |
```

**`docs/research/xunlei/RESEARCH_STATE.md`**：

```
13: **研究完成 + 真实样本验证完成** (2026-08-17)。
...
21: | 项 | 旧推断 (反汇编) | 真实样本 (A 级) |
22: |---|---|---|
23: | cfg magic | "XLBTCFG\x00" | **"XDLCTX\x00\x00"** |
24: | cfg 结构 | 40B 头 + section 数组 (20B/entry) | **TLV 记录** (tag-02 int / tag-04 blob), infohash ASCII @0x3c |
25: | cfg 内容 | 含 piece 哈希表 + bitfield | **无** (32KB cfg 物理装不下 45KB 哈希; 231 个 20B blob 零匹配) |
```

### 1.3 判定

**类型：真矛盾（genuine contradiction）**，且是**未被修正的 stale claim**。

`FINAL_REPORT.md:74` 标题写"已破解 (A 级)"，与 `spec_pending_validation.md:40` 的"已推翻"在同一仓库中并存；两者都以 A 级自居，读者无法自行判别。`FINAL_REPORT.md` 是 README 明示的"最终报告(核心结论,分层证据)"（`README.md:26`），权威外观最强，实际已失效。

**未修正文件清单（`.xlbt.cfg` 相关）**：

| 文件 | 行号 | 断言 | 勘误横幅 |
|---|---|---|---|
| `FINAL_REPORT.md` | 74, 79, 97, 158, 246, 384 | magic = XLBTCFG，A 级"已破解" | ❌ 无 |
| `OPEN_QUESTIONS.md` | 10, 21 | "文件 magic 已破解" | ❌ 无 |
| `FINDINGS.md` | 13, 19, 52, 53 | "已破解 (XLBTCFG)" A 级 | ❌ 无 |
| `README.md` | 12, 65, 67 | "头部 A 级" / "40B + 20B entry A 级" | ❌ 无 |
| `sample_collection_guide.md` | 150 | 验证 magic = XLBTCFG | ❌ 无 |
| `xunlei_research_complete.md`（824KB 归档） | 116, 127, 759, 916, 934, 1083, 1641-1653, 3136, 4375, 5212, 5654, 5818, 5864, 6079, 6195, 6395 | 全套 XLBTCFG | ❌ 无 |

---

## 2. `.bt.xltd` 格式矛盾：sparse 纯 piece 数据 vs 文件位置镜像

### 2.1 主张 A：纯 piece 数据 sparse file，offset = `piece_index × piece_length`

**`FINAL_REPORT.md`**：

```
153: ### 2.1 .bt.xltd 是纯 piece 数据 sparse file (B 级)
...
160: 5. 知乎帖子 "文件大小比占用空间大10倍" 印证 sparse file 推断
161: 6. 没有任何私有头部写入逻辑被反汇编发现
162: 
163: **最可能**: `.bt.xltd` 直接按 `piece_index × piece_length` 偏移存储 piece 数据,使用 NTFS sparse file。
...
206: ### 3.2 .bt.xltd piece 数据按标准偏移 (C 级)
...
209: 推断 `offset = piece_index × piece_length`(标准 BT 规范),但**未直接验证**。
...
226: | H4: .xltd 即使含完整 piece 数据,也无法被 libtorrent 接续 | `.bt.xltd` 推断为纯 piece 数据 sparse file,可被 libtorrent 直接读 |
...
260: 合成 1GB sparse .bt.xltd 文件被探测器正确分析:
262: - 文件大小 1GB,实际占用 ~5MB (sparse ratio 100%)
263: - 16 个采样位置全部为 0 (sparse holes)
...
389: - ".bt.xltd 是纯数据" → 4 个独立证据(类名 + 字符串 + 无 magic + 知乎 sparse 帖)
```

**`FINDINGS.md`**：

```
17: - 选项 A: 直接是 piece 数据 sparse file,按 `piece_index * piece_length` 偏移存储
...
42: - 推断: offset = piece_index × piece_length (标准 BT 规范)
...
65: 1. **.bt.xltd 可能是纯 piece 数据 sparse file** (B 级)
66: 2. **piece 数据按 `piece_index * piece_length` 偏移存储** (C 级推断)
...
86: | 写 .bt.xltd → .part 转换 | 1 天 | 低 (如果是纯数据) |
```

**`OPEN_QUESTIONS.md`**：

```
22: - ⚠ 推断为纯 piece 数据 sparse file (按偏移存储)
51: - ✅ 推断: .bt.xltd 是纯数据,可被 libtorrent 读取 (F26)
```

**`README.md`**：

```
12: 2. **迅雷 BT 占位文件不通用**:`.bt.xltd`(纯 piece 数据 sparse file,B 级)...
66: | .bt.xltd 纯数据 sparse 布局 | ⚠ B 级(多证据支持,未真实验证) |
114: 4. zip 打包(bt.xltd 是 sparse,压缩后很小)提供给我
```

### 2.2 主张 B：文件位置镜像，offset = `p × plen − file_offset`，全量分配

**`spec_pending_validation.md`**：

```
78: ## 3. .bt.xltd 文件结构 (A 级, 已验证)
80: ### 核心模型: 文件的位置镜像
83: .bt.xltd 字节偏移 x  ≡ 目标文件字节偏移 x   (同一文件)
84: 大小 = ceil(file_size / 4096) × 4096       (4096 对齐, 样本双文件精确命中)
85: 无文件头 magic                              (首字节即 piece 数据)
86: 整文件预分配, 未下载区域零填充                (非 NTFS sparse: fsutil queryAllocRanges 显示全量分配)
...
89: ### piece 数据物理偏移公式 (V4, SHA1 验证 1866 命中 / 99.1%)
93: 内部 piece p 的 xltd 偏移 = p × plen − file_offset        (p 完全落在 f 内)
94: 边界 piece (跨多文件) 无法从单个 xltd 验证 — 设计内排除
...
105: ### 已推翻的旧假设
107: - ~~sparse hole 表达未下载~~ → 实际是全量分配 + 零填充
108: - ~~可能有文件头~~ → 无头, 尺寸公式铁证 (741,376 = ceil(740642/4096)×4096 等)
```

**`RESEARCH_STATE.md`**：

```
26: | .bt.xltd | 纯 piece 数据 sparse file | **文件位置镜像**: 无头, 尺寸=ceil(file/4096)*4096, 全量分配+零填充 |
27: | piece 偏移 | idx×piece_length | **p×plen−file_offset** (内部 piece), SHA1 命中 1866/1882=99.1% |
28: | 完成状态 | cfg bitfield | **xltd 零区** + torrent 哈希 SHA1 推导 |
```

### 2.3 判定

**类型：真矛盾**。且矛盾点是**可证伪的物理事实**（是否 sparse、是否全量分配），不是解释分歧。`spec_pending_validation.md:107` 明确写"~~sparse hole 表达未下载~~ → 实际是全量分配 + 零填充"，而 `FINAL_REPORT.md:263` 仍写"16 个采样位置全部为 0 (sparse holes)"，`README.md:114` 还让用户按"sparse、压缩后很小"操作，`sample_collection_guide.md:97,116` 同样。

**未修正文件清单（`.bt.xltd` 相关）**：

| 文件 | 行号 | 断言 | 勘误横幅 |
|---|---|---|---|
| `FINAL_REPORT.md` | 153, 160, 163, 209, 226, 260-263, 389, 401 | sparse + idx×plen | ❌ 无 |
| `FINDINGS.md` | 17, 42, 65, 66, 86 | sparse + idx×plen | ❌ 无 |
| `OPEN_QUESTIONS.md` | 22, 51 | sparse 按偏移 | ❌ 无 |
| `README.md` | 12, 66, 114 | "纯数据 sparse 布局" B 级 | ❌ 无 |
| `sample_collection_guide.md` | 97, 116, 152, 153, 184 | sparse + `piece_index × piece_length` | ❌ 无 |
| `xunlei_research_complete.md` | 233, 242-258, 421, 990-1000, 1046, 1063, 1097-1100, 1226, 1238, 1357-1406, 4587-4633, 5352-5418, 5831 | sparse + idx×plen | ❌ 无 |

> **次要矛盾（同族）**：`FINAL_REPORT.md:272` 写 "实际迅雷可能用 1MB 或 2MB piece…这是 .bt.xltd 与 BT piece hash 表的**唯一可能不兼容点**"，而 `spec_pending_validation.md:127` 已用 1866/1882=99.1% SHA1 命中证明 piece_length 与 torrent 完全一致，该"不兼容点"已被证伪。

---

## 3. "研究完成"状态矛盾：暂停 vs 已完成

### 3.1 各文档"自称日期" vs 标题/头部断言

| 文档 | 标题/头部断言 | 头部声称日期 | 文末最后更新 | 实际内容最晚日期 | 冲突 |
|---|---|---|---|---|---|
| `FINAL_REPORT.md` | `L1: # 迅雷 BT 占位文件独立逆向研究 - 最终报告` | `L8: > **研究周期**: 2026-08-16` | 无 | 08-16 | `L373`/`L485` 断言"研究已到极限" → 与 `RESEARCH_STATE.md:13` 冲突 |
| `RESEARCH_STATE.md` | `L1: # Research State - 最终状态 (真实样本验证完成)` | `L13` 2026-08-17 | 无 | 08-17 | 与 FINAL_REPORT/README/OPEN_QUESTIONS 冲突 |
| `OPEN_QUESTIONS.md` | `L1: # Open Questions (Round 4 后)` | — | `L85: 2026-08-16 14:50 UTC+8` | 08-16 | **列出已被回答的问题为 OPEN** |
| `NEXT_ACTION.md` | `L1: # Next Action - M9 集成完成，待端到端验证` | — | 无 | **08-27 深夜**（`L645-678`） | **标题 08-17 口径，正文续到 08-27，678 行** |
| `FINDINGS.md` | `L1: # Findings (Round 3 完成 + Round 4 新发现)` | `L108: 2026-08-16 14:50 UTC+8` | 同 | 08-16 | 未含 Round 5b 修正 |
| `DECISIONS.md` | `L1: # Decisions` | `L90: 2026-08-17 UTC+8` | 同 | 08-17 | 缺 D28/D29/D36（见 §5） |
| `spec_pending_validation.md` | `L1` 真实样本验证版 (A 级) | `L5` 2026-08-17 | `L159: 2026-08-17 UTC+8` | 08-17 | ✅ 自洽 |
| `README.md` | `L5: > **状态**: 研究完成 · PoC 合成验证通过 · 真实样本验证待用户提供` | `L3` 2026-08-16 迁移 | 无 | **08-30**（`L162,169,174`） | **头部 08-16 待验证，尾部 08-30 已完成** |
| `p2p_research_complete.md` | `L4` 状态已澄清 | `L3: 2026-08-17T11:32:24` | — | 08-27 勘误 | 头部日期 vs 勘误日期错位 |
| `p2p_research_complete_v1.md` | `L4: > **状态**: PHub body 加密格式未完全破解,HTTP 发送链路已完整逆向` | `L3: 2026-08-17T11:06:15` | — | 08-17 | **无勘误横幅**，状态描述已被 v3 取代 |
| `p2p_recon_complete.md` | `L4` 状态已澄清 | `L3: 2026-08-17T10:36:25` | — | 08-27 勘误 | 头部日期 vs 勘误日期错位 |
| `p2p_recon\RESEARCH_STATE.md` | `L1: # P2P 网络接入状态 - 最终` | 勘误 08-27 | — | 08-17 | **内部矛盾**（见 §4.3） |
| `p2p_recon\FINAL_REPORT.md` | `L7: > **研究周期**: 2026-08-17` | 08-17 | 无 | 08-17 | 无勘误横幅，结论被 `REVIEW.md §3` 部分降级 |
| `p2p_recon\PROGRESS_REPORT_v3.md` | `L4` 状态已澄清 | `L3: > **更新**: 2026-08-17` | — | 08-27 勘误 | 头部日期 vs 勘误日期错位 |
| `p2p_recon\REVIEW.md` | `L1: # P2P 网络侦察审查记录（2026-08-17）` | 08-17 | `L92` | **08-17 深夜** | ✅ 自洽（含 §7/§8 追加） |

### 3.2 核心真矛盾："研究到极限暂停" vs "验证完成"

**`FINAL_REPORT.md`**：

```
371: 按研究规则,这是"必须由用户提供文件"的情况(条件 B),允许暂停。
373: 但用户已说明无法上传文件,且沙箱网络限制使本地取样本不可行。**研究在当前条件下已到极限**。
...
485: 研究在当前条件下(无沙箱 BT 网络 + 无用户样本)已到极限。核心结论已通过 A 级证据锁定,工程实现路径已通过 PoC 验证可行。
...
490: 3. ⚠ 真实样本验证推迟到 v1 实施阶段(用户在自己的 Windows 上跑转换器)
492: 不需要等真实样本就可以开始 v1 实施 — 路径 A 完全独立,路径 D 可在用户拿到样本后边测边修。
```

**`RESEARCH_STATE.md`**：

```
9: 4. ✅ 是否能写"迅雷 → libtorrent 转换器" (真实样本验证完成)
13: **研究完成 + 真实样本验证完成** (2026-08-17)。
...
15: ## 真实样本验证结果 (Round 5b, 2026-08-17)
17: 用户提供真实任务样本 (audio-books-cjk, infohash C5AA149AE0776344A270EAFEE49FDADB43FF6097,
18: 2263 pieces @131072, ~83%)。`validate_xunlei_sample.py` V1-V8 **全绿**, 其中 3 项
19: 核心反汇编推断被**推翻并修正**:
```

**`README.md`**（同时含两个时代的口径）：

```
5: > **状态**: 研究完成 · PoC 合成验证通过 · 真实样本验证待用户提供
...
69: | 真实样本验证 | ⛔ 待用户提供(唯一外部依赖) |
...
105: 验证通过后:把 `spec_pending_validation.md` 中 C/D 级升级为 A 级,解锁转换器为正式工具。
107: ### 3. 采集真实样本(需要你做,5-10 分钟)
...
127: - **研究线程已关闭**:所有在当前环境可达的结论与实验均已完成(下称"在当前条件下完成")。
```
`L5`/`L69`/`L105`/`L107` 都假设"样本尚未到手"，而同一文件 `L162`/`L169`/`L174` 已在写 08-30 的 NAS 引擎实测与扫码定案。**这是文档内部的时间线断裂**。

**`OPEN_QUESTIONS.md`** — 已被回答的问题仍标 OPEN：

```
7: ### Q12 (修正). .xlbt.cfg 每个 section_id 对应什么内容?
9: **当前状态**: 部分回答
10: - ✅ 文件 magic 已破解 (A10: "XLBTCFG\x00")     ← 已被推翻
11: - ✅ 头部 40 字节结构已破解 (A11)                 ← 已被推翻
12: - ✅ section entry 20 字节结构已破解 (A12)        ← 已被推翻
13: - ❌ 每个 section_id 对应什么内容未确认
14: - ⚠ 需要真实文件样本做 hex 验证
...
16: ### Q11 (修正). .bt.xltd 文件二进制结构?
...
22: - ⚠ 推断为纯 piece 数据 sparse file (按偏移存储)   ← 已被推翻
23: - ❌ 真实文件 hex 验证未做                        ← 已完成 (V3/V4)
...
31: ### Q17 (新). 是否需要让用户提供真实样本?
33: **当前判断**: **是,需要**                           ← 样本已到,已验证
```

Q11/Q12/Q17 在 `RESEARCH_STATE.md:21-28` 与 `spec_pending_validation.md:120-131` 中**均已关闭为 A 级**（V1-V8），但本文件（`L85` 日期 2026-08-16）仍为 OPEN。**判定：stale claim，且因列举"已破解"内容而升级为真矛盾。**

### 3.3 `NEXT_ACTION.md` 标题 vs 正文（678 行）

```
1: # Next Action - M9 集成完成，待端到端验证
2: 
3: ## 状态: 转换器已集成进 btcore/daemon，单元测试通过
5: ### 2026-08-17 里程碑
```

但正文持续到：

```
118: ## 2026-08-25 战果清单（登录终结日）
146: ### ✅ xunlei-ffi 引擎带身份模式（2026-08-27 晚）
183: ### 🔬 ABI 修复取得决定性突破（2026-08-27 深夜，DLL 重新提取 + 完整反汇编）
253: ### ✅ XLBTTaskParamV2 已修复（2026-08-27 深夜，代码落地）
289: ### 🔬 真机验证进展（2026-08-27 深夜，DLL 可加载，XL_Init 受 server 启动阻塞）
365: ### ✅ 真机验证彻底通过（2026-08-27 深夜，最终）
645: ### 🔑 重大发现：官方 C++ SDK 已在本机落地（2026-08-27，纠正旧认知）
678: 需用户决策：是否有 open.xunlei.com 的 api_key 申请渠道/意愿。
```

**判定：文档内部真矛盾 + 标题 stale。** `L1` 声称的"M9 集成完成，待端到端验证"是 08-17 状态；`L3` 声称"单元测试通过"；而正文主体（约 `L118-678`，占全文 ~82%）是**迅雷登录态、xffi ABI 逆向、官方 SDK 落地**等与 BT 转换器 M9 无关的 08-24~08-27 内容。标题已不能概括正文，且未在标题或头部标注"滚动追加"。另有 `L647-648` 明确自陈旧文档已过时：

```
647: 此前多处文档（PUBLIC_INTEL_REPORT / xunlei_research_complete.md 等）把官方仓库
648: `xunlei-open/xunlei-dlsdk` 判定为「仅接入文档、无协议层、无头文件」——**该认知已过时/错误**。
```
这条自陈证明该目录确实存在未回改的过时文档，且**修正只写在 NEXT_ACTION.md 里，未回改源头文档**。

---

## 4. P2P 加密公式矛盾：v2 `MD5(seq‖cmd)` vs v3 RSA-wrapped

### 4.1 两个模型

**v2 模型**（`AES-ECB(MD5(cmd_id + seq), body)`，13 字节明文头）与
**v3 模型**（8B 头 + 4B ekey_size + 128B RSA-1024 密文包装随机 16B AES key + 4B aes_body_size + N 字节 AES-128-ECB 密文）。

### 4.2 勘误横幅存在性核查（逐文件）

| 文件 | 勘误横幅 | 位置 | 横幅内容 |
|---|---|---|---|
| `p2p_research_complete.md` | ✅ **有** | `L10: ## 勘误（2026-08-27）` | `L11-27` 完整说明 v2 局部作废 + v3 spec |
| `p2p_recon_complete.md` | ✅ **有** | `L9: ## 勘误（2026-08-27）` | `L10-26` 同上 |
| `p2p_recon/PROGRESS_REPORT_v3.md` | ✅ **有** | `L8: ## 勘误（2026-08-27）` | `L9-25` 同上 |
| `p2p_recon/RESEARCH_STATE.md` | ✅ **有** | `L3: ## 勘误（2026-08-27）` | `L5-20` 同上 |
| `p2p_research_complete_v1.md` | ❌ **无** | — | `L4` 仅写"PHub body 加密格式未完全破解" |
| `p2p_recon/PROGRESS_REPORT_v2.md` | ❌ **无** | — | `L4` 写"仍在逆向"；`L246` 仍写 `| AES 密钥派生未知 | ✅ 已知: MD5(header[0:4]+header[5:9]) |` |
| `p2p_recon/FINAL_REPORT.md` | ❌ **无** | — | 见 §4.4 |
| `xunlei_engine_research.md` | ✅ 有（审计范围外） | `L387` | 引用 v3 spec |

> **注意**：`xunlei_engine_research.md:387` 有勘误，但同文件 `L442` 仍写 `- \`AES-128 key = MD5(8_byte_header)\`，其中 8 字节 = \`cmd_constant(4B LE) + counter(4B LE)\``，未加"仅 XUDT"限定。

### 4.3 `p2p_recon/RESEARCH_STATE.md` 文档内部自相矛盾

横幅 `L3-20` 声明 v2 作废，但正文 `L28` 的小节标题仍写 **"已确认 (A 级)"**：

```
28: ## 已确认 (A 级)
29: 
30: ### PHub 包格式（v2 头假设，仅 XUDT/legacy 适用）
31: ```
32: [0:4]   cmd_id = 1 (uint32 LE)
...
36: [13:]   AES-ECB(MD5([0:4]+[5:9]), body)
37: ```
38: > ⚠️ 上式不适用于 PHub HTTP 生产包。生产包见上方「勘误」v3 spec。
39: 
40: ### AES key 派生（v2 公式，仅 XUDT/legacy 适用）
41: ```
42: key = MD5(cmd_id_bytes(4) + seq_bytes(4))  → 16 字节 AES-128 key
...
46: > ⚠️ 上式已被 v3 证伪。PHub HTTP 生产路径用 RSA-wrapped random AES key。
```

**判定：真矛盾（同一文件内）**。标题"已确认 (A 级)"与紧随其后的"已被 v3 证伪"直接冲突。同文件 `L26` 又写 `**逆向到 capstone 反汇编极限，需要更深层工具或真实抓包样本。**` —— 与 `REVIEW.md §8` 的"研究线正式关闭"、`§7` 的"pcap 实测路径不成立"冲突（该文件未收录 08-17 晚的抓包/内存 dump 闭环结论）。

### 4.4 `p2p_recon/FINAL_REPORT.md` 的加密结论未加勘误

```
18: 1. 加密方式: 迅雷 P2P 用 **AES-ECB**(自实现 XPF_AES* + rc4_handler) + **RC4 MSE**。AES 密钥内嵌在每条消息前 8 字节(PAM 2012 论文实证) — 这意味着要解密必须先理解消息结构,要理解消息结构必须先解密,**鸡生蛋问题**。
...
141: | `hub5p.sandai.net` | PHub | HTTP + AES-ECB | device_id | peer 发现 |
142: | `hub5btmain.sandai.net` | BT SHub | HTTP + AES-ECB | captcha_sign | BT 元数据查询 |
...
184: | **PHub (peer 发现)** | AES-ECB + HTTP | captcha_sign 派生 | 密钥内嵌消息头(PAM 2012) |
185: | **SHub (资源查询)** | AES-ECB + HTTP | captcha_sign 派生 | 同上 |
```

该文件 `L7: > **研究周期**: 2026-08-17`，**无勘误横幅**。其"PHub 用 AES-ECB + 密钥内嵌消息头"已被 08-27 v3 勘误取代（RSA-wrapped random key），但文件未更新。

**更严重**：同目录的 `REVIEW.md` 已将该报告的这些证据**明确降级**：

```
24: 汇报声称的以下"反汇编实锤"在产物合集中**无对应物**，且与已知事实冲突：
...
29: | `XPF_AES*` 6 函数 + `rc4_handler` 自实现 RC4/AES-ECB | 合集中搜不到这些符号（grep 0 命中）；命名风格可疑；"AES-ECB 密钥内嵌消息前 8 字节"无来源 | ⚠ 未证实。加密判定维持"私有加密存在但细节未确认"（B-），**不得写死 ECB 细节** |
30: | "PAM 2012 论文印证 AES-ECB" | 论文…公开摘要未提 AES-ECB 细节；无法核验 | ❌ 弃用该"印证"表述 |
59: **证据分级（维持既定决策，不变）**：以上全部为逆向 AI 中间产物——**B-/C 级参考，禁止引用为 A 级**。
```

而 `FINAL_REPORT.md:2.1` 的小节标题是 **"加密方式 — 决定性证据 (A 级)"**（`L30`）。**判定：真矛盾** —— `REVIEW.md:29` 说"不得写死 ECB 细节 / 未证实 B-"，`FINAL_REPORT.md:30` 说"A 级 决定性证据"。`FINAL_REPORT.md` 全文未引用 `REVIEW.md` 的降级结论。

### 4.5 `PROGRESS_REPORT_v3.md` 的 v2/v3 混装

```
3: > **更新**: 2026-08-17
4: > **状态**: PHub HTTP body 加密格式已澄清（v2 MD5 公式仅适用于 XUDT/legacy；PHub 生产路径为 RSA-wrapped random AES key）
...
31: ### 已确认 PHub 包格式 (A 级) — v2 头假设，仅 XUDT/legacy 适用
...
42: PHub body ([13:], AES-ECB 加密):
43:   AES key = MD5([0:4] + [5:9])  ← 8 字节 → 16 字节
...
48: > ⚠️ **上式不适用于 PHub HTTP 生产包**。生产包见上方「勘误」v3 spec。
...
108: ### PHub HTTP body 加密格式 — 已澄清（v3 spec）
110: v2 的 14 个 PoC 全部失败，原因已定位：**生产路径不使用 MD5(seq) 派生 AES key**。
113: 1. ✗ AES-ECB(MD5(cmd+seq), body) — 13B 明文头 + AES(body) — **仅 XUDT/legacy 适用**
```

判定：**有横幅、标注充分**，但 `L31` 仍保留"已确认 (A 级)"标题（与 `p2p_recon/RESEARCH_STATE.md:28` 同源缺陷）。`L3` 头日期 2026-08-17 与 `L8` 勘误日期 2026-08-27 **差 10 天**，头部"更新"未随勘误更新。

---

## 5. `DECISIONS.md` 陈旧性与 D28/D29/D36 缺失

### 5.1 `DECISIONS.md` 实际内容

```
77: ## D-2026-08-17-02 (P2P 侦察关闭)
...
88: ## 最后更新时间
89: 
90: 2026-08-17 UTC+8
```

文件共 90 行，含 5 个决策条目：
- `L5: ## D-2026-08-16-01 (Phase 1 完成时, 已被部分修正)` — 状态 `L13: ⚠ **部分作废**`
- `L17: ## D-2026-08-16-02 (Phase 2 Round 2, 修正后)`
- `L42: ## D-2026-08-16-03 (待决策)` — `L55: **状态**: 等待用户提供,或继续反汇编推测。`（**已被 D-2026-08-16-04/样本到手取代，未更新状态**）
- `L59: ## D-2026-08-16-04 (技术路线对比)`
- `L77: ## D-2026-08-17-02 (P2P 侦察关闭)` ← **最后一条**

### 5.2 D28 / D29 / D36 的引用方

`grep` 确认 **D28/D29/D36 在 `DECISIONS.md` 中出现 0 次**，只出现在 08-30 层文档：

**`CROSS_PLATFORM_UNIVERSAL_SOLUTION.md`**：

```
19: | **L3** | 加速层（VIP 加速通道、PHub/SHub/FreeDCDN 私有 P2P 协议自研） | 无通解 | 无 | ❌ 不可取得（协议未公开 + D28 决策） |
...
198: | 2 | DCDN/FreeDCDN 协议自研 | D28 决策原文（`docs/BACKLOG.md` D 段）：**「迅雷私有 P2P 加速引擎（D28：纯 libtorrent；且 PHub 离线不可实现、DCDN 接入协议未公开）」**——接入协议未公开，服务端依赖不可替代 |
```
另 `L87: | thunder:// 解码 | … （`base64("AA"+URL+"ZZ")`，§7.1 D36） |`

**`APP_COVERAGE_GAP_2026-08-30.md`**：

```
11: **…App 外围（UI、播放器、云盘同步盘、会员商店等）按 D28 决策不在项目范围内。**
49: | P2PBase/P2PFramework/P2PIO/P2PStat/P2PTarget/P2PCommonObjects.dll | 0.1.202.1754 | 私有 P2P 栈（PHub 前置层，PAM 2012 同源） | ❌ 协议未公开，D28 排除 | ❌ 永不做 |
96: | 2 | DCDN/FreeDCDN 协议自研 | 接入协议未公开（D28 原文） | `BACKLOG.md` D 段 |
133: 4. **范围边界**：App 外围（UI/播放器/同步盘/商店）按 D28 永不进入项目；…
```

**`CREDENTIAL_HUNTING.md`**：`L127: …（P2P 线，D28 排除内）`

### 5.3 判定

**类型：编号体系冲突（真矛盾）+ 权威源错位。**

三个 08-30 文档一致地把 `D28`/`D36` 归因到 **`docs/BACKLOG.md` D 段**（`CROSS_PLATFORM_UNIVERSAL_SOLUTION.md:198` 明写"`D28` 决策原文（`docs/BACKLOG.md` D 段）"），而**不是** `docs/research/xunlei/DECISIONS.md`。因此：

1. **`DECISIONS.md` 不记录 D28、D29、D36，也不记录任何 `D<n>` 两位编号式决策** —— 该文件只使用日期式编号 `D-YYYY-MM-DD-NN`。
2. `DECISIONS.md` 头部无"本文件已废弃/已被 `BACKLOG.md` 取代"的指针，`L90: 2026-08-17 UTC+8` 之后**17 天无更新**，却仍是本目录唯一名为 "Decisions" 的文件。
3. **存在两套并存且互不引用的决策编号体系**（`D-2026-MM-DD-NN` vs `D<n>`），读者按目录直觉查找会落空。08-30 文档从未提及 `DECISIONS.md`。
4. P2P 相关决策实际被分裂到两处：`DECISIONS.md:77` 的 `D-2026-08-17-02`（接受 P3-C）与 `BACKLOG.md` 的 `D28`（排除 PHub/DCDN）——`DECISIONS.md:79` 写"接受逆向 AI 侦察的 P3-C 结论 —— 放弃接入迅雷私有 P2P 网络"，`CROSS_PLATFORM_UNIVERSAL_SOLUTION.md:198` 写 `D28：纯 libtorrent；且 PHub 离线不可实现`，两者语义重叠但编号不同、互不交叉引用。

**判定：`DECISIONS.md` 是 stale 文档**，其"最后更新时间 2026-08-17"具有误导性——它暗示该日期之后无决策产生，而实际上 `D28`/`D36` 等更晚决策存在于另一文件。

---

## 6. 其他明确矛盾与陈旧断言

### 6.1 `README.md` 内部时间线断裂（真矛盾）

已在 §3.2 列出。补充：`README.md:40` 描述 `p2p_recon/` 为"P2P 侦察中间产物补齐(08-17 晚,见 REVIEW §6)"，而 `p2p_recon/REVIEW.md` 实际有 §1–§8（`L92` 为 §8 内存转储实证，明确写 `**研究线正式关闭**`）。`README.md` 未更新为 §8，也未反映"研究线关闭"与自身 `L127` "研究线程已关闭" 的重叠表述。

### 6.2 `sample_collection_guide.md` 要求的操作基于已证伪前提（真矛盾，操作性风险）

```
97: **压缩后预期大小**: 远小于下载文件本身(因为 .bt.xltd 是 sparse 文件,压缩后会很小)
116: - `.bt.xltd` 大小应**等于下载任务的完整大小**(sparse 文件,size = total_size,但实际占用 < size)
152:    - 验证 .bt.xltd 是否纯数据 sparse
153:    - 验证 piece 偏移公式 = piece_index × piece_length
184: - .bt.xltd 即使是 sparse 文件,zip 压缩后应该很小
```

`spec_pending_validation.md:86` 已证伪：`整文件预分配, 未下载区域零填充 (非 NTFS sparse: fsutil queryAllocRanges 显示全量分配)`。**后果**：`L114`/`L184` 承诺的"压缩后很小"不成立（真实样本为全量分配），会误导用户按错误预期判断样本是否正常。`README.md:114: 4. zip 打包(bt.xltd 是 sparse,压缩后很小)提供给我` 同源。

### 6.3 `FINAL_REPORT.md` 的产出物路径指向已不存在的沙箱（stale）

```
462: | 路径 | 内容 |
464: | `/home/z/my-project/research/RESEARCH_STATE.md` | 完整研究状态 |
...
481: | `/home/z/my-project/download/xunlei_independence_analysis.md` | 上一份报告 (Round 2 前) |
```
`p2p_recon/REVIEW.md:32` 已指出该路径问题的根因：

```
32: **根因提示**：逆向 AI 沙箱路径（/home/z/...）的中间产物未随汇报落盘（产出物清单中 FINAL_REPORT/xbtpackage_vtables.json 等缺失）；汇报中的部分推导可能是渲染/幻觉。**结论采纳、细节存疑**。
```
`p2p_recon/FINAL_REPORT.md:460-463` 同样列出 `/home/z/my-project/research/p2p_recon/*` 路径。这些路径在本仓库不存在（实际位于 `docs/research/xunlei/...`），属 stale 索引。

### 6.4 `p2p_recon/RESEARCH_STATE.md` 与 `REVIEW.md` 的"下一步"冲突（真矛盾）

`p2p_recon/RESEARCH_STATE.md`：

```
72: ## 下一步选项
74: 1. **用户用 Wireshark 抓 pr-phub.sandai.net:80 的 HTTP POST body** — 在 Windows 上跑迅雷下载一个 BT 任务,用 Wireshark 过滤 `host pr-phub.sandai.net && http.request.method == POST`,导出 HTTP body 给我。1 个真实包就能破解整个格式。
```

`p2p_recon/REVIEW.md`：

```
69: - **判定**：新版迅雷 PHub 走 **443 TLS**（"POST / HTTP/1.1 明文"模板属旧版/其他路径）；body 在 TLS 密文内，应用层还有 ParamStream/AES（§3 未证实项，格式未破）。
70: - **结论**：**建议的"抓 :80 POST body"路径实测不成立**。外部抓包拿不到明文 body（传输 TLS + 应用双层加密；中间人需绕过迅雷证书校验/SSL pinning，成本远超收益）。
71: - **闭环**：P3-C（放弃接入）获实测反面支撑…**本线到此为止，不再深入**。
```

**判定：真矛盾 + 陈旧"下一步"**。`RESEARCH_STATE.md:74` 建议的路径已被 `REVIEW.md:70` 用 pktmon 实测证伪（80 端口 0 条流），但 `RESEARCH_STATE.md` 未回改。同理 `PROGRESS_REPORT_v3.md:142-145` 仍推荐"Frida hook / Wireshark 抓 pr-phub.sandai.net:80"（`L145`），并已被 `REVIEW.md:70` 否定。

### 6.5 `REVIEW.md` 头部 vs 正文尾部（内部时间线）

```
1: # P2P 网络侦察审查记录（2026-08-17）
```
但文件含 `L63: ## 7. 真实抓包实测（2026-08-17 晚，闭环验证）` 与 `L76: ## 8. 内存转储实证（2026-08-17 深夜，A 级升级）`。标题日期与正文时段（晚/深夜）无冲突，`L92` 结论 `**研究线正式关闭**` 与 `REVIEW.md:9` 的 `接受 P3-C` 一致。**判定：自洽，非缺陷**（此处如实标注以区分）。

### 6.6 `REVIEW.md §6` 的"新物证"与 `§3` "禁止引用"的张力（已由文件自身声明消解）

```
51: - `xbtpackage_vtables.json` / `phub_shub_cmd_analysis.json`（§3 审查时"搜不到"的符号表——现在物证在库，可复核）
...
59: **证据分级（维持既定决策，不变）**：以上全部为逆向 AI 中间产物——**B-/C 级参考，禁止引用为 A 级**。§3 的三条"禁止引用"结论维持：message_id 重排表弃用、XPF_AES/RC4 未证实、PAM 论文 AES-ECB 印证弃用。新物证可复核 §3 存疑点，但**不得据此升级**；A 级需要真实抓包（Wireshark/pktmon 路径，用户侧可选）。
```
`L59` 已显式声明取证等级不变。**判定：合理，非矛盾** —— 但注意 `p2p_recon/FINAL_REPORT.md` 从未引用此降级，构成 §4.4 的实际矛盾。

### 6.7 `p2p_research_complete_v1.md` 状态行未随 v2/v3 更新

```
3: > **生成时间**: 2026-08-17T11:06:15.946558
4: > **状态**: PHub body 加密格式未完全破解,HTTP 发送链路已完整逆向
5: > **关键阻碍**: QAClient 协议层对 body 做了额外封装
```
对比 `p2p_research_complete.md:4`（v2，已含 v3 状态与勘误横幅）与 `p2p_recon_complete.md:4`（10:36 版，同样已更新）。**`v1` 是唯一头部状态未更新的合集**，且无勘误横幅。三者时间序为 10:36 → 11:06 → 11:32，但仅中间那版（11:06）状态最旧 —— **三份合集的状态字段与时间序不一致**。

### 6.8 `xunlei_research_complete.md`（824KB 归档）自身头尾矛盾

```
3: > **生成时间**: 2026-08-16T14:19:08.416897
91: > **状态**: ⚠ **未验证** — 所有字段均为反汇编推断,无真实样本验证
```
但同一文件 `L438: ## 最后更新` 之后的内容与 `L845: > **研究周期**: 2026-08-16`，全文（`L116`、`L759`、`L1641`）以 A 级断言 `magic = "XLBTCFG\x00"`。`L91` 的"未验证"与 `L116` 表格中标注的 **`A`** 级自相矛盾：

```
116: | 0x00 | 8B | `magic` | byte[8] = `"XLBTCFG\x00"` | **A** | `movabs rax, 0x47464354424c58` @ 0x1802cbd74 (写) + `cmp rcx, rax` @ 0x18020c0fc (读校验) |
127: - `magic` 严格 `"XLBTCFG\x00"`,任何不匹配直接拒绝加载
```
**判定：文件内部真矛盾（"未验证" vs "A 级"），且内容已被 08-17 样本推翻。**

### 6.9 `README.md` 的验收门槛表与 `RESEARCH_STATE.md` 冲突

```
README.md:65: | .xlbt.cfg 头部(40B + 20B entry) | ✅ A 级反汇编证据 |
README.md:69: | 真实样本验证 | ⛔ 待用户提供(唯一外部依赖) |
```
vs
```
RESEARCH_STATE.md:24: | cfg 结构 | 40B 头 + section 数组 (20B/entry) | **TLV 记录** (tag-02 int / tag-04 blob), infohash ASCII @0x3c |
```
`README.md:65` 把已推翻的"40B + 20B entry"标为"✅ A 级反汇编证据" —— **与 `spec_pending_validation.md:42`（`~~section_count / section 数组 (20B/entry)~~ → 不存在`）直接冲突**。

---

## 7. 汇总表：按缺陷类型分类

### 7.1 真矛盾（genuine contradiction）— 双方均在场，均无消解

| # | 主题 | 主张方 | 反对方 | 判定 |
|---|---|---|---|---|
| C1 | `.xlbt.cfg` magic | `FINAL_REPORT.md:74,79,97` / `OPEN_QUESTIONS.md:10` / `FINDINGS.md:52` / `README.md:65` | `spec_pending_validation.md:26,40` / `RESEARCH_STATE.md:23` | 真矛盾，反对方正确（真实样本 A 级） |
| C2 | `.bt.xltd` 布局 | `FINAL_REPORT.md:153,163,209,263` / `FINDINGS.md:42,66` / `OPEN_QUESTIONS.md:22` / `README.md:66` | `spec_pending_validation.md:83-108` / `RESEARCH_STATE.md:26-27` | 真矛盾，反对方正确 |
| C3 | 研究状态 | `FINAL_REPORT.md:373,485,490` / `README.md:5,69` / `OPEN_QUESTIONS.md` 全体 | `RESEARCH_STATE.md:13` / `spec_pending_validation.md:3` | 真矛盾，反对方正确 |
| C4 | P2P 加密等级 | `p2p_recon/FINAL_REPORT.md:30`（A 级 决定性证据） | `p2p_recon/REVIEW.md:29`（未证实 B-，不得写死 ECB 细节） | 真矛盾，审查方正确 |
| C5 | PHub v2 公式等级 | `p2p_recon/RESEARCH_STATE.md:28`（已确认 A 级） | 同文件 `L46`（已被 v3 证伪） | 文件内真矛盾 |
| C6 | .xlbt.cfg 头部结构 | `README.md:65`（40B + 20B entry ✅A 级） | `spec_pending_validation.md:42`（不存在） | 真矛盾 |
| C7 | 抓包路径可行性 | `p2p_recon/RESEARCH_STATE.md:74` / `PROGRESS_REPORT_v3.md:145`（抓 :80 POST body） | `REVIEW.md:70`（实测不成立） | 真矛盾，实测方正确 |
| C8 | 归档自评等级 | `xunlei_research_complete.md:91`（未验证） | 同文件 `L116`（A 级） | 文件内真矛盾 |
| C9 | DECISIONS 编号体系 | `DECISIONS.md`（`D-2026-MM-DD-NN`，止于 08-17） | `CROSS_PLATFORM_UNIVERSAL_SOLUTION.md:198`（`D28`@`BACKLOG.md`） | 编号体系冲突 + 权威源错位 |

### 7.2 合法被取代但缺勘误横幅（stale claim，缺陷但非矛盾）

| # | 文件 | 行号 | 应加横幅内容 |
|---|---|---|---|
| S1 | `FINAL_REPORT.md` | 全文（494 行） | 08-17 真实样本已推翻 §1.5/§1.6/§2.1/§3.2；头部应加勘误 |
| S2 | `FINDINGS.md` | 13, 19, 42, 52, 65-66 | 同上 |
| S3 | `OPEN_QUESTIONS.md` | 10, 11, 12, 22, 23, 33 | Q11/Q12/Q17 已关闭 |
| S4 | `README.md` | 5, 12, 65, 66, 67, 69, 105, 114 | 状态行 + 门槛表 |
| S5 | `sample_collection_guide.md` | 97, 116, 150, 152, 153, 184 | sparse/magic 前提已证伪 |
| S6 | `p2p_research_complete_v1.md` | 4 | v2/v3 状态 |
| S7 | `p2p_recon/PROGRESS_REPORT_v2.md` | 4, 246 | PHub v2 键派生 |
| S8 | `p2p_recon/FINAL_REPORT.md` | 18, 141-142, 184-185 | v3 spec + `REVIEW.md §3` 降级 |
| S9 | `xunlei_research_complete.md` | 116, 127, 759, 1641-1653 等 | 全套 XLBTCFG |
| S10 | `xunlei_engine_research.md` | 442 | `AES-128 key = MD5(8_byte_header)` 未限"仅 XUDT" |
| S11 | `NEXT_ACTION.md` | 1, 3 | 标题/状态仍 08-17 |
| S12 | `DECISIONS.md` | 42-55, 88-90 | D-2026-08-16-03 状态未更新；缺 08-17 后全部决策 |

### 7.3 对照：正确的修订（应作为回改模板）

| 文件 | 行号 | 属性 |
|---|---|---|
| `spec_pending_validation.md` | 1-7, 26, 39-42, 78-108, 120-134, 151-159 | ✅ 头部状态 + 逐条"已推翻的旧假设" + 变更日志 |
| `RESEARCH_STATE.md` | 1, 13, 15-28, 43-47 | ✅ "旧推断 vs 真实样本"对照表 |
| `p2p_research_complete.md` | 10-27 | ✅ 顶部独立勘误节 + 指向 v3 spec |
| `p2p_recon/PROGRESS_REPORT_v3.md` | 8-25, 108-132 | ✅ 勘误节 + 文内 ⚠ 回引 |

---

## 8. 修复优先级建议（未执行，仅建议）

1. **P0 — `README.md`**：目录索引，是读者第一入口。`L5` 状态行、`L12` 摘要、`L65-69` 门槛表全部为 08-16 口径，与同目录 08-17 权威文档冲突。**误导面最大。**
2. **P0 — `FINAL_REPORT.md`**：文件名与 README `L26` 定位（"最终报告(核心结论,分层证据)"）赋予其最高权威外观，但 §1.5/§1.6/§2.1/§3.2 全为已推翻内容。需顶部加勘误横幅并逐节打 `~~删除线~~` + 修正指向（照 `spec_pending_validation.md:39-42` 模板）。
3. **P1 — `sample_collection_guide.md`**：含**可执行的操作指导**（"压缩后很小"、"验证 offset = piece_index × piece_length"），错误前提会导致用户误判样本。操作类文档应为最高修复优先级之一。
4. **P1 — `OPEN_QUESTIONS.md`**：应整体重写或标记"已被 Round 5b 关闭"，否则"P0 阻塞最终结论"标题会持续误导排期。
5. **P1 — `NEXT_ACTION.md`**：拆分。`L1-116`（M9/转换器）与 `L118-678`（登录态/ffi ABI/官方 SDK）是两条不同工作线，标题应分别反映；或加"滚动追加区"分隔。
6. **P2 — `DECISIONS.md`**：加头部指针至 `docs/BACKLOG.md`，说明编号体系差异；或补录 `D-2026-08-17-03` 起的决策。
7. **P2 — `p2p_recon/RESEARCH_STATE.md`**：`L28` 标题"已确认 (A 级)"改为"v2 头假设（仅 XUDT/legacy，见勘误）"；`L72-78` "下一步"补 `REVIEW.md:70` 的证伪结论。
8. **P2 — `p2p_recon/FINAL_REPORT.md`**：`L30` "A 级 决定性证据"降级并回引 `REVIEW.md §3`。
9. **P3 — `xunlei_research_complete.md`**：824KB 归档。建议**不改正文**（属历史归档，改动会破坏可追溯性），仅加**顶部注记**："本文档为 08-16 沙箱推断快照，其中 .xlbt.cfg magic 与 .bt.xltd 布局已被 08-17 真实样本推翻，权威版本见 `spec_pending_validation.md`"。这比逐条回改更符合归档语义。

---

## 9. 审计方法与边界声明

**已完整阅读**（任务指定）：`FINAL_REPORT.md`(494)、`FINDINGS.md`(108)、`RESEARCH_STATE.md`(53)、`OPEN_QUESTIONS.md`(85)、`NEXT_ACTION.md`(678)、`DECISIONS.md`(90)、`spec_pending_validation.md`(159)、`README.md`(174)、`p2p_recon/RESEARCH_STATE.md`(78)、`p2p_recon/FINAL_REPORT.md`(465)、`p2p_recon/REVIEW.md`(94)。

**按指定范围部分阅读**：`p2p_research_complete.md` 前 80 行 / 全文 7971 行；`p2p_recon_complete.md` 前 80 行 / 全文 7465 行；`p2p_research_complete_v1.md` 前 60 行 / 全文 7994 行；`p2p_recon/PROGRESS_REPORT_v3.md` 前 60 行 + `L100-145` / 全文 145 行。

**全局 `grep` 覆盖**（补足未逐行阅读的大文件）：`XLBTCFG|XDLCTX`、`sparse|位置镜像|全量分配`、`MD5\(|RSA-wrapped`、`D28|D29|D36`、`勘误`、`M9|已到极限|研究完成`、日期与状态行模式。因此 §1.3/§2.3 中对大文件的"未修正"判定基于 grep 全量命中（含行号），非抽样。

**边界**：
- `xunlei_research_complete.md` 中除 grep 命中的行外未逐行阅读；§6.8 的"文件内部矛盾"仅基于 `L91` 与 `L116` 两处直接引用。
- 本报告**未做只读之外的任何文件操作**；`_AUDIT_CONTRADICTIONS.md` 为本次审计新增产物，未改动任何被审文档。
- 文中所有行号对应审计时点文件状态；若后续回改，行号会漂移。
