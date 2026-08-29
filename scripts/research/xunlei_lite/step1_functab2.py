#!/usr/bin/env python3
"""Step 1d: locate the Go functab by finding a long run of 8-byte aligned
pairs (funcOffset, nameOffset) where:
  - nameOffset in [0, NAMETAB_SIZE]
  - funcOffset in [0, TEXT_SIZE]
  - funcOffset is STRICTLY INCREASING across consecutive entries (functab is sorted)
  - resolving nameOffset yields a plausible funcsym (contains '.' or '(')
We then derive pcHeader (just before functab) and dump target functions.
"""
import struct
data = open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe", "rb").read()

NAMETAB_START = 0x140e0e2
NAMETAB_SIZE = 0x87b82b
TEXT_START_VA = 0x1000
TEXT_SIZE = 0x12fedb5

def name_at(off):
    e = data.find(b"\x00", off)
    if e < 0:
        e = off + 80
    return data[off:e].decode("utf-8", "replace")

SCAN_LO = NAMETAB_START + NAMETAB_SIZE
SCAN_HI = len(data) - 8

def valid_pair(fo, no):
    if not (0 < no <= NAMETAB_SIZE):
        return False
    if not (0 < fo <= TEXT_SIZE):
        return False
    nm = name_at(NAMETAB_START + no)
    if len(nm) < 4:
        return False
    if ("." not in nm) and ("(" not in nm):
        return False
    if " " in nm:
        return False
    return True

best_run = None
best_run_len = 0
# slide a window; compute max increasing run
prev_fo = -1
run_start = None
run_len = 0
# We scan candidate-start positions but efficiently check increasing runs.
i = SCAN_LO
# To bound time, only start runs where entry itself is valid
while i < SCAN_HI:
    fo, no = struct.unpack_from("<II", data, i)
    if valid_pair(fo, no):
        # begin a run here
        j = i
        cnt = 0
        last_fo = -1
        while j + 8 <= SCAN_HI:
            fo2, no2 = struct.unpack_from("<II", data, j)
            if not valid_pair(fo2, no2):
                break
            if fo2 <= last_fo:
                # not strictly increasing -> run ends (but this entry may start a new run)
                break
            last_fo = fo2
            cnt += 1
            j += 8
        if cnt > best_run_len:
            best_run_len = cnt
            best_run = (i, last_fo)
            print(f"[*] new best run @ {i:#x} len={cnt}")
        # jump to where run broke to continue, but to be safe advance modestly
        i = j
    else:
        i += 4

print(f"[*] best functab run: start={best_run[0]:#x} entries={best_run_len} lastFuncOff={best_run[1]:#x}")

if best_run and best_run_len > 50:
    start = best_run[0]
    # resolve names for a sample
    with open(r"E:\Code\ai\smart-downloader\scripts\research\xunlei_lite\out\functab_sample.txt", "w", encoding="utf-8") as g:
        for k in range(0, best_run_len, max(1, best_run_len // 200)):
            fo, no = struct.unpack_from("<II", data, start + k * 8)
            nm = name_at(NAMETAB_START + no)
            line = f"funcVA={TEXT_START_VA+fo:#010x} name={nm}"
            g.write(line + "\n")
            if k < 30:
                print("   ", line)
    # search targets
    targets = ["GetClientSecret", "GetClientID", "GetRawConfig", "Init", "With",
               "initNasId", "GetConfig", "GetRunnerType", "detectFile"]
    base = "gitlab.xunlei.cn/xlppc/pan-cli/pkg/platformdetect"
    found = {}
    for k in range(best_run_len):
        fo, no = struct.unpack_from("<II", data, start + k * 8)
        nm = name_at(NAMETAB_START + no)
        if nm.startswith(base):
            for t in targets:
                if nm.endswith("." + t) or nm == base + "." + t:
                    found[nm] = TEXT_START_VA + fo
                    break
    print("[*] target functions (VA):")
    for nm, va in sorted(found.items()):
        print(f"    {nm} -> VA {va:#010x}  RVA {va-TEXT_START_VA:#x}")
    import json
    json.dump({nm: hex(va) for nm, va in found.items()},
              open(r"E:\Code\ai\smart-downloader\scripts\research\xunlei_lite\out\target_funcs_va.json", "w"), indent=2)
