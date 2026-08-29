#!/usr/bin/env python3
import sys, time, os, subprocess
import frida

env = dict(os.environ); env["PLATFORM"] = "pcxllite"
p = subprocess.Popen(["C:\\Program Files\\Thunder Network\\Thunder\\program\\xllite.exe", "run"],
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd="C:\\xlrun", env=env)
pid = p.pid
print("pid", pid)
js = open("scripts/research/xunlei_lite/frida_test.js", "r", encoding="utf-8").read()
buf = []
def onmsg(m, d):
    if m.get("type") == "send": print("  FRIDA:", m["payload"]); buf.append(m["payload"])
    elif m.get("type") == "error": print("  FRIDA-ERR:", m.get("description")); buf.append("ERR "+str(m.get("description")))
try:
    s = frida.attach(pid); sc = s.create_script(js); sc.on("message", onmsg); sc.load(); time.sleep(5)
    s.detach()
except Exception as e:
    print("attach err", e)
open("scripts/research/xunlei_lite/out/frida_test_run.txt","w",encoding="utf-8").write("\n".join(buf))
p.terminate()
