// frida_va.js - determine correct runtime mapping for the client_id string region and dump the
// real string pool around each candidate, using runtime = base + file_offset (proven for file_play).
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;
  function rstr(foff, len) {
    var a = base.add(foff);
    try {
      var out = "";
      for (var i = 0; i < len; i++) {
        var b = a.add(i).readU8();
        if (b === 0) break;
        out += String.fromCharCode(b);
      }
      return out;
    } catch (e) { return "<err:" + e + ">"; }
  }
  const FOFFS = {
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
  setTimeout(function () {
    for (var n in FOFFS) {
      send("[*] " + n + " @base+foff => " + JSON.stringify(rstr(FOFFS[n], 40)));
    }
    send("[*] file_play region check @0x1758f00 => " + JSON.stringify(rstr(0x1758f00, 60)));
    send("[*] done");
  }, 1500);
})();
