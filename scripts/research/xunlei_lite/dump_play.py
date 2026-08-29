data=open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe","rb").read()
# The file_play block. Earlier raw disk read showed the JSON clearly with \" escapes.
# Print it verbatim in chunks so we can read the groupings.
start=0x1758f00
chunk=data[start:start+0x900]
# Replace raw backslash-quote sequences for readability: keep as-is but print
import sys
out=chunk.decode("utf-8","replace")
open(r"E:\Code\ai\smart-downloader\scripts\research\xunlei_lite\out\file_play_raw.txt","w",encoding="utf-8").write(out)
print("written out/file_play_raw.txt, length", len(out))
# Also extract just the client id tokens and the platform tokens with their preceding context
import re
# find all quoted 16-char ids
ids=re.findall(rb'"([A-Za-z0-9_\-]{16,24})"', chunk)
print("ids found:", [i.decode() for i in ids])
# find all "platform","in",["x","y"...] style
for m in re.finditer(rb'platform\\?",\\?"(in|notIn)\\?",\\?\[([^\]]*)\]', chunk):
    print("PLAT", m.group(1).decode(), m.group(2)[:200])
