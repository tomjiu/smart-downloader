#!/usr/bin/env python3
import os, time, frida

env = dict(os.environ); env["PLATFORM"] = "pcxllite"
exe = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
wd = r"C:\xlrun"

js = open("scripts/research/xunlei_lite/frida_spawn.js", "r", encoding="utf-8").read()
buf = []
def on(m, d):
    if m.get("type") == "send":
        try: print("  FRIDA:", m["payload"])
        except: pass
        buf.append(m["payload"])
    elif m.get("type") == "error":
        print("  ERR:", m.get("description"))

pid = frida.spawn([exe, "run"], cwd=wd, env=env)
print("spawned pid", pid)
session = frida.attach(pid)
script = session.create_script(js)
script.on("message", on)
script.load()
frida.resume(pid)
time.sleep(9)
open("scripts/research/xunlei_lite/out/frida_spawn_out.txt","w",encoding="utf-8").write("\n".join(buf))
try: session.detach()
except: pass
print("[*] done, wrote out/frida_spawn_out.txt")
