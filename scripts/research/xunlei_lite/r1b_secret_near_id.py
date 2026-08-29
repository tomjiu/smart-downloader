#!/usr/bin/env python3
"""R1b: for each known client_id, dump context and look for adjacent
client_secret / client_version / device_id values (embedded config pairing)."""
import os

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)

with open(BIN, "rb") as f:
    data = f.read()

IDS = [
    "X9ibISwpIp8jQ4Ya", "XW-G4v1H72tgfJym", "XVJVzaJv8vKHzVCk",
    "XW5SkOhLDjnOZP7J", "Xqp0kJBXWhwaTpB6", "YGQTOphnGIuyiAxH",
    "XoL5lqbDWNW0e7QA", "Xp6vsxz_7IYVw2BB", "Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP", "Yd0zTVGrNJhCC2oL", "Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH", "Yd0y91GrNJhCC2oJ", "Yd00e1GrNJhCC2oR",
]

def try_decode(b):
    try:
        return b.decode("utf-8")
    except Exception:
        return b.decode("latin-1")

fn = os.path.join(OUT, "secret_near_id.txt")
with open(fn, "w", encoding="utf-8") as g:
    for cid in IDS:
        g.write(f"\n========== client_id {cid} ==========\n")
        start = 0
        occ = 0
        while True:
            idx = data.find(cid.encode("utf-8"), start)
            if idx < 0:
                break
            occ += 1
            ctx = data[max(0, idx-400):idx+600]
            g.write(f"\n--- occ #{occ} @ off={idx} ---\n")
            g.write(try_decode(ctx))
            g.write("\n")
            start = idx + 1
            if occ >= 6:
                break
        g.write(f"[occurrences shown: {occ}]\n")
print(f"[done] -> out/secret_near_id.txt")
