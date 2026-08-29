// frida_hook_secret.js - attach to running xllite, find the function that reads the pcxllite
// client_id constant (XW-G4v1H72tgfJym) by scanning .text for LEA reg,[disp32] -> that address,
// instrument the function prologue, and on every invocation dump the function's name-resolving
// argument + all returned Go-strings (ptr,len pairs) so we recover client_id/client_secret.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;
  const ID_FOFF = 0x1659482;
  const idAddr = base.add(ID_FOFF);
  send("[*] base=" + base + " client_id @ " + idAddr);

  function goStr(addr) {
    try {
      var len = addr.add(4).readU32();
      if (len <= 0 || len > 4096) return null;
      return addr.readUtf8String(len);
    } catch (e) { return null; }
  }
  function hexdump(a, n) {
    try { var u = new Uint8Array(a.readByteArray(n)); var s=""; for (var i=0;i<u.length;i++) s+=("0"+u[i].toString(16)).slice(-2)+" "; return s; }
    catch(e){ return "<err>"; }
  }

  // 1) find LEA -> client_id
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
  send("[*] LEA->client_id hits: " + leaHits.length);
  for (var i = 0; i < leaHits.length; i++) {
    var lea = leaHits[i];
    // find prologue: scan backward for 55 89 E5 (push ebp; mov ebp,esp) or 83 EC xx
    var prog = lea;
    for (var j = 1; j < 0x3000; j++) {
      var p = lea.sub(j);
      try {
        var b0 = p.readU8(), b1 = p.add(1).readU8(), b2 = p.add(2).readU8();
        if (b0 === 0x55 && b1 === 0x89 && b2 === 0xe5) { prog = p; break; }
        if (b0 === 0x83 && b1 === 0xec) { prog = p; break; } // sub esp,imm
      } catch (e) { break; }
    }
    send("[*] func@" + prog + " (lea=" + lea + ")");
    // instrument: on entry dump args; on leave dump return Go-strings at stack/registers
    try {
      Interceptor.attach(prog, {
        onEnter: function (args) {
          this.args = args;
          try {
            var nameStr = goStr(args[0]);
            send("[CALL] func@" + prog + " arg0(name)=" + JSON.stringify(nameStr) + " arg0ptr=" + args[0]);
          } catch (e) { send("[CALL] func@" + prog + " arg0 err " + e); }
        },
        onLeave: function (ret) {
          // Go returns structs in caller frame; for a (*string) return we can't easily read.
          // Instead, dump the first few qwords after the call site return area is hard.
          // Best effort: read args[0] again (name) and scan the function's local string slots:
          // just report the name we saw; the caller likely stores the secret in a struct we capture elsewhere.
          send("[RET] func@" + prog);
        }
      });
    } catch (e) { send("[err] attach " + prog + ": " + e); }
  }
  send("[*] instrumentation done; waiting for calls");
})();
