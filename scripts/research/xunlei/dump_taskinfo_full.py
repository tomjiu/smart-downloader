#!/usr/bin/env python3
"""dump_taskinfo_full.py - dump XLTaskInfo 完整 924 字节，逆向剩余字段。

通过对比不同状态（未启动/下载中/暂停/完成/错误），定位：
- 下载速度字段（下载中非零）
- 错误信息字段（失败任务）
- task_state 完整枚举值
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


def dump_nonzero(raw, label):
    """按 u32 视角列出所有非零字段的偏移和值"""
    print(f'--- {label} ---')
    fields = []
    for off in range(0, len(raw), 4):
        v = struct.unpack('<I', raw[off:off+4])[0]
        if v != 0:
            fields.append(f'+{off:#04x}={v}')
    # 合并显示
    if fields:
        print('  ' + '  '.join(fields))
    else:
        print('  （全零）')
    print()


def dump_region(raw, start, end, label):
    """dump 某个区域，按 u32 和 ascii 双视角"""
    print(f'--- {label} ({start:#x}..{end:#x}) ---')
    for off in range(start, end, 4):
        v = struct.unpack('<I', raw[off:off+4])[0]
        if v != 0:
            ascii_repr = ''
            b = raw[off:off+4]
            if all(32 <= x < 127 for x in b if x != 0):
                ascii_repr = ' ' + repr(b.rstrip(b'\x00').decode('ascii', 'ignore'))
            print(f'  +{off:#04x}: {v} ({v:#x}){ascii_repr}')
    print()


def main():
    os.add_dll_directory(str(DLL_DIR)); os.chdir(DLL_DIR)
    lib = ctypes.WinDLL(str(DLL_DIR / 'DownloadSDKProxy.dll'))
    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]; lib.XL_Init.restype = ctypes.c_int
    lib.XL_CreateBTTask_V2.argtypes = [ctypes.POINTER(XLBTTaskParamV2), ctypes.POINTER(ctypes.c_uint)]; lib.XL_CreateBTTask_V2.restype = ctypes.c_int
    lib.XL_StartTask.argtypes = [ctypes.c_uint]; lib.XL_StartTask.restype = ctypes.c_int
    lib.XL_StopTask.argtypes = [ctypes.c_uint]; lib.XL_StopTask.restype = ctypes.c_int
    lib.XL_QueryTaskInfo.argtypes = [ctypes.c_uint, ctypes.POINTER(ctypes.c_byte*924)]; lib.XL_QueryTaskInfo.restype = ctypes.c_int
    lib.XL_DeleteTask.argtypes = [ctypes.c_uint, ctypes.c_int]; lib.XL_DeleteTask.restype = ctypes.c_int
    lib.XL_UnInit.argtypes = []; lib.XL_UnInit.restype = ctypes.c_int

    p = XLInitParam(); p.size=0x28; p.field4=0; p.field8=0
    sp = str(DLL_DIR/'DownloadSDKServer.exe').encode()
    assert lib.XL_Init(sp, ctypes.byref(p)) == 0

    save = str(DLL_DIR/'downloads'); os.makedirs(save, exist_ok=True)
    bt = XLBTTaskParamV2(); bt.size=0x28; bt.torrent_path=str(TORRENT); bt.save_path=save; bt.third_str=b'test'
    tid = ctypes.c_uint(0)
    assert lib.XL_CreateBTTask_V2(ctypes.byref(bt), ctypes.byref(tid)) == 0
    print(f'[创建] task_id = {tid.value}')

    def query():
        buf = (ctypes.c_byte*924)(); buf[0:4] = struct.pack('<I', 0x39c)
        r = lib.XL_QueryTaskInfo(tid.value, buf)
        return r, bytes(buf)

    # 1. 未启动（pending）
    r, raw = query()
    print(f'[1] 未启动 state@0x04={struct.unpack("<I", raw[4:8])[0]}')
    dump_nonzero(raw, '未启动完整 924 字节')

    # 2. 启动后下载中（等 10s，下载速度应非零）
    lib.XL_StartTask(tid.value)
    time.sleep(10)
    r, raw = query()
    print(f'[2] 下载中 state@0x04={struct.unpack("<I", raw[4:8])[0]}')
    dump_nonzero(raw, '下载中完整 924 字节')

    # 3. 暂停（StopTask）
    lib.XL_StopTask(tid.value)
    time.sleep(2)
    r, raw = query()
    print(f'[3] 暂停后 state@0x04={struct.unpack("<I", raw[4:8])[0]}')
    dump_nonzero(raw, '暂停后完整 924 字节')

    # 4. 恢复（重新 Start）
    lib.XL_StartTask(tid.value)
    time.sleep(3)
    r, raw = query()
    print(f'[4] 恢复后 state@0x04={struct.unpack("<I", raw[4:8])[0]}')

    # 5. 清理
    lib.XL_StopTask(tid.value); lib.XL_DeleteTask(tid.value, 1)
    lib.XL_UnInit()
    print('\n[done]')


if __name__ == '__main__':
    main()
