#!/usr/bin/env python3
"""verify_bt_progress.py - 观察大文件 BT 下载进度，定位 XLTaskInfo 真实字段布局。

用 ubuntu iso（1.17GB）观察 download_size 从 0 增长，精确定位字段偏移。
"""
import ctypes
import os
import sys
import time
import struct
from pathlib import Path

DLL_DIR = Path(r'C:\xl')
TORRENT = Path(r'E:\Code\ai\smart-downloader\docs\research\clients\refs\rqbit\crates\librqbit\resources\ubuntu-21.04-live-server-amd64.iso.torrent')


class XLInitParam(ctypes.Structure):
    _pack_ = 1
    _fields_ = [('size', ctypes.c_uint), ('field4', ctypes.c_uint), ('field8', ctypes.c_ushort), ('json', ctypes.c_char*30)]

class XLBTTaskParamV2(ctypes.Structure):
    _pack_ = 1
    _fields_ = [('size', ctypes.c_uint), ('torrent_path', ctypes.c_wchar_p), ('save_path', ctypes.c_wchar_p), ('third_str', ctypes.c_char_p), ('_reserved', ctypes.c_byte*12)]


def main():
    os.add_dll_directory(str(DLL_DIR)); os.chdir(DLL_DIR)
    lib = ctypes.WinDLL(str(DLL_DIR / 'DownloadSDKProxy.dll'))
    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]; lib.XL_Init.restype = ctypes.c_int
    lib.XL_CreateBTTask_V2.argtypes = [ctypes.POINTER(XLBTTaskParamV2), ctypes.POINTER(ctypes.c_uint)]; lib.XL_CreateBTTask_V2.restype = ctypes.c_int
    lib.XL_StartTask.argtypes = [ctypes.c_uint]; lib.XL_StartTask.restype = ctypes.c_int
    lib.XL_QueryTaskInfo.argtypes = [ctypes.c_uint, ctypes.POINTER(ctypes.c_byte*924)]; lib.XL_QueryTaskInfo.restype = ctypes.c_int
    lib.XL_StopTask.argtypes = [ctypes.c_uint]; lib.XL_StopTask.restype = ctypes.c_int
    lib.XL_DeleteTask.argtypes = [ctypes.c_uint, ctypes.c_int]; lib.XL_DeleteTask.restype = ctypes.c_int
    lib.XL_UnInit.argtypes = []; lib.XL_UnInit.restype = ctypes.c_int

    p = XLInitParam(); p.size=0x28; p.field4=0; p.field8=0
    sp = str(DLL_DIR/'DownloadSDKServer.exe').encode()
    rc = lib.XL_Init(sp, ctypes.byref(p))
    print(f'XL_Init = {rc}')
    if rc: return

    save = str(DLL_DIR/'downloads'); os.makedirs(save, exist_ok=True)
    bt = XLBTTaskParamV2(); bt.size=0x28; bt.torrent_path=str(TORRENT); bt.save_path=save; bt.third_str=b'ubuntu-live'
    tid = ctypes.c_uint(0)
    rc2 = lib.XL_CreateBTTask_V2(ctypes.byref(bt), ctypes.byref(tid))
    print(f'XL_CreateBTTask_V2 = {rc2}, task_id = {tid.value}')
    rc3 = lib.XL_StartTask(tid.value)
    print(f'XL_StartTask = {rc3}')

    # 观察 60 秒，每 5 秒 dump 前 0x40 字节的 u32 视图
    print(f'\n观察下载进度（每 5s dump 前 0x40 字节）...')
    for i in range(12):
        time.sleep(5)
        buf = (ctypes.c_byte*924)(); buf[0:4] = struct.pack('<I', 0x39c)
        r = lib.XL_QueryTaskInfo(tid.value, buf)
        raw = bytes(buf)
        # 打印前 0x40 字节里非零的 u32
        nonzero = []
        for off in range(0, 0x40, 4):
            v = struct.unpack('<I', raw[off:off+4])[0]
            if v != 0:
                nonzero.append(f'+{off:#04x}={v}')
        print(f'  t={5*(i+1):2d}s rc={r} | {"  ".join(nonzero)}')

    # 最终完整 dump 前 0x100 字节
    time.sleep(2)
    buf = (ctypes.c_byte*924)(); buf[0:4] = struct.pack('<I', 0x39c)
    lib.XL_QueryTaskInfo(tid.value, buf)
    raw = bytes(buf)
    print(f'\n最终前 0x100 字节非零 u32:')
    for off in range(0, 0x100, 4):
        v = struct.unpack('<I', raw[off:off+4])[0]
        if v != 0:
            print(f'  +{off:#04x}: {v} ({v:#x})')

    lib.XL_StopTask(tid.value); lib.XL_DeleteTask(tid.value, 1)
    lib.XL_UnInit()
    print('\n[done]')


if __name__ == '__main__':
    main()
