#!/usr/bin/env python3
"""dump_taskinfo.py - dump XLTaskInfo 原始 924 字节，还原真实字段布局。

对已完成的 BT 任务（state=3）和进行中任务 dump，观察字段值分布。
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
        ('size', ctypes.c_uint),
        ('torrent_path', ctypes.c_wchar_p),
        ('save_path', ctypes.c_wchar_p),
        ('third_str', ctypes.c_char_p),
        ('_reserved', ctypes.c_byte * 12),
    ]


def dump_bytes(buf, label):
    """dump 非零字节的偏移和值"""
    raw = bytes(buf)
    nonzero = [(i, raw[i]) for i in range(len(raw)) if raw[i] != 0]
    print(f'--- {label}: {len(raw)} 字节，{len(nonzero)} 个非零字节 ---')
    # 分组连续非零区域
    if nonzero:
        start = nonzero[0][0]
        prev = start
        for i, v in nonzero:
            if i - prev > 3:  # gap
                print(f'  [{start:#x}..{prev:#x}] ({prev-start+1} bytes)')
                start = i
            prev = i
        print(f'  [{start:#x}..{prev:#x}] ({prev-start+1} bytes)')
    # 打印前 64 字节的 hex + 每 4 字节的 u32 解释
    print('  前 64 字节 hex:')
    for off in range(0, 64, 16):
        chunk = raw[off:off+16]
        hexs = ' '.join(f'{b:02x}' for b in chunk)
        print(f'    {off:#06x}: {hexs}')
    print()


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
    lib.XL_QueryTaskInfo.argtypes = [ctypes.c_uint, ctypes.POINTER(ctypes.c_byte * 924)]
    lib.XL_QueryTaskInfo.restype = ctypes.c_int
    lib.XL_StopTask.argtypes = [ctypes.c_uint]
    lib.XL_StopTask.restype = ctypes.c_int
    lib.XL_DeleteTask.argtypes = [ctypes.c_uint, ctypes.c_int]
    lib.XL_DeleteTask.restype = ctypes.c_int
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

    save = str(DLL_DIR / 'downloads')
    os.makedirs(save, exist_ok=True)
    bt = XLBTTaskParamV2()
    bt.size = 0x28
    bt.torrent_path = str(TORRENT)
    bt.save_path = save
    bt.third_str = b'test-download'
    for i in range(12):
        bt._reserved[i] = 0

    out_task_id = ctypes.c_uint(0)
    rc2 = lib.XL_CreateBTTask_V2(ctypes.byref(bt), ctypes.byref(out_task_id))
    print(f'[2] XL_CreateBTTask_V2 = {rc2}, task_id = {out_task_id.value}')
    tid = out_task_id.value

    # dump 启动前（pending 状态）
    buf0 = (ctypes.c_byte * 924)()
    # 首字段 size 必须 = 0x39c（versioned struct 铁证）
    import struct as _s
    buf0[0:4] = _s.pack('<I', 0x39c)
    lib.XL_QueryTaskInfo(tid, buf0)
    dump_bytes(buf0, '启动前（pending）')

    # StartTask
    rc3 = lib.XL_StartTask(tid)
    print(f'[3] XL_StartTask = {rc3}')

    # dump 启动后立即
    time.sleep(1)
    buf1 = (ctypes.c_byte * 924)()
    buf1[0:4] = _s.pack('<I', 0x39c)
    lib.XL_QueryTaskInfo(tid, buf1)
    dump_bytes(buf1, '启动后 1s')

    # 等待，dump 下载中/完成
    for t in [3, 6, 10]:
        time.sleep(t - (t - 3 if t > 3 else 0))
        buf = (ctypes.c_byte * 924)()
        buf[0:4] = _s.pack('<I', 0x39c)
        lib.XL_QueryTaskInfo(tid, buf)
        raw = bytes(buf)
        # 只打印 u32 视角的前 8 个字段
        import struct as s
        vals = s.unpack('<8I', raw[:32])
        print(f'  t={t}s 前8个u32: {[hex(v) for v in vals]}')

    # 最终完整 dump
    time.sleep(5)
    buf2 = (ctypes.c_byte * 924)()
    buf2[0:4] = _s.pack('<I', 0x39c)
    lib.XL_QueryTaskInfo(tid, buf2)
    dump_bytes(buf2, '最终状态')

    lib.XL_StopTask(tid)
    lib.XL_DeleteTask(tid, 1)
    lib.XL_UnInit()
    print('[done]')


if __name__ == '__main__':
    main()
