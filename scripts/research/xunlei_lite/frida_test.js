// frida_test.js - minimal rename-bypass + string pool dump
(function () {
  function hook(name) {
    try {
      var ex = Module.getExportByName("kernel32.dll", name);
      if (ex) {
        Interceptor.attach(ex, {
          onEnter: function () { this.skip = true; },
          onLeave: function (ret) { ret.replace(1); }
        });
        send("[*] hooked " + name);
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

  const KNOWN = [
    "X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Yqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ","Yd00e1GrNJhCC2oR"
  ];
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  setTimeout(function () {
    var results = {};
    for (var k = 0; k < KNOWN.length; k++) {
      var s = KNOWN[k];
      var addrs = [];
      try {
        Memory.scan(main.base, main.size, hex(s), {
          onMatch: function (address) { addrs.push(address.toString()); },
          onComplete: function () {}
        });
      } catch (e) { send("[err] scan " + s + ": " + e); }
      results[s] = addrs;
    }
    send("[*] LIVE addresses:\n" + JSON.stringify(results, null, 1));
    send("[*] done");
  }, 2000);
})();
