#!/usr/bin/env python3
"""verify_xl_init.py - 真机验证 XL_Init（用真实 ABI：2 参数 + 新 XLInitParam 布局）。

2026-08-27 真机反汇编铁证后的最终验证：
- XL_Init(server_path, param) -> LtErr，**2 参数，无 out_handle**
- XLInitParam = size(40) + u32 + word(0xffff=无JSON) + json[30]
- server_path 有 100 字符限制

用法:
    python verify_xl_init.py
"""
import ctypes
import os
import sys
import time
from pathlib import Path

DLL_DIR = Path(r'C:\xl')  # 短路径（server_path 100 字符限制）
DLL_NAME = 'DownloadSDKProxy.dll'


class XLInitParam(ctypes.Structure):
    """XLInitParam：size(4) + u32(4) + word(2) + json(30) = 40（pack(1)）。"""
    _pack_ = 1
    _fields_ = [
        ('size', ctypes.c_uint),       # +0x00 = 0x28
        ('field4', ctypes.c_uint),     # +0x04 u32 配置标志
        ('field8', ctypes.c_ushort),   # +0x08 word（0xffff = 无 JSON）
        ('json', ctypes.c_char * 30),  # +0x0a JSON 字符串 "{...}"
    ]


def main():
    os.add_dll_directory(str(DLL_DIR))
    os.chdir(DLL_DIR)

    lib = ctypes.WinDLL(str(DLL_DIR / DLL_NAME))

    # XL_Init(server_path, param) -> int（2 参数，无 out_handle）
    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]
    lib.XL_Init.restype = ctypes.c_int

    lib.XL_UnInit.argtypes = []
    lib.XL_UnInit.restype = ctypes.c_int

    # 构造 XLInitParam（field8=0 = 空 JSON，实测成功；0xffff = 无 JSON 会返回 1）
    param = XLInitParam()
    param.size = 0x28
    param.field4 = 0
    param.field8 = 0  # 空 JSON（实测 rc=0；0xffff 会 rc=1）
    # json 保持全 0（空字符串）

    server_path = str(DLL_DIR / 'DownloadSDKServer.exe').encode('utf-8')

    print(f'[i] server_path = {server_path.decode()} (len={len(server_path)})')
    print(f'[i] XLInitParam 布局: size={ctypes.sizeof(XLInitParam)} 字节（期望 40）')
    print(f'[i] field8 = 0（空 JSON，实测成功）')

    rc = lib.XL_Init(server_path, ctypes.byref(param))

    print(f'[result] XL_Init 返回码 = {rc}')

    if rc == 0:
        print('[PASS] XL_Init 成功！真实 ABI（2 参数 + 新布局）验证通过。')
        print('[i] handle 是 SDK 全局状态（无输出参数），后续 XL_* 调用无需 handle。')

        # 检查 server 进程
        time.sleep(1)
        import subprocess
        out = subprocess.run(['tasklist', '/FI', 'IMAGENAME eq DownloadSDKServer.exe'],
                             capture_output=True, text=True).stdout
        if 'DownloadSDKServer' in out:
            print('[i] server 进程持续运行确认')
        else:
            print('[warn] server 进程未在运行（可能已退出）')

        # 清理
        rc2 = lib.XL_UnInit()
        print(f'[i] XL_UnInit 返回码 = {rc2}')
    else:
        print(f'[FAIL] XL_Init 返回 {rc}，需继续排查（field4/field8 语义或 JSON 内容）')


if __name__ == '__main__':
    main()
