#!/usr/bin/env python3
"""Step 2: find which function reads each candidate client_id/secret string, by
scanning .text for LEA reg, [disp32] whose target (VA) equals the candidate
string's absolute VA. In 386 Go, string literals are referenced via LEA with an
absolute (imagebase+...) or relative displacement. We cover both:
 - 8D xx disp32 : LEA r32, [disp32]  (absolute address in PE-without-reloc form? no)
 - 8D xx disp32 where disp32 is RELATIVE to instruction end (standard 386).
We compute target VA = inst_end + disp32 and compare to candidate string VAs.
Outputs, per candidate, the list of (funcRVA-ish) caller addresses and we cluster.
"""
import struct, pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_32

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
pe = pefile.PE(BIN, fast_load=False)
data = open(BIN, "rb").read()
IB = pe.OPTIONAL_HEADER.ImageBase
cs = Cs(CS_ARCH_X86, CS_MODE_32)
cs.detail = True

def absVA(off):
    for s in pe.sections:
        RO=s.PointerToRawData; RS=s.SizeOfRawData; VA=s.VirtualAddress
        if RO<=off<RO+RS:
            return IB + VA + (off-RO)
    return None

CAND = {
 "X9ibISwpIp8jQ4Ya":0x1659432,
 "XW-G4v1H72tgfJym":0x1659482,
 "XVJVzaJv8vKHzVCk":0x1659472,
 "XW5SkOhLDjnOZP7J":0x1659492,
 "YGQTOphnGIuyiAxH":0x16594b2,
 "XoL5lqbDWNW0e7QA":0x1758fd9+0,
 "Xqp0kJBXWhwaTpB6":0x1759018,
 "Yd0uSVGrNJhCC2oE":0x1758fd9,
 "Yd00NFGrNJhCC2oP":0x1758fee,
 "Yd0zTVGrNJhCC2oL":0x1759003,
 "Yd0zylGrNJhCC2oN":0x175902d,
 "Yd0yklGrNJhCC2oH":0x1759042,
 "Yd0y91GrNJhCC2oJ":0x1759057,
}
cand_va = {k: absVA(v) for k,v in CAND.items()}
print("[*] candidate string VAs:")
for k,v in cand_va.items():
    print(f"   {k} -> {v:#010x}")

# Disassemble only .text; find LEA targeting candidate VAs
txt_ro = 0x400
txt_rs = 0x12fee00
txt = data[txt_ro:txt_ro+txt_rs]
cand_set = set(cand_va.values())
hits = {c: [] for c in CAND}
# iterate functions roughly: capstone over whole .text is heavy but doable (50MB)
# To bound time, use quick regex for LEA opcode 8D then validate with capstone per hit.
import re
lea_re = re.compile(rb'\x8d')  # LEA opcode; many false positives but cheap pre-filter
count = 0
# Better: directly scan for 8D followed by ModRM 05 (disp32) or 15/35/3d etc with disp32
# 8D ModRM(05) disp32  -> LEA r32,[disp32]  ; operands we want
# 8D ModRM(0d) disp32  -> LEA r32,[disp32] (with SIB? no, 0x0d = /5 with no base? actually mod=01 rm=101 -> disp32)
# Common encodings for LEA reg,[abs]: 8D 05 disp32 (eax), 8D 15 disp32 (edx), 8D 1d, 8D 25, 8D 2d, 8D 35, 8D 3d
modrm05 = re.compile(rb'\x8d[\x05\x15\x1d\x25\x2d\x35\x3d](....)')
for m in modrm05.finditer(txt):
    off_in_text = m.start()
    disp = struct.unpack_from("<i", m.group(1), 0)[0]
    inst_end = txt_ro + off_in_text + 6
    target = IB + inst_end + disp  # absolute VA (imagebase + rva)
    if target in cand_set:
        caller = txt_ro + off_in_text
        name = [k for k,v in cand_va.items() if v==target][0]
        hits[name].append(caller)
print("[*] LEA-abs xref hits:")
for c, lst in hits.items():
    if lst:
        print(f"   {c}: {len(lst)} refs, first few fileoffs {[hex(x) for x in lst[:8]]}")

# Also try relative LEA where disp is the string RVA directly (no imagebase add) - some packers
print("[*] done LEA scan")
pe.close()
