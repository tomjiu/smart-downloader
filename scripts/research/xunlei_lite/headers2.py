data = open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe","rb").read()
# extract the header-name table region around 0x1645d00
s=0x1645c00; e=0x1646400
seg=data[s:e]
# split on non-ascii boundaries to find tokens
import re
for m in re.finditer(rb'[ -~]{3,}', seg):
    print(hex(s+m.start()), repr(m.group(0).decode('ascii')))
