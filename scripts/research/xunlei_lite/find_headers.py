import struct
data = open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe","rb").read()
out=[]
for s in [b"x-client-secret", b"x-client-id", b"x-client-version", b"x-client-type"]:
    i=0; c=0
    while c<8:
        j=data.find(s,i)
        if j<0: break
        a=max(0,j-40); b=min(len(data),j+120)
        out.append(hex(j)+" "+repr(data[a:b]))
        i=j+1; c+=1
    out.append("---")
open(r"E:\Code\ai\smart-downloader\scripts\research\xunlei_lite\out\headers_find.txt","w",encoding="utf-8").write("\n".join(out))
print("\n".join(out[:60]))
