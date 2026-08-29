// frida_findid.js - find the TRUE runtime address of the pcxllite client_id string by scanning
// module memory for its UTF-8 bytes, then locate the LEA that references it and instrument the function.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  // candidate ids to locate
  const IDS = {
    "XW-G4v1H72tgfJym": "pcxllite(pcx)", // pc group
    "X9ibISwpIp8jQ4Ya": "pc(pc)",
    "XoL5lqbDWNW0e7QA": "h5",
    "Yd0uSVGrNJhCC2oE": "h5",
    "YGQTOphnGIuyiAxH": "candSecret"
  };
  var addrs = {};
  Process.enumerateRanges("--r").forEach(function (r) {
    if (r.base.compare(base) >= 0 && r.base.add(r.size).compare(base.add(main.size)) <= 0) {
      for (var name in IDS) {
        try {
          Memory.scan(r.base, r.size, hex(name), {
            onMatch: function (a) { if (!addrs[name]) addrs[name] = a.toString(); },
            onComplete: function () {}
          });
        } catch (e) {}
      }
    }
  });
  send("[*] runtime id addresses: " + JSON.stringify(addrs));

  // now find LEA -> each id addr and instrument the function
  for (var name in addrs) {
    var idAddr = ptr(addrs[name]);
    var leaHits = [];
    Process.enumerateRanges("x--").forEach(function (r) {
      if (r.base.compare(base) >= 0 && r.base.add(r.size).compare(base.add(main.size)) <= 0) {
        try {
          Memory.scan(r.base, r.size, "8d", {
            onMatch: function (a) {
              try {
                var modrm = a.add(1).readU8();
                if (((modrm >> 6) & 3) === 0 && (modrm & 7) === 5) {
                  var disp = a.add(2).readS32();
                  var target = a.add(6).add(disp);
                  if (target.compare(idAddr) === 0) leaHits.push(a);
                }
              } catch (e) {}
            },
            onComplete: function () {}
          });
        } catch (e) {}
      }
    });
    send("[*] " + name + " LEA hits: " + leaHits.length + " " + JSON.stringify(leaHits));

    // instrument each function prologue
    for (var k = 0; k < leaHits.length; k++) {
      var lea = leaHits[k];
      var prog = lea;
      for (var j = 1; j < 0x3000; j++) {
        var p = lea.sub(j);
        try {
          var b0=p.readU8(), b1=p.add(1).readU8(), b2=p.add(2).readU8();
          if (b0===0x55&&b1===0x89&&b2===0xe5){prog=p;break;}
          if (b0===0x83&&b1===0xec){prog=p;break;}
        } catch(e){break;}
      }
      try {
        Interceptor.attach(prog, {
          onEnter: function (args) {
            try {
              var n = args[0].readUtf8String(args[0].add(4).readU32());
              send("[CALL "+name+"] func@"+prog+" name="+JSON.stringify(n));
            } catch(e){ send("[CALL "+name+"] func@"+prog+" (name unreadable)"); }
          }
        });
      } catch (e) { send("[err attach "+prog+"] "+e); }
    }
  }
  send("[*] done");
})();
