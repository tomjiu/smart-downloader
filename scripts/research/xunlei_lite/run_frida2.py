#!/usr/bin/env python3
"""Run frida_cred.py against running xllite daemon and capture its messages."""
import sys, time, json
import frida

pid = int(open("scripts/research/xunlei_lite/out/daemon_pid.txt").read().strip())
js_path = "scripts/research/xunlei_lite/frida_cred.py"
js = open(js_path, "r", encoding="utf-8").read()

def on_message(msg, data):
    if msg.get("type") == "send":
        payload = msg["payload"]
        print(payload)
    elif msg.get("type") == "error":
        print("[frida-err]", msg.get("description"))

session = frida.attach(pid)
script = session.create_script(js)
script.on("message", on_message)
script.load()
time.sleep(4)
print("[*] detaching")
session.detach()
