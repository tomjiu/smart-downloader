// frida_test2.js - fix hook return + verify Memory.scan
(function () {
  function hook(name) {
    try {
      var ex = Module.getExportByName("kernel32.dll", name);
      if (ex) {
        Interceptor.attach(ex, {
          onEnter: function () {},
          onLeave: function (ret) { try { ret.replace(ptr("0x1")); } catch (e) { send("[err] replace " + name + ": " + e); } }
        });
        send("[*] hooked " + name + " @ " + ex);
      } else {
        send("[!] " + name + " not found");
      }
    } catch (e) { send("[err] hook " + name + ": " + e); }
  }
  hook("MoveFileExW");
  hook("MoveFileExA");
  hook("MoveFileW");
  hook("MoveFileA");
  hook("ReplaceFileW");

  const main = Process.enumerateModules()[0];
  send("[*] module " + main.name + " base=" + main.base + " size=" + main.size);

  // verify scan with a known unique banner string present at startup
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  setTimeout(function () {
    // test 1: banner
    var banner = "getEnvs succ";
    var baddr = [];
    try {
      Memory.scan(main.base, main.size, hex(banner), {
        onMatch: function (a) { baddr.push(a.toString()); },
        onComplete: function () {}
      });
    } catch (e) { send("[err] scan banner: " + e); }
    send("[*] banner '" + banner + "' hits: " + baddr.length + " " + JSON.stringify(baddr));

    // test 2: known client id
    var KNOWN = ["XW-G4v1H72tgfJym","YGQTOphnGIuyiAxH","X9ibISwpIp8jQ4Ya","Yd0uSVGrNJhCC2oE"];
    var results = {};
    for (var k = 0; k < KNOWN.length; k++) {
      var s = KNOWN[k];
      var addrs = [];
      try {
        Memory.scan(main.base, main.size, hex(s), {
          onMatch: function (a) { addrs.push(a.toString()); },
          onComplete: function () {}
        });
      } catch (e) { send("[err] scan " + s + ": " + e); }
      results[s] = addrs;
    }
    send("[*] KNOWN hits: " + JSON.stringify(results));

    // test 3: list readable ranges within module
    var ranges = [];
    try {
      Process.enumerateRanges("r--").forEach(function (r) {
        if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) {
          ranges.push(r.base.toString() + "+" + r.size + " " + r.protection);
        }
      });
    } catch (e) { send("[err] ranges: " + e); }
    send("[*] readable module ranges: " + ranges.length);
    send("[*] done");
  }, 2000);
})();
