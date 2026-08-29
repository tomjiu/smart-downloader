#!/bin/bash
# reverse_engineering.sh — 复现完整逆向工作流
# 用法: bash scripts/reverse_engineering.sh [deb_file_path]
set -e

DEB="${1:-BitComet-2.21.2-x86_64.deb}"
OUT="${2:-./reverse_output}"

if [ ! -f "$DEB" ]; then
    echo "Error: $DEB not found"
    echo "Usage: $0 <deb_file> [output_dir]"
    exit 1
fi

mkdir -p "$OUT"
echo "============================================================"
echo "BitComet Reverse Engineering Pipeline"
echo "Sample : $DEB"
echo "Output : $OUT"
echo "============================================================"

echo ""
echo "[1/8] Verifying file type..."
file "$DEB"

echo ""
echo "[2/8] Extracting .deb..."
rm -rf "$OUT/extracted"
mkdir -p "$OUT/extracted"
dpkg-deb -R "$DEB" "$OUT/extracted"

BIN=$(find "$OUT/extracted" -name "BitComet" -o -name "bitcometd" | head -1)
echo "    binary: $BIN"

echo ""
echo "[3/8] Checking binary type + dependencies..."
file "$BIN"
readelf -d "$BIN" | grep NEEDED | head -25

echo ""
echo "[4/8] Extracting demangled symbols (this may take a minute)..."
nm -C "$BIN" > "$OUT/symbols_all.txt"
TOTAL=$(wc -l < "$OUT/symbols_all.txt")
echo "    total symbols: $TOTAL"

echo ""
echo "[5/8] Filtering BitComet-specific symbols..."
grep -E "^[0-9a-f]+ [TtWw] (Core_|BitComet_|BC|Ctrl)" "$OUT/symbols_all.txt" \
    > "$OUT/bitcomet_symbols.txt"
BITCOMET=$(wc -l < "$OUT/bitcomet_symbols.txt")
echo "    bitcomet-specific: $BITCOMET"

echo ""
echo "[6/8] Extracting namespaces..."
grep -oE "^[0-9a-f]+ [TtWw] [A-Z][A-Za-z0-9_]*::" "$OUT/symbols_all.txt" \
    | sed 's/.* [TtWw] //; s/::$//' | sort -u > "$OUT/namespaces.txt"
NS=$(wc -l < "$OUT/namespaces.txt")
echo "    namespaces: $NS"

echo ""
echo "[7/8] Extracting strings (API endpoints, URLs, configs)..."
strings "$BIN" > "$OUT/all_strings.txt"
echo "    total strings: $(wc -l < "$OUT/all_strings.txt")"

# API 端点
grep -E "^/api/" "$OUT/all_strings.txt" | sort -u > "$OUT/api_endpoints.txt"
echo "    API endpoints: $(wc -l < "$OUT/api_endpoints.txt")"

# 配置项
grep -E "^(enable|disable)_" "$OUT/all_strings.txt" | sort -u > "$OUT/config_keys.txt"
echo "    config keys: $(wc -l < "$OUT/config_keys.txt")"

# BitComet URLs
grep -E "https?://[a-z]+\.bitcomet\.com" "$OUT/all_strings.txt" | sort -u > "$OUT/bitcomet_urls.txt"
echo "    bitcomet URLs: $(wc -l < "$OUT/bitcomet_urls.txt")"

# Core_* 模块统计
echo ""
echo "[8/8] Core_* module statistics:"
grep -oE "Core_[A-Za-z]+" "$OUT/symbols_all.txt" | sort | uniq -c | sort -rn | head -25

echo ""
echo "============================================================"
echo "Reverse engineering complete!"
echo ""
echo "Artifacts:"
echo "  $OUT/symbols_all.txt        ($TOTAL lines)"
echo "  $OUT/bitcomet_symbols.txt   ($BITCOMET lines)"
echo "  $OUT/namespaces.txt         ($NS lines)"
echo "  $OUT/api_endpoints.txt"
echo "  $OUT/config_keys.txt"
echo "  $OUT/bitcomet_urls.txt"
echo ""
echo "Next steps:"
echo "  python3 src/bitcomet_symbol_extractor.py --deb $DEB -o $OUT"
echo "============================================================"
