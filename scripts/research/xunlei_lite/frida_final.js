// frida_final.js - attach to the long-running daemon (decrypt stub already done) and locate the
// client_id literal. The string may now be readable. Also hook the function that prints the
// detect line ("detect platform: %s %s nasid:") which uses the client_id -> that reveals the
// GetClientID caller. And patch the Go map lookup by intercepting runtime string comparison?
// Pragmatic: find LEA->client_id in .text and instrument; if string now readable, the LEA target
// will resolve and we can hook the enclosing GetClientID/GetClientSecret function and dump returns.
(function () {
  const main = Process.enumerateModules()[0];
  send("[*] base=" + main.base + " size=" + main.size);

  function goStr(a) {
    try { var len = a.add(4).readU32(); if (len<=0||len>8192) return null; return a.readUtf8String(len); }
    catch(e){ return null; }
  }
  // enumerate ALL ranges belonging to module (any protection)
  var all = [];
  ["r--","rw-","r-x","rwx","-w-","--x"].forEach(function (prot) {
    try {
      Process.enumerateRanges(prot).forEach(function (r) {
        if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) all.push(r);
      });
    } catch(e){}
  });
  send("[*] total module ranges scanned: " + all.length);

  function scanStr(pat, cb) {
    var ph = ""; for (var i=0;i<pat.length;i++) ph += ("0"+pat.charCodeAt(i).toString(16)).slice(-2);
    all.forEach(function (r) {
      try {
        Memory.scan(r.base, r.size, ph, {
          onMatch: function (a) { cb(a, r); },
          onComplete: function () {}
        });
      } catch(e){}
    });
  }

  // locate candidate ids now
  var IDS = {
    "XW-G4v1H72tgfJym":"pcxllite", "X9ibISwpIp8jQ4Ya":"pc", "XoL5lqbDWNW0e7QA":"h5",
    "Yd0uSVGrNJhCC2oE":"h5", "YGQTOphnGIuyiAxH":"cand", "Xqp0kJBXWhwaTpB6":"h5",
    "Yd0y91GrNJhCC2oJ":"h5", "Yd0zylGrNJhCC2oN":"h5"
  };
  var idAddr = {};
  IDS && Object.keys(IDS).forEach(function (name) {
    scanStr(name, function (a, r) { if (!idAddr[name]) { idAddr[name] = a; send("[*] FOUND "+name+" @ "+a+" prot="+r.protection); } });
  });
  // give scan time then proceed
  setTimeout(function () {
    var keys = Object.keys(idAddr);
    send("[*] ids found at runtime: " + keys.length + " / " + Object.keys(IDS).length);
    if (keys.length === 0) { send("[*] string pool still not readable -> obfuscated; cannot hook by LEA"); send("[*] done"); return; }
    // for the pcxllite id, find LEA in .text (x--) and hook function
    var idA = idAddr["XW-G4v1H72tgfJym"];
    var leaHits = [];
    Process.enumerateRanges("x--").forEach(function (r) {
      if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) {
        try {
          Memory.scan(r.base, r.size, "8d", {
            onMatch: function (a) {
              try {
                var modrm = a.add(1).readU8();
                if (((modrm>>6)&3)===0 && (modrm&7)===5) {
                  var disp = a.add(2).readS32();
                  if (a.add(6).add(disp).compare(idA)===0) leaHits.push(a);
                }
              } catch(e){}
            },
            onComplete: function () {}
          });
        } catch(e){}
      }
    });
    send("[*] LEA->pcxllite client_id: " + leaHits.length);
    leaHits.forEach(function (lea) {
      var prog = lea;
      for (var j=1;j<0x3000;j++){ var p=lea.sub(j); try{ var b0=p.readU8(),b1=p.add(1).readU8(),b2=p.add(2).readU8();
        if (b0===0x55&&b1===0x89&&b2===0xe5){prog=p;break;} if(b0===0x83&&b1===0xec){prog=p;break;} }catch(e){break;} }
      try {
        Interceptor.attach(prog, {
          onEnter: function (args) {
            try { var n = goStr(args[0]); send("[CALL GetClientX] func@"+prog+" name="+JSON.stringify(n)+" arg0="+args[0]); }
            catch(e){ send("[CALL] func@"+prog+" err "+e); }
          },
          onLeave: function () { send("[RET] func@"+prog); }
        });
        send("[*] hooked func@"+prog+" (lea="+lea+")");
      } catch(e){ send("[err attach "+prog+"] "+e); }
    });
    send("[*] done; awaiting calls");
  }, 3000);
})();
