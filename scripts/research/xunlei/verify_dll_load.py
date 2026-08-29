#!/usr/bin/env python3
"""verify_dll_load.py - 验证 DownloadSDKProxy.dll 可加载 + 符号可解析。

这是真机验证的第一步（安全，不启动 server 进程、不调用 XL_Init）：
1. 设置 DLL 搜索路径到解包目录
2. LoadLibrary DownloadSDKProxy.dll
3. 解析关键导出符号
4. 报告结果

用法:
    python verify_dll_load.py
"""
import ctypes
import os
import sys
from pathlib import Path

DLL_DIR = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted\resource_1288_1304_unpacked')
DLL_NAME = 'DownloadSDKProxy.dll'

# 关键导出符号（用于验证）
KEY_SYMBOLS = [
    'XL_Init',
    'XL_UnInit',
    'XL_CreateBTTask_V2',
    'XL_CreateMagnetTask',
    'XL_AddServer',
    'XL_AddPeer',
    'XL_QueryTaskInfo',
    'XL_StartTask',
    'XL_StopTask',
    'XL_DeleteTask',
]


def main():
    # 1. 设置 DLL 搜索路径
    os.add_dll_directory(str(DLL_DIR))
    os.chdir(DLL_DIR)  # 让相对依赖能找到

    # 2. 检查依赖文件
    missing = []
    for f in ['DownloadSDKProxy.dll', 'DownloadSDKServer.exe', 'DownloadSDK.dll',
              'msvcr90.dll', 'msvcp90.dll']:
        if not (DLL_DIR / f).exists():
            missing.append(f)
    if missing:
        print(f'[ERR] 缺少依赖: {missing}')
        sys.exit(1)
    print('[OK] 依赖文件齐全')

    # 3. LoadLibrary
    dll_path = str(DLL_DIR / DLL_NAME)
    try:
        lib = ctypes.WinDLL(dll_path)
        print(f'[OK] LoadLibrary 成功: {dll_path}')
    except OSError as e:
        print(f'[ERR] LoadLibrary 失败: {e}')
        print('      可能原因: 依赖 DLL 缺失 / VC90 运行时未注册 / 架构不匹配')
        sys.exit(1)

    # 4. 解析符号
    print(f'\n=== 导出符号解析 ===')
    all_ok = True
    for sym in KEY_SYMBOLS:
        try:
            fn = getattr(lib, sym)
            print(f'  [OK] {sym} -> {fn}')
        except AttributeError:
            print(f'  [MISS] {sym}')
            all_ok = False

    if all_ok:
        print('\n[PASS] 所有关键符号可解析，DLL 可加载。')
        print('下一步（真机验证）: 调用 XL_Init 启动 server 进程，验证结构体布局。')
    else:
        print('\n[WARN] 部分符号缺失，检查 DLL 版本。')


if __name__ == '__main__':
    main()
