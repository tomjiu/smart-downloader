#!/usr/bin/env python3
"""verify_p2sp.py - 真机验证 XL_CreateP2spTask_V2（XLP2spParam 布局）。

2026-08-27 反汇编铁证：
- XLP2spParam = size(8) + 5×宽字符串指针(40) + flags(8) = 56
- XL_CreateP2spTask_V2(param, out) 2 参数
"""
import ctypes
import os
import sys
import time
from pathlib import Path

DLL_DIR = Path(r'C:\xl')
DLL_NAME = 'DownloadSDKProxy.dll'


class XLInitParam(ctypes.Structure):
    _pack_ = 1
    _fields_ = [
        ('size', ctypes.c_uint),
        ('field4', ctypes.c_uint),
        ('field8', ctypes.c_ushort),
        ('json', ctypes.c_char * 30),
    ]


class XLP2spParam(ctypes.Structure):
    _fields_ = [
        ('size', ctypes.c_ulonglong),       # +0 = 0x38
        ('url', ctypes.c_wchar_p),          # +8 宽字符串
        ('field10', ctypes.c_wchar_p),      # +0x10
        ('field18', ctypes.c_wchar_p),      # +0x18
        ('save_path', ctypes.c_wchar_p),    # +0x20
        ('field28', ctypes.c_wchar_p),      # +0x28
        ('flags', ctypes.c_ulonglong),      # +0x30 = 2
    ]


def main():
    os.add_dll_directory(str(DLL_DIR))
    os.chdir(DLL_DIR)
    lib = ctypes.WinDLL(str(DLL_DIR / DLL_NAME))

    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]
    lib.XL_Init.restype = ctypes.c_int

    lib.XL_CreateP2spTask_V2.argtypes = [ctypes.POINTER(XLP2spParam), ctypes.POINTER(ctypes.c_uint)]
    lib.XL_CreateP2spTask_V2.restype = ctypes.c_int

    lib.XL_UnInit.argtypes = []
    lib.XL_UnInit.restype = ctypes.c_int

    # 1. Init
    param = XLInitParam()
    param.size = 0x28
    param.field4 = 0
    param.field8 = 0
    server_path = str(DLL_DIR / 'DownloadSDKServer.exe').encode('utf-8')
    rc = lib.XL_Init(server_path, ctypes.byref(param))
    print(f'[1] XL_Init = {rc}')
    if rc != 0:
        return

    # 2. CreateP2spTask_V2（用合理 URL + save 路径）
    p2sp = XLP2spParam()
    p2sp.size = 0x38
    p2sp.url = 'https://example.com/test.zip'  # 宽字符串
    p2sp.field10 = 'referer'   # 可能是 referer
    p2sp.field18 = 'ua'        # 可能是 user-agent
    p2sp.save_path = str(DLL_DIR / 'downloads')
    p2sp.field28 = 'test.zip'  # 可能是文件名
    p2sp.flags = 2

    out_task_id = ctypes.c_uint(0)
    rc2 = lib.XL_CreateP2spTask_V2(ctypes.byref(p2sp), ctypes.byref(out_task_id))
    print(f'[2] XL_CreateP2spTask_V2 = {rc2}, task_id = {out_task_id.value}')

    if rc2 == 0 and out_task_id.value != 0:
        print(f'[PASS] 创建 P2SP 任务成功，task_id = {out_task_id.value}')
    elif rc2 != 0:
        print(f'[info] 返回 {rc2}（可能 URL 无效或字段语义待确认）')
    else:
        print(f'[info] 返回 0 但 task_id=0')

    time.sleep(1)
    rc3 = lib.XL_UnInit()
    print(f'[3] XL_UnInit = {rc3}')


if __name__ == '__main__':
    main()
