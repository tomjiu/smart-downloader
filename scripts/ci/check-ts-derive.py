#!/usr/bin/env python3
"""契约类型守卫（#161 P2）：wire 结构体必须有 TS derive + #[ts(export)]。

背景：项目最初的 P0（UI 发 max_download_kb_s、后端要 down_kb_s → serde
静默丢弃 → 限速从未生效）的根因是「wire 契约无编译期类型」。ts-rs 生成
链路建立后，新增结构体漏挂 TS derive = 同构风险回归。本脚本在 CI 里
挡住这个方向。

规则（名字后缀近似 wire 契约，与既有命名约定一致）：
  1. struct 名以 Req / Out / Dto / Query 结尾
  2. 且 derive 含 serde Deserialize 或 Serialize（= 真正上 wire；
     纯内部结构如 ListQuery 无 serde，天然排除）
  3. 且非 #[cfg(feature)] 门控（门控结构体默认 feature 集编译不到，
     ts-rs export 测试不存在 → 走显式豁免清单 + 原因）
  → derive 必须含 TS，且属性必须含 #[ts(export ...)]，
    且 ui/src/lib/daemon/generated/<Name>.ts 必须存在（防「加了
    derive 忘跑生成」；与 ci.yml drift check 互为双向兜底）。

范围：core / provider / daemon 三 crate（gen:types 的导出集合）。
enum 不在本规则内（不带 Req/Out 后缀，由生成链路自身覆盖）。

用法：python3 scripts/ci/check-ts-derive.py  （仓库根执行；非零退出 = 违规）
"""

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCAN_GLOBS = [
    "crates/core/src/**/*.rs",
    "crates/provider/src/**/*.rs",
    "crates/daemon/src/**/*.rs",
]
GENERATED_DIR = REPO / "ui" / "src" / "lib" / "daemon" / "generated"

SUFFIX_RE = re.compile(r"(Req|Out|Dto|Query)$")

# 属性块 + struct 声明（跨行；pub / pub(crate) / 私有均扫）
STRUCT_RE = re.compile(
    r"((?:#\[[^\]]*\]\s*)+)" r"(?:pub(?:\(\w+\))?\s+)?struct\s+(\w+)"
)

SERDE_RE = re.compile(r"\b(?:serde::)?(?:Deserialize|Serialize)\b")
TS_RE = re.compile(r"\bTS\b")
TS_EXPORT_RE = re.compile(r"#\[ts\([^)]*export\b")
CFG_FEATURE_RE = re.compile(r'#\[cfg\([^)]*feature\s*=')

# 豁免清单：cfg(feature) 门控的 wire 类型。默认 feature 集编译不到 →
# ts-rs 导出测试不生成 → 本检查与 gen 流程均覆盖不到，显式列出并附原因；
# 待导出流程支持 feature 矩阵后收口（见 #161）。
ALLOWLIST = {
    "AddXunleiImportReq": "feature xunlei-import 隐含 bt（需 libtorrent 链接），默认 feature 集编译不到",
    "NasInstallReq": "feature nas（Linux-only），默认 feature 集编译不到",
    "NasTokenReq": "feature nas（Linux-only），默认 feature 集编译不到",
}


def check() -> int:
    violations = []
    scanned = 0
    for pattern in SCAN_GLOBS:
        for path in sorted(REPO.glob(pattern)):
            src = path.read_text(encoding="utf-8")
            for m in STRUCT_RE.finditer(src):
                attrs, name = m.group(1), m.group(2)
                if not SUFFIX_RE.search(name):
                    continue
                # 规则 2：必须 serde（不上 wire 的后缀同名结构不适用）
                if not SERDE_RE.search(attrs):
                    continue
                scanned += 1
                line = src[: m.start()].count("\n") + 1
                rel = f"{path.relative_to(REPO)}:{line}"

                # 规则 3：cfg(feature) 门控 → 豁免清单必须收录（带原因）
                if CFG_FEATURE_RE.search(attrs):
                    if name not in ALLOWLIST:
                        violations.append(
                            f"{rel} {name}: cfg(feature) 门控的 wire 类型未列入豁免清单"
                            f"（补 ALLOWLIST 条目 + 原因，或去门控）"
                        )
                    continue
                if name in ALLOWLIST:
                    violations.append(
                        f"{rel} {name}: 已无 feature 门控，应从豁免清单移除并补 TS derive"
                    )

                # 规则 4：TS derive + #[ts(export)] + 生成文件存在
                if not TS_RE.search(attrs):
                    violations.append(
                        f"{rel} {name}: wire 契约缺 TS derive"
                        f"（P0 同构风险：字段名漂移 UI 编译期不报错）"
                    )
                    continue
                if not TS_EXPORT_RE.search(attrs):
                    violations.append(f"{rel} {name}: 有 TS derive 但缺 #[ts(export)]（不生成 .ts）")
                    continue
                if not (GENERATED_DIR / f"{name}.ts").exists():
                    violations.append(
                        f"{rel} {name}: generated/{name}.ts 不存在"
                        f"——运行 `cargo test -p smart-dl-core -p smart-dl-provider"
                        f" -p smart-dl-daemon --lib export_bindings` 重新生成并提交"
                    )

    print(f"[check-ts-derive] 扫描 wire 契约结构体 {scanned} 个（含豁免 {len(ALLOWLIST)} 个）")
    if violations:
        print(f"\n[check-ts-derive] 违规 {len(violations)} 处：")
        for v in violations:
            print(f"  - {v}")
        return 1
    print("[check-ts-derive] OK")
    return 0


if __name__ == "__main__":
    sys.exit(check())
