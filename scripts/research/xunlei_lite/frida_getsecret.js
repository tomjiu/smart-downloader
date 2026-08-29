// frida_getsecret.js - locate GetClientSecret via its unique error string and instrument it.
// 1) Find runtime address of "PlatformConfig GetClientSecret empty. name:%v"
// 2) Scan .text (x--) for LEA reg,[disp32] -> that address (386: target = inst_end + disp32).
// 3) For each hit, find the function prologue (push ebp; mov ebp,esp  OR  sub esp,imm)
//    and instrument it: onEnter dump the `name` argument (Go string = (ptr,len) on stack);
//    onLeave, scan the caller return-slot / nearby stack for a (ptr,len) Go-string that is a
//    16-26 char mixed token (the secret). Deferred via setTimeout so load() returns fast.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;

  const ERRSTR = "PlatformConfig GetClientSecret empty. name:%v";
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  // collect readable + exec ranges
  var allR = [], xranges = [];
  ["r--","rw-"].forEach(function(prot){ try { Process.enumerateRanges(prot).forEach(function(r){
    if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) allR.push(r);
  }); } catch(e){} });
  ["x--","r-x"].forEach(function(prot){ try { Process.enumerateRanges(prot).forEach(function(r){
    if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) xranges.push(r);
  }); } catch(e){} });

  const KNOWN = ["X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Xqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ","Yd00e1GrNJhCC2oR"];
  const knownSet={}; KNOWN.forEach(function(k){knownSet[k]=1;});

  function isMixed(t){let u=0,l=0,d=0;for(let i=0;i<t.length;i++){const c=t.charCodeAt(i);if(c>=65&&c<=90)u++;else if(c>=97&&c<=122)l++;else if(c>=48&&c<=57)d++;else if(c!==95&&c!==45)return false;}return u>0&&l>0&&d>0;}

  setTimeout(function () {
    // Scan all readable + exec module ranges for the raw error-string bytes.
    var ph = hex(ERRSTR);
    var errAddr = null;
    var scanRanges = [];
    ["r--","rw-","r-x","x--"].forEach(function(prot){ try { Process.enumerateRanges(prot).forEach(function(r){
      if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) scanRanges.push(r);
    }); } catch(e){} });
    scanRanges.forEach(function(r){
      if (errAddr) return;
      try { Memory.scan(r.base, r.size, ph, {
        onMatch:function(a){ errAddr=a; }, onComplete:function(){}
      }); } catch(e){}
    });
    if (!errAddr) { send("[!] error string not found by scan"); return; }
    send("[*] GetClientSecret error string @ " + errAddr);

    // find LEA -> errAddr in all exec module ranges
    var leaHits = [];
    scanRanges.forEach(function(r){
      if (!(r.protection.indexOf("x") >= 0)) return; // only executable ranges hold code
      try { Memory.scan(r.base, r.size, "8d", {
        onMatch: function(a){
          try {
            var modrm = a.add(1).readU8();
            if (((modrm>>6)&3)===0 && (modrm&7)===5) { // mod=00,rm=101 -> disp32
              var disp = a.add(2).readS32();
              var target = a.add(6).add(disp);
              if (target.compare(errAddr)===0) leaHits.push(a);
            }
          } catch(e){}
        },
        onComplete:function(){}
      }); } catch(e){}
    });
    send("[*] LEA->errorstring hits: " + leaHits.length + " " + JSON.stringify(leaHits));

    leaHits.forEach(function(lea){
      // find prologue scanning backward
      var prog = lea;
      for (var j=1;j<0x4000;j++){ var p=lea.sub(j); try{ var b0=p.readU8(),b1=p.add(1).readU8(),b2=p.add(2).readU8();
        if (b0===0x55&&b1===0x89&&b2===0xe5){prog=p;break;} if(b0===0x83&&b1===0xec){prog=p;break;} }catch(e){break;} }
      try {
        Interceptor.attach(prog, {
          onEnter: function(args){
            // Go 386: string arg = (data ptr @ args[0], len @ args[1])
            try {
              var len = args[1].toInt32 ? args[1].toInt32() : args[1].toUInt32();
              if (len>0 && len<256) { var nm = args[0].readUtf8String(len); send("[GetClientSecret CALL] name="+JSON.stringify(nm)+" arg0="+args[0]); this._nm=nm; }
              else send("[GetClientSecret CALL] name len weird="+len);
            } catch(e){ send("[GetClientSecret CALL] name read err "+e); }
          },
          onLeave: function(){
            // best-effort: scan caller stack region [esp..esp+0x200] for a (ptr,len) Go string
            // that is a mixed 16-26 token not a known id. Go 386 returns string via caller slot;
            // we scan the function's saved-frame area just above return addr.
            try {
              var sp = this.context.esp;
              for (var o=0; o<0x300; o+=4) {
                var ptr = sp.add(o).readPointer();
                var len = sp.add(o+4).readU32();
                if (len>=16 && len<=26 && ptr.compare(base)>0 && ptr.compare(base.add(main.size))<0) {
                  try {
                    var s = ptr.readUtf8String(len);
                    if (isMixed(s) && !knownSet[s]) { send("[GetClientSecret RETURN?] secret="+JSON.stringify(s)+" at stackoff+"+o+" name="+this._nm); }
                  } catch(e){}
                }
              }
            } catch(e){ send("[GetClientSecret onLeave scan err "+e); }
          }
        });
        send("[*] hooked GetClientSecret prologue @ " + prog + " (lea="+lea+")");
      } catch(e){ send("[err attach "+prog+"] "+e); }
    });
    send("[*] instrumentation done; awaiting calls (daemon may need to make an API call)");
  }, 800);
})();
