#!/usr/bin/env python3
"""检查迅雷 P2P 模块导出表：验证「P2P 模块是否有独立可调用入口」。
判定标准：
- 若 P2P*.dll 导出的是内部 C++ 插件接口（少量序号导出 / GetClassObject 类工厂），则只能被 DownloadSDK 宿主加载；
- 若有独立的 Connect/Task/Session 类 C 导出，则存在单独调用可能。
同时检查 DownloadSDK.dll 的导入表，确认宿主与 P2P 模块的耦合方式。
"""
import os
import pefile

BASE = "/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/extracted/resource_1288_1304_unpacked"

P2P_MODULES = [
    "P2PBase.dll", "P2PFramework.dll", "P2PTarget.dll", "P2PStat.dll",
    "TcpImpl.dll", "XUdt.dll", "Http.dll",
]
HOSTS = ["DownloadSDK.dll", "DownloadSDKProxy.dll"]

def dump_exports(path):
    try:
        pe = pefile.PE(path, fast_load=True)
        pe.parse_data_directories(
            directories=[pefile.DIRECTORY_ENTRY['IMAGE_DIRECTORY_ENTRY_EXPORT']])
        out = []
        exp = getattr(pe, "DIRECTORY_ENTRY_EXPORT", None)
        if not exp:
            return out
        for sym in exp.symbols[:60]:
            name = sym.name.decode() if sym.name else f"#{sym.ordinal}"
            out.append(name)
        return out
    except Exception as e:
        return [f"<error: {e}>"]

def dump_imports(path, limit=40):
    try:
        pe = pefile.PE(path, fast_load=True)
        pe.parse_data_directories(
            directories=[pefile.DIRECTORY_ENTRY['IMAGE_DIRECTORY_ENTRY_IMPORT']])
        mods = []
        for entry in getattr(pe, "DIRECTORY_ENTRY_IMPORT", []):
            dll = entry.dll.decode()
            mods.append((dll, len(entry.imports)))
        return mods
    except Exception as e:
        return [(f"<error: {e}>", 0)]

print("=" * 78)
print("A. P2P 传输层模块导出面（判定是否可脱离宿主单独调用）")
print("=" * 78)
for m in P2P_MODULES:
    p = os.path.join(BASE, m)
    if not os.path.exists(p):
        print(f"[{m}] 不存在，跳过")
        continue
    exps = dump_exports(p)
    print(f"\n[{m}] 导出 {len(exps)} 个：")
    for e in exps[:40]:
        print(f"    {e}")
    if len(exps) > 40:
        print(f"    ...（其余 {len(exps)-40} 个省略）")

print()
print("=" * 78)
print("B. 宿主引擎导入表（DownloadSDK / Proxy 依赖哪些模块）")
print("=" * 78)
for h in HOSTS:
    p = os.path.join(BASE, h)
    if not os.path.exists(p):
        print(f"[{h}] 不存在，跳过")
        continue
    print(f"\n[{h}] 导入：")
    for dll, cnt in dump_imports(p):
        print(f"    {dll}  ({cnt} 函数)")
