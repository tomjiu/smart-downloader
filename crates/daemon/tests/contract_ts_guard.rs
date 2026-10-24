//! 契约类型守卫（#161 P2 → #171 收口）：wire 结构体必须 ts-rs 化。
//!
//! 背景：`max_download_kb_s` → `down_kb_s` 字段名事故（serde 静默丢弃、
//! 空操作返回 200、限速从未生效）的根因是请求结构体没有 TS 对应物——
//! 任何一方改名，另一边编译期毫无信号。#148 起逐个补 derive，#163 把
//! 「漏网」检查做成了 CI 里的 Python 脚本（scripts/ci/check-ts-derive.py，
//! 本 PR 移除），本测试是其测试化合并（吸收 #167 的方案，规则超集）：
//!
//! 1. **本地即拦**：随 `cargo test -p smart-dl-daemon` 运行（CI 四个特性
//!    组合 + 双平台无差别生效），不再依赖独立 CI step 与 Python 环境；
//! 2. **门控按 CI 矩阵判定**：cfg(feature) 门控 ≠ 一律豁免——CI 矩阵
//!    覆盖的特性（CI_COVERED_FEATURES）导出测试真的会跑，必须 TS 化
//!    （nas 即此例：`test daemon (ftp + nas)` 生成其绑定）；
//! 3. **登记簿精确匹配**：CI 未覆盖的门控结构体集合必须与
//!    GATED_UNCOVERED 完全一致（删 cfg / CI 补特性 → 测试逼你同步，
//!    豁免清单不会无声腐化）；
//! 4. **金丝雀 + 反空转**：#160 复盘的教训是「检查空转比没有检查更糟」
//!    （ts-rs 漂移检查曾恒净通过）。本测试自带已知结构体金丝雀与扫描
//!    目录存在性断言——解析逻辑失效 / 目录搬迁会直接红，而非静默绿。
//!    解析器健壮性同样按此标准实现：多行属性先归一化为单逻辑行（否则
//!    `#[derive(\n    TS\n)]` 会被逐行窗口漏扫），语义检查只看 `#[…]`
//!    行（否则 `/// …TS…` 文档注释会让 contains("TS") 假绿）。
//!
//! 规则（静态源码检查，与特性无关，任何特性组合下结果一致）：
//! - 名字以 `Req` / `Out` / `Dto` / `Query` 结尾，且 derive 含 serde
//!   （= 真正上 wire；纯内部后缀同名结构如 ListQuery 无 serde，天然排除）
//!   的结构体必须有 `#[derive(..., TS ...)]` 与 `#[ts(export ...)]`；
//! - 门控结构体：CI_COVERED_FEATURES 内同样必须有；未覆盖的登记在
//!   GATED_UNCOVERED（导出测试永不运行，生成文件将无人验证地腐化——
//!   显式不生成优于假守卫）；
//! - 有 `#[ts(export)]` 的结构体，`ui/src/lib/daemon/generated/<Name>.ts`
//!   必须已提交（防「本地生成了忘提交」——默认特性 job 的 git diff 漂移
//!   检查只覆盖默认特性，门控导出须靠本规则兜底）。
//!
//! 非目标（记录边界，防误解为疏漏）：
//! - enum 与不带契约后缀的结构体：由生成链路自身覆盖；
//! - 宏展开生成的结构体：静态扫描不可见（本仓无此形态）。
//!
//! 有效性自证（回滚必红）：临时去掉任一 derive（如 NasTokenReq）、删除
//! 任一生成文件、增删 GATED_UNCOVERED 条目，本测试必须失败——提交前
//! 已双向验证。
//!
//! 维护提醒：
//! - 变更 ci.yml 特性矩阵时须同步 CI_COVERED_FEATURES；
//! - 新增 wire 结构体后：`npm run gen:types` 重新生成并提交（门控结构体
//!   在 cargo 侧加对应 `--features` 组合执行）。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// ci.yml 跑过 `cargo test -p smart-dl-daemon --features …` 的特性
/// （rust job：ftp / nas / ftp,nas / sftp；bt-integration job：bt）。
const CI_COVERED_FEATURES: &[&str] = &["ftp", "nas", "sftp", "bt"];

/// CI 矩阵未覆盖、豁免 ts-rs 的门控结构体登记簿：(结构体名, 特性)。
/// 解除豁免三步：ci.yml 补该特性 → 删除本条 → 加 `#[derive(TS)]`。
const GATED_UNCOVERED: &[(&str, &str)] = &[("AddXunleiImportReq", "xunlei-import")];

/// 金丝雀（#160 教训）：解析器必须能找到这些已知结构体，覆盖各形态
/// （跨 crate / 四种后缀 / 门控+CI 覆盖+已 TS 化）。搬目录、改解析、拆
/// crate 而忘了同步本测试 → 这里先红，而不是静默漏扫。
const CANARIES: &[(&str, &str)] = &[
    ("daemon", "AddTaskReq"),        // 普通请求体
    ("daemon", "SettingsQuery"),     // Query 后缀（URL 参数）
    ("daemon", "ProviderStatusDto"), // Dto 后缀（响应）
    ("daemon", "ProbeOut"),          // Out 后缀（响应）
    ("daemon", "NasTokenReq"),       // 门控 + CI 覆盖（nas）+ 已 TS 化
    ("provider", "PwdReq"),          // daemon 之外的 crate
];

/// 扫描的 crate（= gen:types 的导出集合；core 当前无契约结构体，但纳入
/// 扫描面——将来新增即受守卫约束，目录消失则结构性断言变红）。
const SCAN_CRATES: &[&str] = &["core", "provider", "daemon"];

#[derive(Debug, Default)]
struct StructInfo {
    file: String,
    line: usize,
    name: String,
    feature: Option<String>,
    serde: bool,
    ts_derive: bool,
    ts_export: bool,
}

/// 把跨多行的 `#[…]` 属性合并为单逻辑行（`#[derive(\n  A, B\n)]` → 一行），
/// 其余行原样保留。语义检查因此对多行属性写法免疫。
fn normalize_attrs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    let mut depth: i32 = 0;
    for line in text.lines() {
        match pending.take() {
            None => {
                if line.trim_start().starts_with("#[") {
                    depth = line.matches('[').count() as i32 - line.matches(']').count() as i32;
                    if depth > 0 {
                        pending = Some(line.to_string());
                    } else {
                        out.push(line.to_string());
                    }
                } else {
                    out.push(line.to_string());
                }
            }
            Some(mut p) => {
                p.push(' ');
                p.push_str(line.trim());
                depth += line.matches('[').count() as i32;
                depth -= line.matches(']').count() as i32;
                if depth <= 0 {
                    out.push(p);
                } else {
                    pending = Some(p);
                }
            }
        }
    }
    out
}

/// 递归收集 .rs 文件。
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 解析 `struct` 定义逻辑行，返回名字（pub / pub(crate) / pub(super) / 私有）。
fn parse_struct_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("pub struct ")
        .or_else(|| t.strip_prefix("pub(crate) struct "))
        .or_else(|| t.strip_prefix("pub(super) struct "))
        .or_else(|| t.strip_prefix("struct "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// 从结构体定义行向上收集属性块（归一化后的逻辑行；上限 30 行），返回
/// `(语义属性, 完整窗口)`。窗口含 `#[…]` / `///` / `//` / 空行——doc 与
/// derive 常见顺序是「derive 在上、doc 贴 struct」，必须穿过 doc 才能取到
/// derive；但语义判定只用 `#[…]` 行，防文档注释里的 "TS" / "Deserialize"
/// 字样造成假绿。
fn attribute_block(lines: &[String], idx: usize) -> (String, String) {
    let mut window: Vec<&str> = Vec::new();
    let mut i = idx;
    while i > 0 && window.len() < 30 {
        let t = lines[i - 1].trim();
        if t.is_empty() || t.starts_with("#[") || t.starts_with("///") || t.starts_with("//") {
            window.push(t);
            i -= 1;
        } else {
            break;
        }
    }
    let full = window.join("\n");
    let attrs_only = window
        .iter()
        .filter(|l| l.starts_with("#["))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    (attrs_only, full)
}

/// `\b<word>\b` 近似（两侧非 [A-Za-z0-9_] 即词边界）。
fn has_word(hay: &str, word: &str) -> bool {
    let bytes = hay.as_bytes();
    let mut from = 0usize;
    while let Some(pos) = hay[from..].find(word) {
        let s = from + pos;
        let e = s + word.len();
        let ok_before = s == 0 || !(bytes[s - 1].is_ascii_alphanumeric() || bytes[s - 1] == b'_');
        let ok_after = e == bytes.len() || !(bytes[e].is_ascii_alphanumeric() || bytes[e] == b'_');
        if ok_before && ok_after {
            return true;
        }
        from = s + 1;
    }
    false
}

/// 形如 `#[cfg(feature = "nas")]` / `#[cfg(all(feature = "a", …))]`——
/// 取首个 feature（本仓无多 feature 复合门控的契约结构体）。
fn extract_feature(attrs: &str) -> Option<String> {
    let pos = attrs.find("feature = ")?;
    let after = &attrs[pos + 10..];
    let quote = after.find('"')?;
    let end = after[quote + 1..].find('"')? + quote + 1;
    Some(after[quote + 1..end].to_string())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// 扫描 SCAN_CRATES 三 crate 的 src（静态源码，与当前编译特性无关）。
fn scan_contract_structs() -> Vec<StructInfo> {
    let root = repo_root();
    let mut out = Vec::new();
    for crate_name in SCAN_CRATES {
        let src = root.join("crates").join(crate_name).join("src");
        assert!(
            src.is_dir(),
            "扫描目录不存在：{}（crate 拆分/搬迁后须同步本测试的 SCAN_CRATES）",
            src.display()
        );
        let mut files = Vec::new();
        collect_rs_files(&src, &mut files);
        assert!(
            !files.is_empty(),
            "扫描目录无 .rs 文件：{}（反空转，#160 教训）",
            src.display()
        );
        files.sort();
        for f in files {
            let Ok(text) = fs::read_to_string(&f) else {
                panic!("读取失败: {}", f.display());
            };
            let lines = normalize_attrs(&text);
            for (i, line) in lines.iter().enumerate() {
                let Some(name) = parse_struct_name(line) else {
                    continue;
                };
                if !(name.ends_with("Req")
                    || name.ends_with("Out")
                    || name.ends_with("Dto")
                    || name.ends_with("Query"))
                {
                    continue;
                }
                let (attrs, _full) = attribute_block(&lines, i);
                out.push(StructInfo {
                    file: f.strip_prefix(&root).unwrap_or(&f).display().to_string(),
                    line: i + 1,
                    name,
                    feature: extract_feature(&attrs),
                    serde: has_word(&attrs, "Deserialize") || has_word(&attrs, "Serialize"),
                    ts_derive: has_word(&attrs, "TS"),
                    ts_export: attrs.contains("#[ts(") && has_word(&attrs, "export"),
                });
            }
        }
    }
    out
}

/// 金丝雀：解析器健康度（#160「元测试空转」复盘的直接对策）。
#[test]
fn contract_guard_canaries() {
    let structs = scan_contract_structs();
    let found: BTreeSet<&str> = structs.iter().map(|s| s.name.as_str()).collect();
    let mut missing = Vec::new();
    for (crate_name, name) in CANARIES {
        if !found.contains(name) {
            missing.push(format!("{name}（{crate_name}）"));
        }
    }
    assert!(
        missing.is_empty(),
        "金丝雀结构体未被扫描到：{}。解析逻辑或目录结构已失效（搬目录 / 改命名约定 / \
         拆 crate 后须同步本测试的 SCAN_CRATES / parse_struct_name / CANARIES），\
         当前共扫到 {} 个契约后缀结构体",
        missing.join(", "),
        structs.len()
    );
}

#[test]
fn wire_structs_must_have_ts_derive() {
    let structs = scan_contract_structs();
    assert!(
        !structs.is_empty(),
        "扫描器空转：未发现任何契约结构体（金丝雀测试应已先行失败）"
    );

    // 只保留真正上 wire 的（derive 含 serde）；纯内部后缀同名结构排除。
    let wire: Vec<&StructInfo> = structs.iter().filter(|s| s.serde).collect();
    assert!(
        wire.len() >= 50,
        "wire 契约结构体仅 {} 个（预期 60+）：扫描面异常收缩，排查解析逻辑",
        wire.len()
    );

    let mut violations = Vec::new();
    let mut uncovered_found: BTreeSet<(String, String)> = BTreeSet::new();
    let registry: BTreeSet<(String, String)> = GATED_UNCOVERED
        .iter()
        .map(|(n, f)| (n.to_string(), f.to_string()))
        .collect();
    let gen_dir = repo_root().join("ui/src/lib/daemon/generated");

    for s in &wire {
        let must_ts = match &s.feature {
            None => true,
            Some(f) => {
                if CI_COVERED_FEATURES.contains(&f.as_str()) {
                    true // CI 覆盖的门控结构体：导出测试会跑，必须有
                } else {
                    uncovered_found.insert((s.name.clone(), f.clone()));
                    false // 未覆盖：登记簿校验在后面统一做
                }
            }
        };
        if must_ts && (!s.ts_derive || !s.ts_export) {
            violations.push(format!(
                "{}:{} `{}`（feature={:?}）缺 TS 契约：derive(TS)={} ts(export)={} —— 加 \
                 #[derive(serde::Deserialize, TS)] 与 #[ts(export)]，跑 `npm run gen:types` \
                 生成并提交（门控结构体在 cargo 侧加对应 --features）",
                s.file, s.line, s.name, s.feature, s.ts_derive, s.ts_export
            ));
        }
        // 声明了导出 → 生成文件必须已提交（漂移检查只覆盖默认特性组合）
        if s.ts_export {
            let gen = gen_dir.join(format!("{}.ts", s.name));
            if !gen.exists() {
                violations.push(format!(
                    "{}:{} `{}` 声明 #[ts(export)] 但 {} 不存在 —— 跑 `npm run gen:types` \
                     重新生成并提交（门控结构体加对应 --features）",
                    s.file,
                    s.line,
                    s.name,
                    gen.strip_prefix(repo_root()).unwrap_or(&gen).display()
                ));
            }
        }
    }

    // 登记簿与源码精确一致（防陈旧条目 / 防新门控静默漏登）
    for (name, feat) in uncovered_found.difference(&registry) {
        violations.push(format!(
            "新出现 CI 未覆盖的门控契约结构体 `{}`（feature={feat}）：加入 GATED_UNCOVERED \
             登记簿并注明原因，或给 CI 矩阵补该特性后加 TS derive",
            name
        ));
    }
    for (name, feat) in registry.difference(&uncovered_found) {
        violations.push(format!(
            "登记簿条目 `{}`（feature={feat}）已失效（源码中不存在或特性已被 CI 覆盖）——\
             删除该条目",
            name
        ));
    }

    assert!(
        violations.is_empty(),
        "契约类型守卫发现 {} 处违规：\n  - {}",
        violations.len(),
        violations.join("\n  - ")
    );
}
