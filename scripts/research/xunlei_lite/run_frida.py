#!/usr/bin/env python3
"""R4 driver: attach to running xllite (PID from argv), load the JS, resume,
and capture WinHttp headers for api-pan (G2). Also try to hook the Go
platformdetect.GetClientSecret / GetClientID by locating them via the
embedded func-name table (best-effort)."""
import sys, time
import frida

pid = int(sys.argv[1])
js = open("scripts/research/xunlei_lite/frida_platformsecret.js", "r", encoding="utf-8").read()

def on_message(msg, data):
    if msg.get("type") == "send":
        print("[frida]", msg["payload"])
    elif msg.get("type") == "error":
        print("[frida-err]", msg.get("description"))

session = frida.attach(pid)
script = session.create_script(js)
script.on("message", on_message)
script.load()
print(f"[*] attached to pid {pid}, script loaded; sleeping 25s to capture traffic")
time.sleep(25)
print("[*] done")
session.detach()
