// frida_cred.py JS payload: runtime extraction of client_id/client_secret for platformdetect.
// Strategy: xllite is a relocatable Go 1.18/386 binary; pclntab is stripped so we cannot
// resolve symbols statically. Instead we:
//  1) Locate the embedded string pool by scanning module memory for a known client_id literal.
//  2) Dump a 8KB window around it, and scan the whole module for EVERY occurrence of the
//     known candidate ids/secrets, recording their live (relocated) addresses + neighbors,
//     to reconstruct the credential pool physically.
//  3) Hook getenv/GetEnvironmentVariableA/W is not useful; instead intercept the function that
//     returns the client_id/secret by scanning for code that LEA's the known client_id address.
//     On 386 the string is referenced by LEA reg,[disp32]; the disp32 is relocated. We find the
//     live address of the string, then scan .text for the LEA instruction whose relocated target
//     equals that address, and instrument it. Simpler & robust: just dump the pool + log every
//     time the known client_id address is loaded (watchpoint-free: we patch the LEA's destination
//     reads via Memory.scan for the absolute address inside code is impractical). 
//  => Pragmatic: rely on + run the binary with -d (debug) and capture actual API calls; AND dump
//     the relocated string pool to recover sibling secrets.
(function () {
  const KNOWN = [
    "X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Yqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ","Yd00e1GrNJhCC2oR"
  ];
  function sendObj(tag, o) { send(tag + " " + JSON.stringify(o)); }

  const mods = Process.enumerateModules();
  const main = mods[0];
  sendObj("[*] main module", {name: main.name, base: main.base, size: main.size});

  // 1) locate each known string in live memory, record address + surrounding 64 bytes (ascii)
  let found = [];
  for (const s of KNOWN) {
    const addrs = [];
    const needle = s;
    // Memory.scan for the UTF-8 bytes
    try {
      Memory.scan(main.base, main.size, toHex(needle), {
        onMatch(address, size) {
          addrs.push(address.toString());
        },
        onComplete() {}
      });
    } catch (e) { sendObj("[err] scan", {s: s, e: "" + e}); }
    found.push({s: s, addrs: addrs});
  }
  // Need to wait for async scan; use a small delay via setTimeout
  setTimeout(function () {
    sendObj("[*] KNOWN string live addresses", found);
    // 2) for the pcxllite client_id we already know (XW-G4v1H72tgfJym), dump a window
    for (const f of found) {
      if (f.addrs.length) {
        const a = ptr(f.addrs[0]);
        const win = a.sub(256);
        try {
          const buf = win.readByteArray(1024);
          // ascii print
          send("[*] window@" + win + " for " + f.s + "\n" + hexAscii(buf));
        } catch (e) { send("[err] window " + f.s + " " + e); }
      }
    }
    // 3) hook GetEnvironmentVariable? no. Instead, intercept the HTTP header builder:
    // search for WinHttp/CreateThread not needed. We rely on string pool + the fact that
    // GetClientSecret returns the secret; we cannot resolve it without symbol.
    // Final: just report the pool reconstruction.
    send("[*] frida_cred done");
  }, 1500);

  function toHex(str) {
    let out = "";
    for (let i = 0; i < str.length; i++) {
      out += ("0" + str.charCodeAt(i).toString(16)).slice(-2);
    }
    return out;
  }
  function hexAscii(buf) {
    const u8 = new Uint8Array(buf);
    let lines = [];
    for (let off = 0; off < u8.length; off += 32) {
      let hex = "", asc = "";
      for (let i = 0; i < 32 && off + i < u8.length; i++) {
        const b = u8[off + i];
        hex += ("0" + b.toString(16)).slice(-2) + " ";
        asc += (b >= 32 && b < 127) ? String.fromCharCode(b) : ".";
      }
      lines.push(("00000000" + off.toString(16)).slice(-8) + "  " + hex + " " + asc);
    }
    return lines.join("\n");
  }
})();
