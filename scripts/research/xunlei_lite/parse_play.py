import re
data=open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe","rb").read()
seg=data[0x1758f00:0x1759b00]
txt=seg.decode("utf-8","replace")
# The string on disk has literal backslash-escaped JSON: \\" means " and \\\\ means \
# So to get real JSON we reverse the escaping that the compiler applied to the string literal:
#   on disk:  \"  -> represents "  ;  \\  -> represents \
# Easiest: replace the doubly-escaped forms. The on-disk text uses "\\\"" for an embedded quote.
# We saw earlier the raw disk text: \",\\\"matchAny\\\"... => after 1 level of unescape: ","matchAny",...
norm = txt.replace('\\\\','\x00').replace('\\"','"').replace('\x00','\\')
print("=== x-client-id groups ===")
for m in re.finditer(r'x-client-id","in",\[(.*?)\]', norm):
    ids = re.findall(r'"([A-Za-z0-9_\-]+)"', m.group(1))
    print("  client-ids:", ids)
print("=== platform groups ===")
for m in re.finditer(r'"platform","(in|notIn)",\[(.*?)\]', norm):
    plats = re.findall(r'"([a-z0-9]+)"', m.group(2))
    print("  ", m.group(1), ":", plats)
# Also find which desc groups the pcxllite platform appears in
print("=== desc blocks mentioning pcxllite ===")
for m in re.finditer(r'"desc":"([^"]*)"', norm):
    if 'pcxllite' in m.group(1) or 'pc' == m.group(1):
        print("  desc:", m.group(1))
