// frida_run.js - runtime extraction of platformdetect credentials.
// 1) Bypass xllite's cwd-rename self-test by forcing MoveFileEx*/MoveFile*/ReplaceFile* to succeed.
// 2) Once 'run' proceeds, locate the embedded client_id string pool and dump a window around the
//    pcxllite client_id (XW-G4v1H72tgfJym) and around the candidate secret YGQTOphnGIuyiAxH, to
//    recover sibling credentials physically.
// 3) Hook the function that reads the client_id constant by scanning .text for LEA targeting its
//    runtime address (386: 8D xx disp32, target = inst_end + disp32). When found, instrument it:
//    on entry, log + dump caller; also place a Memory.readUtf8String on the resolved string. Since
//    we cannot easily resolve the function name, we instead intercept the HTTP header injection:
//    xllite sets x-client-id via a Go string => we scan for code that loads XW-G4v1H72tgfJym and
//    instrument the closest function prologue.
// Simpler robust extraction: just dump the pool windows and report the runtime VA of each known id,
// plus a 2KB window around the pcxllite id and around YGQTOphnGIuyiAxH. The window reveals pairing.

(function () {
  function hookOk(name) {
    try {
      var ex = Module.getGlobalExportByName(name);
      if (ex && !ex.isNull()) {
        Interceptor.attach(ex, {
          onEnter: function () {},
          onLeave: function (ret) { try { ret.replace(ptr("0x1")); } catch (e) { send("[err] replace " + name + ": " + e); } }
        });
        return true;
      }
    } catch (e) { send("[err] hook " + name + ": " + e); }
    return false;
  }
  var hooked = 0;
  ["MoveFileExW","MoveFileExA","MoveFileW","MoveFileA","ReplaceFileW","ReplaceFileExW"].forEach(function (n) {
    if (hookOk(n)) { send("[*] bypass " + n); hooked++; }
    else send("[!] cannot hook " + n);
  });
  send("[*] bypass installed count=" + hooked);

  const main = Process.enumerateModules()[0];
  const IB = 0x400000;
  function vaFromFileOff(foff) { return 0x1300000 + (foff - 0x12ff200); }

  const FILEOFFS = {
    "X9ibISwpIp8jQ4Ya": 0x1659432,
    "XVJVzaJv8vKHzVCk": 0x1659472,
    "XW-G4v1H72tgfJym": 0x1659482,
    "XW5SkOhLDjnOZP7J": 0x1659492,
    "YGQTOphnGIuyiAxH": 0x16594b2,
    "XoL5lqbDWNW0e7QA": 0x1b59daf,
    "Xp6vsxz_7IYVw2BB": 0x1b59dc4,
    "Yd0uSVGrNJhCC2oE": 0x1b59dd9,
    "Yd00NFGrNJhCC2oP": 0x1b59dee,
    "Yd0zTVGrNJhCC2oL": 0x1b59e03,
    "Xqp0kJBXWhwaTpB6": 0x1b59e18,
    "Yd0zylGrNJhCC2oN": 0x1b59e2d,
    "Yd0yklGrNJhCC2oH": 0x1b59e42,
    "Yd0y91GrNJhCC2oJ": 0x1b59e57
  };

  function readStr(addr, maxlen) {
    try {
      var out = "";
      for (var i = 0; i < maxlen; i++) {
        var b = addr.add(i).readU8();
        if (b === 0) break;
        out += String.fromCharCode(b);
      }
      return out;
    } catch (e) { return "<err:" + e + ">"; }
  }
  function hexdump(addr, len) {
    try {
      var u8 = new Uint8Array(addr.readByteArray(len));
    } catch (e) { return "<err:" + e + ">"; }
    var lines = [];
    for (var off = 0; off < u8.length; off += 32) {
      var h = "", a = "";
      for (var i = 0; i < 32 && off + i < u8.length; i++) {
        var b = u8[off + i];
        h += ("0" + b.toString(16)).slice(-2) + " ";
        a += (b >= 32 && b < 127) ? String.fromCharCode(b) : ".";
      }
      lines.push(("00000000" + off.toString(16)).slice(-8) + "  " + h + " " + a);
    }
    return lines.join("\n");
  }

  send("[*] module base=" + main.base);
  setTimeout(function () {
    // dump each known id's runtime string + a 256-byte window before/after
    for (var name in FILEOFFS) {
      var va = vaFromFileOff(FILEOFFS[name]);
      var rva = va - IB;
      var addr = main.base.add(rva);
      var preview = readStr(addr, 32);
      send("[*] " + name + " rva=" + rva.toString(16) + " @ " + addr + " => " + JSON.stringify(preview));
    }
    // big windows around pcxllite id and around YGQTOphnGIuyiAxH
    var targets = {
      "XW-G4v1H72tgfJym@pcxllite": vaFromFileOff(0x1659482),
      "YGQTOphnGIuyiAxH(candSecret)": vaFromFileOff(0x16594b2),
      "X9ibISwpIp8jQ4Ya": vaFromFileOff(0x1659432),
      "pcxllite_pool@0x1b59dd9": vaFromFileOff(0x1b59dd9)
    };
    for (var t in targets) {
      var r = targets[t] - IB;
      var a = main.base.add(r);
      send("[*] WINDOW " + t + " @ " + a + " (-512..+512):\n" + hexdump(a.sub(512), 1024));
    }
    send("[*] done");
  }, 4000);
})();
