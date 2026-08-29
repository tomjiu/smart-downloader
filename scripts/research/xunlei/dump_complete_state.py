#!/usr/bin/env python3
"""dump_complete_state.py - 观察完成状态的 task_state 值 + 速度字段。

用 384 字节 single.torrent（秒完成），观察：
- 完成时 state = 几
- 下载速度字段位置（连续快速查询）
"""
import ctypes
import os
import sys
import time
import struct
from pathlib import Path

DLL_DIR = Path(r'C:\xl')
TORRENT = Path(r'E:\Code\ai\smart-downloader\docs\research\clients\refs\aria2\test\single.torrent')


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
    assert lib.XL_Init(str(DLL_DIR/'DownloadSDKServer.exe').encode(), ctypes.byref(p)) == 0

    save = str(DLL_DIR/'downloads'); os.makedirs(save, exist_ok=True)
    bt = XLBTTaskParamV2(); bt.size=0x28; bt.torrent_path=str(TORRENT); bt.save_path=save; bt.third_str=b'test'
    tid = ctypes.c_uint(0)
    assert lib.XL_CreateBTTask_V2(ctypes.byref(bt), ctypes.byref(tid)) == 0

    def query():
        buf = (ctypes.c_byte*924)(); buf[0:4] = struct.pack('<I', 0x39c)
        r = lib.XL_QueryTaskInfo(tid.value, buf)
        return bytes(buf)

    # 启动 + 快速轮询（0.5s 间隔）观察 state 和速度字段变化
    lib.XL_StartTask(tid.value)
    print('快速轮询（0.5s 间隔，观察 state 变化 + 完整非零字段）...')
    for i in range(20):
        raw = query()
        state = struct.unpack('<I', raw[4:8])[0]
        # 列出 +0x38 之后的所有非零 u32
        tail = []
        for off in range(0x38, 924, 4):
            v = struct.unpack('<I', raw[off:off+4])[0]
            if v not in (0, 0xffffffff):
                tail.append(f'+{off:#04x}={v}')
        dl = struct.unpack('<I', raw[0x14:0x18])[0]
        print(f'  t={0.5*(i+1):4.1f}s state={state} download={dl} | 尾部: {tail}')
        if state in (8, 9):  # 假设完成/失败
            break
        time.sleep(0.5)

    # 最终完整 dump
    print('\n最终完整 924 字节非零字段：')
    raw = query()
    for off in range(0, 924, 4):
        v = struct.unpack('<I', raw[off:off+4])[0]
        if v != 0:
            print(f'  +{off:#04x}: {v} ({v:#x})')

    lib.XL_StopTask(tid.value); lib.XL_DeleteTask(tid.value, 1)
    lib.XL_UnInit()
    print('\n[done]')


if __name__ == '__main__':
    main()
