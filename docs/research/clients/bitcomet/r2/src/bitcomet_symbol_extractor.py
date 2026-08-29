"""
bitcomet_symbol_extractor.py — BitComet 二进制符号自动分析器
============================================================

复现我们整个逆向工程工作流的工具脚本。

逆向流程 (本脚本完整覆盖):
1. dpkg-deb -R 解压 .deb 文件
2. file / readelf 确认二进制类型
3. nm -C 提取 demangled 符号 (关键: BitComet 未 strip + 带 debug_info)
4. 分类符号到命名空间
5. 提取字符串特征 (URL / API 端点 / 配置项)
6. 与 qBittorrent 源码对比
7. 输出 Markdown 分析报告

使用方式:
    python3 bitcomet_symbol_extractor.py --deb BitComet-2.21.2-x86_64.deb \\
        --qbittorrent-src ./qbittorrent_src \\
        --output report.md

关键产物:
- symbols_all.txt       所有 demangled 符号
- namespaces.txt        所有 C++ 命名空间
- bitcomet_specific.txt BitComet 独有符号 (Core_*, BitComet_*, BC_*)
- api_endpoints.txt    从 strings 提取的 /api/ 端点
- config_keys.txt      从 strings 提取的 enable_*/disable_* 配置项
- module_stats.csv     各 Core_* 模块的符号计数

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import argparse
import csv
import os
import re
import shutil
import subprocess
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple


# -----------------------------------------------------------------------------
# 工具: 调用系统二进制工具
# -----------------------------------------------------------------------------

class Toolchain:
    """封装 binutils 工具链."""

    def __init__(self):
        for tool in ("nm", "objdump", "strings", "file", "readelf"):
            if not shutil.which(tool):
                raise RuntimeError(f"required tool not found: {tool}")
        self.nm = shutil.which("nm")
        self.strings = shutil.which("strings")
        self.file = shutil.which("file")
        self.readelf = shutil.which("readelf")
        self.objdump = shutil.which("objdump")

    def file_type(self, path: str) -> str:
        return subprocess.check_output([self.file, path], text=True).strip()

    def readelf_dynamic(self, path: str) -> List[str]:
        out = subprocess.check_output([self.readelf, "-d", path], text=True,
                                       stderr=subprocess.DEVNULL)
        return [l for l in out.splitlines() if "NEEDED" in l]

    def nm_demangle(self, path: str) -> List[str]:
        """nm -C 输出每行: 'addr T symbol_demangled'."""
        try:
            out = subprocess.check_output([self.nm, "-C", path], text=True,
                                            stderr=subprocess.DEVNULL)
            return out.splitlines()
        except subprocess.CalledProcessError as e:
            print(f"nm failed: {e}", file=sys.stderr)
            return []

    def strings_extract(self, path: str) -> List[str]:
        try:
            out = subprocess.check_output([self.strings, path], text=True,
                                            stderr=subprocess.DEVNULL)
            return out.splitlines()
        except subprocess.CalledProcessError:
            return []


# -----------------------------------------------------------------------------
# 主分析器
# -----------------------------------------------------------------------------

@dataclass
class AnalysisResult:
    binary_path: str
    file_type: str
    needed_libs: List[str] = field(default_factory=list)
    symbol_count: int = 0
    bitcomet_symbol_count: int = 0
    namespaces: Set[str] = field(default_factory=set)
    core_module_stats: Dict[str, int] = field(default_factory=dict)
    api_endpoints: List[str] = field(default_factory=list)
    config_keys: List[str] = field(default_factory=list)
    bitcomet_urls: List[str] = field(default_factory=list)
    unique_classes: List[str] = field(default_factory=list)


class BitCometSymbolExtractor:
    """完整逆向流程."""

    # BitComet 独有的命名空间前缀
    BITCOMET_PREFIXES = (
        "Core_", "BitComet_", "BC", "CtrlBitComet", "View_",
        "AppUtil", "AutoUpdate", "Dialog", "CommonGUI", "Common_",
    )

    # 模块统计正则
    CORE_MODULE_RE = re.compile(r"^(Core_[A-Za-z]+)::")

    # API 端点正则
    API_RE = re.compile(r"^/api/[a-z][a-z_/-]+$")

    # 配置项正则
    CONFIG_RE = re.compile(r"^(enable|disable)_[a-z_]+$")

    # URL 正则
    URL_RE = re.compile(r"https?://[a-zA-Z0-9.-]+\.bitcomet\.com[^\s]*")

    # 唯一类正则 (用于识别 BitComet 独有设计)
    UNIQUE_CLASS_RE = re.compile(
        r"^(?:Core_[A-Za-z]+::|BitComet_[A-Za-z]+::|Ctrl[A-Z][A-Za-z]+::)"
        r"([A-Z][A-Za-z0-9_]+)::"
    )

    def __init__(self, deb_path: str, output_dir: str,
                 qbt_src: Optional[str] = None,
                 toolchain: Optional[Toolchain] = None):
        self.deb_path = deb_path
        self.output_dir = Path(output_dir)
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.qbt_src = Path(qbt_src) if qbt_src else None
        self.tools = toolchain or Toolchain()
        self.extract_dir = self.output_dir / "extracted"
        self.symbols_dir = self.output_dir / "symbols"
        self.symbols_dir.mkdir(exist_ok=True)

    def run(self) -> AnalysisResult:
        """执行完整分析流程."""
        print("==> [1/7] Extracting .deb...")
        binary_path = self._extract_deb()
        print(f"    binary: {binary_path}")

        print("==> [2/7] Identifying binary type...")
        file_type = self.tools.file_type(binary_path)
        print(f"    type: {file_type}")
        needed = self.tools.readelf_dynamic(binary_path)

        print("==> [3/7] Extracting demangled symbols (this may take a minute)...")
        symbols = self.tools.nm_demangle(binary_path)
        print(f"    total symbols: {len(symbols)}")

        print("==> [4/7] Extracting strings...")
        strings = self.tools.strings_extract(binary_path)
        print(f"    total strings: {len(strings)}")

        print("==> [5/7] Categorizing symbols...")
        bitcomet_syms = self._filter_bitcomet_symbols(symbols)
        namespaces = self._extract_namespaces(symbols)
        core_stats = self._count_core_modules(symbols)
        unique_classes = self._extract_unique_classes(symbols)

        print("==> [6/7] Extracting API endpoints / config keys / URLs...")
        api_endpoints = sorted({s for s in strings if self.API_RE.match(s)})
        config_keys = sorted({s for s in strings if self.CONFIG_RE.match(s)})
        bc_urls = sorted({m for s in strings for m in self.URL_RE.findall(s)})

        print("==> [7/7] Saving artifacts...")
        self._save_artifacts(symbols, bitcomet_syms, namespaces, core_stats,
                              api_endpoints, config_keys, bc_urls, unique_classes)

        # 写一份 qBittorrent 对比报告 (可选)
        qbt_info = None
        if self.qbt_src and self.qbt_src.exists():
            print(f"==> [optional] Analyzing qBittorrent source at {self.qbt_src}...")
            qbt_info = self._analyze_qbittorrent()

        return AnalysisResult(
            binary_path=str(binary_path),
            file_type=file_type,
            needed_libs=needed,
            symbol_count=len(symbols),
            bitcomet_symbol_count=len(bitcomet_syms),
            namespaces=namespaces,
            core_module_stats=core_stats,
            api_endpoints=api_endpoints,
            config_keys=config_keys,
            bitcomet_urls=bc_urls,
            unique_classes=unique_classes,
        )

    # ----- 步骤 1: 解压 .deb -----

    def _extract_deb(self) -> Path:
        if not self.extract_dir.exists():
            self.extract_dir.mkdir(parents=True)
        # 如果已解压, 直接复用
        candidates = list(self.extract_dir.glob("**/usr/bin/*"))
        if candidates:
            return candidates[0]
        # 用 dpkg-deb 解压
        subprocess.check_call(
            ["dpkg-deb", "-R", self.deb_path, str(self.extract_dir)],
            stderr=subprocess.DEVNULL,
        )
        # 找 BitComet 主二进制
        for binary_name in ("BitComet", "bitcometd"):
            p = self.extract_dir / "usr/bin" / binary_name
            if p.exists():
                return p
        # 退化: 找任何可执行
        for p in (self.extract_dir / "usr/bin").iterdir():
            return p
        raise RuntimeError("no binary found in extracted deb")

    # ----- 步骤 3: 提取 BitComet 独有符号 -----

    def _filter_bitcomet_symbols(self, symbols: List[str]) -> List[str]:
        """过滤出 BitComet 独有的符号 (Core_*, BitComet_*, BC*, Ctrl*, View_*)."""
        result = []
        for line in symbols:
            # line 格式: "0000000000... T symbol"
            parts = line.split(None, 2)
            if len(parts) < 3:
                continue
            sym = parts[2]
            if any(sym.startswith(p) for p in self.BITCOMET_PREFIXES):
                result.append(line)
        return result

    def _extract_namespaces(self, symbols: List[str]) -> Set[str]:
        """提取所有 C++ 命名空间."""
        ns = set()
        for line in symbols:
            parts = line.split(None, 2)
            if len(parts) < 3:
                continue
            sym = parts[2]
            # 找最外层 namespace
            m = re.match(r"^([A-Z][A-Za-z0-9_]*)::", sym)
            if m:
                ns.add(m.group(1))
        return ns

    def _count_core_modules(self, symbols: List[str]) -> Dict[str, int]:
        """统计 Core_* 模块的符号数量."""
        counter = Counter()
        for line in symbols:
            parts = line.split(None, 2)
            if len(parts) < 3:
                continue
            sym = parts[2]
            m = self.CORE_MODULE_RE.match(sym)
            if m:
                counter[m.group(1)] += 1
        return dict(counter.most_common())

    def _extract_unique_classes(self, symbols: List[str]) -> List[str]:
        """提取 BitComet 独有的类 (Core_*::ClassName)."""
        classes = set()
        for line in symbols:
            parts = line.split(None, 2)
            if len(parts) < 3:
                continue
            sym = parts[2]
            m = self.UNIQUE_CLASS_RE.match(sym)
            if m:
                classes.add(f"{sym[:sym.index('::')]}::{m.group(1)}")
        return sorted(classes)

    # ----- 步骤 6: 提取 API / 配置 -----

    def _save_artifacts(self, symbols, bitcomet_syms, namespaces, core_stats,
                         api_endpoints, config_keys, bc_urls, unique_classes) -> None:
        # 所有符号
        with open(self.symbols_dir / "all_symbols.txt", "w") as f:
            f.write("\n".join(symbols))
        # BitComet 独有符号
        with open(self.symbols_dir / "bitcomet_symbols.txt", "w") as f:
            f.write("\n".join(bitcomet_syms))
        # 命名空间
        with open(self.symbols_dir / "namespaces.txt", "w") as f:
            f.write("\n".join(sorted(namespaces)))
        # 唯一类
        with open(self.symbols_dir / "unique_classes.txt", "w") as f:
            f.write("\n".join(unique_classes))
        # API 端点
        with open(self.symbols_dir / "api_endpoints.txt", "w") as f:
            f.write("\n".join(api_endpoints))
        # 配置项
        with open(self.symbols_dir / "config_keys.txt", "w") as f:
            f.write("\n".join(config_keys))
        # URL
        with open(self.symbols_dir / "bitcomet_urls.txt", "w") as f:
            f.write("\n".join(bc_urls))
        # 模块统计 CSV
        with open(self.symbols_dir / "module_stats.csv", "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["module", "symbol_count"])
            for mod, cnt in core_stats.items():
                w.writerow([mod, cnt])
        print(f"    artifacts saved to: {self.symbols_dir}")

    # ----- qBittorrent 对比 -----

    def _analyze_qbittorrent(self) -> Dict:
        """分析 qBittorrent 源码, 用于对比."""
        info = {
            "src_dir": str(self.qbt_src),
            "modules": [],
            "libtorrent_integration": "",
            "webui_controllers": [],
            "webui_pages": [],
        }
        # 主要模块
        base_dir = self.qbt_src / "src/base/bittorrent"
        if base_dir.exists():
            info["modules"] = [
                f.stem for f in base_dir.glob("*.cpp")
            ]
        # libtorrent 使用
        cmake = self.qbt_src / "CMakeLists.txt"
        if cmake.exists():
            content = cmake.read_text()
            m = re.search(r"minLibtorrentVersion\s+(\S+)", content)
            if m:
                info["libtorrent_integration"] = f"requires libtorrent >= {m.group(1)}"
        # WebUI controllers
        webui_dir = self.qbt_src / "src/webui/api"
        if webui_dir.exists():
            info["webui_controllers"] = [
                f.stem.replace("controller", "") for f in webui_dir.glob("*controller.cpp")
            ]
        # WebUI HTML pages
        www_dir = self.qbt_src / "src/webui/www/private"
        if www_dir.exists():
            info["webui_pages"] = [f.stem for f in www_dir.glob("*.html")]
        return info


# -----------------------------------------------------------------------------
# Markdown 报告生成
# -----------------------------------------------------------------------------

def generate_markdown_report(result: AnalysisResult,
                              qbt_info: Optional[Dict] = None) -> str:
    """生成完整的 Markdown 分析报告."""
    lines: List[str] = []
    lines.append(f"# BitComet 符号分析报告\n")
    lines.append(f"**分析目标**: `{result.binary_path}`\n")
    lines.append(f"**文件类型**: {result.file_type}\n")
    lines.append(f"**符号总数**: {result.symbol_count:,}\n")
    lines.append(f"**BitComet 独有符号**: {result.bitcomet_symbol_count:,}\n")
    lines.append(f"**C++ 命名空间数**: {len(result.namespaces):,}\n")
    lines.append(f"**API 端点数**: {len(result.api_endpoints)}\n")
    lines.append(f"**配置项数**: {len(result.config_keys)}\n\n")

    lines.append("## 动态依赖库\n\n")
    for lib in result.needed_libs:
        lines.append(f"- {lib}\n")

    lines.append("\n## Core_* 模块分布\n\n")
    lines.append("| 模块 | 符号数 |\n|------|--------:|\n")
    for mod, cnt in list(result.core_module_stats.items())[:30]:
        lines.append(f"| `{mod}` | {cnt:,} |\n")

    lines.append("\n## API 端点清单\n\n")
    lines.append(f"共 {len(result.api_endpoints)} 个端点.\n\n")
    lines.append("```\n")
    for ep in result.api_endpoints[:50]:
        lines.append(f"{ep}\n")
    if len(result.api_endpoints) > 50:
        lines.append(f"... ({len(result.api_endpoints) - 50} more)\n")
    lines.append("```\n\n")

    lines.append("## 配置项 (enable_* / disable_*)\n\n")
    lines.append("```\n")
    for key in result.config_keys:
        lines.append(f"{key}\n")
    lines.append("```\n\n")

    lines.append("## BitComet 域名端点\n\n")
    for url in result.bitcomet_urls:
        lines.append(f"- `{url}`\n")

    if qbt_info:
        lines.append("\n## qBittorrent 对比\n\n")
        lines.append(f"- **源码目录**: `{qbt_info['src_dir']}`\n")
        lines.append(f"- **libtorrent 集成**: {qbt_info['libtorrent_integration']}\n")
        lines.append(f"- **WebUI Controllers**: {len(qbt_info['webui_controllers'])} 个\n")
        lines.append(f"- **WebUI HTML 页面**: {len(qbt_info['webui_pages'])} 个\n")
        lines.append(f"- **BT 模块 .cpp 文件**: {len(qbt_info['modules'])} 个\n")

    return "".join(lines)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(
        description="BitComet 二进制符号自动分析器 (复现完整逆向流程)"
    )
    ap.add_argument("--deb", required=True, help="BitComet .deb 文件路径")
    ap.add_argument("--output", "-o", required=True, help="输出目录")
    ap.add_argument("--qbittorrent-src", help="qBittorrent 源码目录 (可选, 用于对比)")
    ap.add_argument("--report", default="report.md", help="生成的 Markdown 报告文件名")
    args = ap.parse_args()

    extractor = BitCometSymbolExtractor(
        deb_path=args.deb,
        output_dir=args.output,
        qbt_src=args.qbittorrent_src,
    )
    result = extractor.run()
    qbt_info = extractor._analyze_qbittorrent() if args.qbittorrent_src else None

    # 写报告
    report = generate_markdown_report(result, qbt_info)
    report_path = Path(args.output) / args.report
    report_path.write_text(report, encoding="utf-8")
    print(f"\n✓ Report written to: {report_path}")
    print(f"✓ Symbols saved to: {extractor.symbols_dir}")


if __name__ == "__main__":
    main()
