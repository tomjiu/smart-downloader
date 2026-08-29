#!/usr/bin/env python3
"""verify_create_task.py - 真机验证 XL_Init → XL_CreateMagnetTask 完整流程。

2026-08-27 反汇编铁证后的验证：
- XL_Init(server_path, param) 2 参数
- XL_CreateMagnetTask(magnet_wide, save_wide, out) 3 参数（宽字符串）
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


def main():
    os.add_dll_directory(str(DLL_DIR))
    os.chdir(DLL_DIR)
    lib = ctypes.WinDLL(str(DLL_DIR / DLL_NAME))

    # XL_Init(server_path, param) -> int
    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]
    lib.XL_Init.restype = ctypes.c_int

    # XL_CreateMagnetTask(magnet_w, save_w, out) -> int
    lib.XL_CreateMagnetTask.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_uint)]
    lib.XL_CreateMagnetTask.restype = ctypes.c_int

    lib.XL_UnInit.argtypes = []
    lib.XL_UnInit.restype = ctypes.c_int

    # 1. Init
    param = XLInitParam()
    param.size = 0x28
    param.field4 = 0
    param.field8 = 0  # 空 JSON
    server_path = str(DLL_DIR / 'DownloadSDKServer.exe').encode('utf-8')

    rc = lib.XL_Init(server_path, ctypes.byref(param))
    print(f'[1] XL_Init = {rc}')
    if rc != 0:
        print('[FAIL] XL_Init 失败')
        return
    print('[PASS] XL_Init 成功，server 已启动')

    # 2. CreateMagnetTask（宽字符串）
    magnet = 'magnet:?xt=urn:btih:0000000000000000000000000000000000000000&dn=test'
    save = str(DLL_DIR / 'downloads')
    os.makedirs(save, exist_ok=True)

    out_task_id = ctypes.c_uint(0)
    rc2 = lib.XL_CreateMagnetTask(magnet, save, ctypes.byref(out_task_id))
    print(f'[2] XL_CreateMagnetTask = {rc2}, task_id = {out_task_id.value}')

    if rc2 == 0 and out_task_id.value != 0:
        print(f'[PASS] 创建磁力任务成功，task_id = {out_task_id.value}')
    elif rc2 != 0:
        print(f'[info] XL_CreateMagnetTask 返回 {rc2}（可能是无效磁力链接或网络问题）')
    else:
        print(f'[info] 返回 0 但 task_id=0（可能 magnet 链接无效）')

    # 3. UnInit
    time.sleep(1)
    rc3 = lib.XL_UnInit()
    print(f'[3] XL_UnInit = {rc3}')


if __name__ == '__main__':
    main()
