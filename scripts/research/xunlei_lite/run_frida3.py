#!/usr/bin/env python3
import sys, time, os
import frida

pid = int(sys.argv[1])
js = open("scripts/research/xunlei_lite/frida_bypass.py","r",encoding="utf-8").read()
out_path = "scripts/research/xunlei_lite/out/frida_bypass_run.txt"
buf = []
def on_message(msg, data):
    if msg.get("type")=="send":
        line = msg["payload"]
        print(line)
        buf.append(line)
    elif msg.get("type")=="error":
        print("[frida-err]", msg.get("description"))
        buf.append("[frida-err] "+str(msg.get("description")))
session = frida.attach(pid)
script = session.create_script(js)
script.on("message", on_message)
script.load()
time.sleep(6)
open(out_path,"w",encoding="utf-8").write("\n".join(buf))
print("[*] wrote", out_path)
session.detach()
