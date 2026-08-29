#!/usr/bin/env python3
# Spawn a fresh xllite (run mode) with rename-bypass pre-installed, then immediately
# hook GetClientSecret via the error-string LEA, and capture (name -> secret) pairs live.
import os, time, frida

env = dict(os.environ)
env["PLATFORM"] = "pcxllite"
exe = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
wd = r"C:\xlrun"

JS = r'''
(function () {
  function bypass(name) {
    try {
      var ex = Module.getGlobalExportByName(name);
      if (ex && !ex.isNull()) {
        Interceptor.attach(ex, { onEnter:function(){}, onLeave:function(r){ try{r.replace(ptr("0x1"));}catch(e){} } });
        return true;
      }
    } catch (e) {}
    return false;
  }
  ["MoveFileExW","MoveFileExA","MoveFileW","MoveFileA","ReplaceFileW"].forEach(function(n){ if(bypass(n)) send("[*] bypass "+n); });

  const main = Process.enumerateModules()[0];
  const base = main.base;
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  const KNOWN = ["X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Xqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ","Yd00e1GrNJhCC2oR"];
  const knownSet={}; KNOWN.forEach(function(k){knownSet[k]=1;});
  function isMixed(t){let u=0,l=0,d=0;for(let i=0;i<t.length;i++){const c=t.charCodeAt(i);if(c>=65&&c<=90)u++;else if(c>=97&&c<=122)l++;else if(c>=48&&c<=57)d++;else if(c!==95&&c!==45)return false;}return u>0&&l>0&&d>0;}

  var ERRSTR = "PlatformConfig GetClientSecret empty. name:%v";
  var ph = hex(ERRSTR);

  // Enumerate module ranges (any protection)
  function modRanges(){ var out=[]; ["r--","rw-","r-x","x--","rwx"].forEach(function(p){try{Process.enumerateRanges(p).forEach(function(r){if(r.base.compare(base)>=0&&r.base.add(r.size).compare(base.add(main.size))<=0)out.push(r);});}catch(e){}}); return out; }

  setTimeout(function(){
    var ranges = modRanges();
    var errAddr=null;
    ranges.forEach(function(r){ if(errAddr)return; try{Memory.scan(r.base,r.size,ph,{onMatch:function(a){errAddr=a;},onComplete:function(){}});}catch(e){} });
    if(!errAddr){ send("[!] errstr not found (ranges="+ranges.length+")"); return; }
    send("[*] GetClientSecret errstr @ "+errAddr);
    // LEA scan in exec ranges
    var leaHits=[];
    ranges.forEach(function(r){ if(r.protection.indexOf("x")<0)return; try{Memory.scan(r.base,r.size,"8d",{onMatch:function(a){try{var m=a.add(1).readU8();if(((m>>6)&3)===0&&(m&7)===5){var d=a.add(2).readS32();var t=a.add(6).add(d);if(t.compare(errAddr)===0)leaHits.push(a);}}catch(e){}},onComplete:function(){}});}catch(e){} });
    send("[*] LEA hits: "+leaHits.length+" "+JSON.stringify(leaHits));
    leaHits.forEach(function(lea){
      var prog=lea;
      for(var j=1;j<0x4000;j++){var p=lea.sub(j);try{var b0=p.readU8(),b1=p.add(1).readU8(),b2=p.add(2).readU8();if(b0===0x55&&b1===0x89&&b2===0xe5){prog=p;break;}if(b0===0x83&&b1===0xec){prog=p;break;}}catch(e){break;}}
      try{
        Interceptor.attach(prog,{
          onEnter:function(args){
            try{var len=args[1].toInt32?args[1].toInt32():args[1].toUInt32();if(len>0&&len<256){var nm=args[0].readUtf8String(len);this._nm=nm;send("[CALL] GetClientSecret name="+JSON.stringify(nm));}else this._nm=null;}catch(e){this._nm=null;send("[CALL] name err "+e);}
          },
          onLeave:function(){
            try{
              var sp=this.context.esp;
              for(var o=0;o<0x400;o+=4){var ptr2=sp.add(o).readPointer();var len2=sp.add(o+4).readU32();if(len2>=16&&len2<=26&&ptr2.compare(base)>0&&ptr2.compare(base.add(main.size))<0){try{var ss=ptr2.readUtf8String(len2);if(isMixed(ss)&&!knownSet[ss]){send("[SECRET?] name="+this._nm+" secret="+JSON.stringify(ss)+" stackoff+"+o);}}catch(e){}}}
            }catch(e){send("[onLeave scan err] "+e);}
          }
        });
        send("[*] hooked func@"+prog+" lea="+lea);
      }catch(e){send("[attach err "+prog+"] "+e);}
    });
    send("[*] instrumented; awaiting calls");
  }, 2500);
})();
'''

buf = []
def on(m, d):
    if m.get("type") == "send":
        try: print("  FRIDA:", m["payload"][:600])
        except: buf.append(str(m["payload"]))
        buf.append(m["payload"])
    elif m.get("type") == "error":
        print("  ERR:", m.get("description"))

pid = frida.spawn([exe, "run"], cwd=wd, env=env)
print("spawned", pid)
ses = frida.attach(pid)
sc = ses.create_script(JS)
sc.on("message", on)
sc.load()
frida.resume(pid)
time.sleep(20)
try: ses.detach()
except: pass
open("scripts/research/xunlei_lite/out/frida_spawn_getsecret_out.txt","w",encoding="utf-8").write("\n".join(buf))
print("wrote", len(buf))
