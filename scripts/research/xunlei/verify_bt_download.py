#!/usr/bin/env python3
"""verify_bt_download.py - 真机验证 BT 下载全链路。

2026-08-27 反汇编铁证后的验证：
- XL_CreateBTTask_V2(param, out_task_id) 2 参数，param = XLBTTaskParamV2 (pack1, 40字节)
- XL_StartTask(task_id) / XL_QueryTaskInfo(task_id, out) / XL_StopTask(task_id)
"""
import ctypes
import os
import sys
import time
from pathlib import Path

DLL_DIR = Path(r'C:\xl')
DLL_NAME = 'DownloadSDKProxy.dll'
TORRENT = Path(r'E:\Code\ai\smart-downloader\docs\research\clients\refs\aria2\test\single.torrent')


class XLInitParam(ctypes.Structure):
    _pack_ = 1
    _fields_ = [
        ('size', ctypes.c_uint),
        ('field4', ctypes.c_uint),
        ('field8', ctypes.c_ushort),
        ('json', ctypes.c_char * 30),
    ]


class XLBTTaskParamV2(ctypes.Structure):
    _pack_ = 1
    _fields_ = [
        ('size', ctypes.c_uint),            # +0x00 = 0x28
        ('torrent_path', ctypes.c_wchar_p), # +0x04 宽字符串
        ('save_path', ctypes.c_wchar_p),    # +0x0c 宽字符串
        ('third_str', ctypes.c_char_p),     # +0x14 窄字符串
        ('_reserved', ctypes.c_byte * 12),  # +0x1c
    ]


class XLTaskInfo(ctypes.Structure):
    """推测布局（需 dump 还原）。先读前几个字段看 task_state/download_size。"""
    _pack_ = 1
    _fields_ = [
        ('size', ctypes.c_uint),
        ('task_state', ctypes.c_uint),
        ('task_id', ctypes.c_ulonglong),
        ('download_size', ctypes.c_ulonglong),
        ('file_size', ctypes.c_ulonglong),
        ('_rest', ctypes.c_byte * (924 - 4 - 4 - 8 - 8 - 8)),
    ]


def main():
    os.add_dll_directory(str(DLL_DIR))
    os.chdir(DLL_DIR)
    lib = ctypes.WinDLL(str(DLL_DIR / DLL_NAME))

    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]
    lib.XL_Init.restype = ctypes.c_int
    lib.XL_CreateBTTask_V2.argtypes = [ctypes.POINTER(XLBTTaskParamV2), ctypes.POINTER(ctypes.c_uint)]
    lib.XL_CreateBTTask_V2.restype = ctypes.c_int
    lib.XL_StartTask.argtypes = [ctypes.c_uint]
    lib.XL_StartTask.restype = ctypes.c_int
    lib.XL_QueryTaskInfo.argtypes = [ctypes.c_uint, ctypes.POINTER(XLTaskInfo)]
    lib.XL_QueryTaskInfo.restype = ctypes.c_int
    lib.XL_StopTask.argtypes = [ctypes.c_uint]
    lib.XL_StopTask.restype = ctypes.c_int
    lib.XL_DeleteTask.argtypes = [ctypes.c_uint, ctypes.c_int]
    lib.XL_DeleteTask.restype = ctypes.c_int
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
        print('[FAIL] XL_Init 失败')
        return

    # 2. CreateBTTask_V2
    save = str(DLL_DIR / 'downloads')
    os.makedirs(save, exist_ok=True)
    bt = XLBTTaskParamV2()
    bt.size = 0x28
    bt.torrent_path = str(TORRENT)   # 宽字符串路径
    bt.save_path = save
    bt.third_str = b'test-download'  # 窄字符串，语义待确认（任务名/infohash？），必须非空
    for i in range(12):
        bt._reserved[i] = 0

    out_task_id = ctypes.c_uint(0)
    rc2 = lib.XL_CreateBTTask_V2(ctypes.byref(bt), ctypes.byref(out_task_id))
    print(f'[2] XL_CreateBTTask_V2 = {rc2}, task_id = {out_task_id.value}')
    if rc2 != 0 or out_task_id.value == 0:
        print(f'[FAIL] 创建 BT 任务失败 (rc={rc2}, task_id={out_task_id.value})')
        lib.XL_UnInit()
        return

    tid = out_task_id.value

    # 3. StartTask
    rc3 = lib.XL_StartTask(tid)
    print(f'[3] XL_StartTask({tid}) = {rc3}')

    # 4. 轮询 QueryTaskInfo，看 task_state 变化
    print(f'[4] 轮询 QueryTaskInfo（每 2s，共 30s）...')
    for i in range(15):
        time.sleep(2)
        info = XLTaskInfo()
        info.size = 0x39c
        rc4 = lib.XL_QueryTaskInfo(tid, ctypes.byref(info))
        print(f'    t={2*(i+1):2d}s: QueryTaskInfo={rc4}, state={info.task_state}, '
              f'download_size={info.download_size}, file_size={info.file_size}, '
              f'task_id={info.task_id}')
        if rc4 != 0:
            print(f'[FAIL] QueryTaskInfo 返回 {rc4}')
            break
        # state 3=complete, 4=error 时停止
        if info.task_state in (3, 4):
            break

    # 5. 清理
    lib.XL_StopTask(tid)
    lib.XL_DeleteTask(tid, 1)
    time.sleep(0.5)
    rc5 = lib.XL_UnInit()
    print(f'[5] XL_UnInit = {rc5}')


if __name__ == '__main__':
    main()
