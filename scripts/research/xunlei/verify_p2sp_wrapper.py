#!/usr/bin/env python3
"""verify_p2sp_wrapper.py - 真机验证 XL_CreateP2spTask（6 参数薄包装）。

2026-08-27 反汇编铁证：XL_CreateP2spTask(url, referer, ua, save, filename, out) 6 参数。
"""
import ctypes
import os
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

    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]
    lib.XL_Init.restype = ctypes.c_int

    # 6 参数薄包装
    lib.XL_CreateP2spTask.argtypes = [
        ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_wchar_p,
        ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_uint),
    ]
    lib.XL_CreateP2spTask.restype = ctypes.c_int

    lib.XL_UnInit.argtypes = []
    lib.XL_UnInit.restype = ctypes.c_int

    param = XLInitParam()
    param.size = 0x28
    param.field4 = 0
    param.field8 = 0
    server_path = str(DLL_DIR / 'DownloadSDKServer.exe').encode('utf-8')
    rc = lib.XL_Init(server_path, ctypes.byref(param))
    print(f'[1] XL_Init = {rc}')
    if rc != 0:
        return

    out_task_id = ctypes.c_uint(0)
    rc2 = lib.XL_CreateP2spTask(
        'https://example.com/test.zip',  # url
        '',                              # referer（空串非 NULL）
        '',                              # user-agent（空串非 NULL）
        str(DLL_DIR / 'downloads'),      # save_path
        'test.zip',                      # filename
        ctypes.byref(out_task_id),
    )
    print(f'[2] XL_CreateP2spTask = {rc2}, task_id = {out_task_id.value}')

    if rc2 == 0 and out_task_id.value != 0:
        print(f'[PASS] 创建 P2SP 任务成功（薄包装），task_id = {out_task_id.value}')
    else:
        print(f'[info] 返回 {rc2}')

    time.sleep(1)
    rc3 = lib.XL_UnInit()
    print(f'[3] XL_UnInit = {rc3}')


if __name__ == '__main__':
    main()
