#!/usr/bin/env python3
"""verify_p2sp_lifecycle.py - 用本地 HTTP server 验证 P2SP 完整生命周期。

起一个本地 HTTP server 提供 ~10MB 文件，用 XL_CreateP2spTask 下载，
观察完整状态迁移（downloading → complete）+ 下载速度字段。
"""
import ctypes
import os
import sys
import time
import struct
import threading
import http.server
import socketserver
import tempfile
from pathlib import Path

DLL_DIR = Path(r'C:\xl')


class XLInitParam(ctypes.Structure):
    _pack_ = 1
    _fields_ = [('size', ctypes.c_uint), ('field4', ctypes.c_uint), ('field8', ctypes.c_ushort), ('json', ctypes.c_char*30)]


# 生成一个 ~5MB 的临时文件
def make_test_file():
    p = Path(DLL_DIR) / 'downloads' / 'test_5mb.bin'
    p.parent.mkdir(exist_ok=True)
    if not p.exists() or p.stat().st_size < 5*1024*1024:
        data = os.urandom(5*1024*1024)
        p.write_bytes(data)
    return p


def start_http_server(filepath):
    """起一个本地 HTTP server，返回 URL"""
    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *a, **kw):
            super().__init__(*a, directory=str(filepath.parent), **kw)
        def log_message(self, *a):
            pass
    server = socketserver.TCPServer(('127.0.0.1', 0), Handler)
    port = server.server_address[1]
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    return f'http://127.0.0.1:{port}/{filepath.name}', server


def main():
    fp = make_test_file()
    url, server = start_http_server(fp)
    print(f'本地文件: {fp} ({fp.stat().st_size} bytes)')
    print(f'HTTP URL: {url}')

    os.add_dll_directory(str(DLL_DIR)); os.chdir(DLL_DIR)
    lib = ctypes.WinDLL(str(DLL_DIR / 'DownloadSDKProxy.dll'))
    lib.XL_Init.argtypes = [ctypes.c_char_p, ctypes.POINTER(XLInitParam)]; lib.XL_Init.restype = ctypes.c_int
    lib.XL_CreateP2spTask.argtypes = [ctypes.c_wchar_p]*5 + [ctypes.POINTER(ctypes.c_uint)]; lib.XL_CreateP2spTask.restype = ctypes.c_int
    lib.XL_StartTask.argtypes = [ctypes.c_uint]; lib.XL_StartTask.restype = ctypes.c_int
    lib.XL_QueryTaskInfo.argtypes = [ctypes.c_uint, ctypes.POINTER(ctypes.c_byte*924)]; lib.XL_QueryTaskInfo.restype = ctypes.c_int
    lib.XL_StopTask.argtypes = [ctypes.c_uint]; lib.XL_StopTask.restype = ctypes.c_int
    lib.XL_DeleteTask.argtypes = [ctypes.c_uint, ctypes.c_int]; lib.XL_DeleteTask.restype = ctypes.c_int
    lib.XL_UnInit.argtypes = []; lib.XL_UnInit.restype = ctypes.c_int

    p = XLInitParam(); p.size=0x28; p.field4=0; p.field8=0
    assert lib.XL_Init(str(DLL_DIR/'DownloadSDKServer.exe').encode(), ctypes.byref(p)) == 0

    save = str(DLL_DIR/'downloads')
    tid = ctypes.c_uint(0)
    rc = lib.XL_CreateP2spTask(url, '', '', save, 'test_5mb.bin', ctypes.byref(tid))
    print(f'XL_CreateP2spTask = {rc}, task_id = {tid.value}')

    def query():
        buf = (ctypes.c_byte*924)(); buf[0:4] = struct.pack('<I', 0x39c)
        r = lib.XL_QueryTaskInfo(tid.value, buf)
        return r, bytes(buf)

    lib.XL_StartTask(tid.value)
    print('轮询下载进度...')
    prev_dl = 0
    for i in range(30):
        r, raw = query()
        state = struct.unpack('<I', raw[4:8])[0]
        dl = struct.unpack('<I', raw[0x14:0x18])[0]
        fs = struct.unpack('<I', raw[0x0c:0x10])[0]
        speed = (dl - prev_dl)  # 每秒增量近似
        print(f'  t={1*(i+1):2d}s state={state} download={dl}/{fs} ({speed}B/s)')
        prev_dl = dl
        if state in (8, 9) or (fs > 0 and dl >= fs):
            break
        time.sleep(1)

    # 最终完整 dump
    print('\n最终完整 924 字节非零字段:')
    r, raw = query()
    for off in range(0, 924, 4):
        v = struct.unpack('<I', raw[off:off+4])[0]
        if v != 0:
            print(f'  +{off:#04x}: {v} ({v:#x})')

    lib.XL_StopTask(tid.value); lib.XL_DeleteTask(tid.value, 1)
    lib.XL_UnInit()
    server.shutdown()
    print('\n[done]')


if __name__ == '__main__':
    main()
