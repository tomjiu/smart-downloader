data = open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe","rb").read()
# platform names that act as keys
for name in [b"pcxllite", b"synology", b"qnap", b"terramaster", b"nas", b"docker", b"pc", b"tv", b"h5", b"linux"]:
    i=0; c=0
    out=[]
    while c<6:
        j=data.find(name,i)
        if j<0: break
        a=max(0,j-60); b=min(len(data),j+260)
        out.append(hex(j)+" "+repr(data[a:b]))
        i=j+1; c+=1
    if out:
        print("==== "+name.decode()+" ====")
        for o in out:
            print(o)
        print()
