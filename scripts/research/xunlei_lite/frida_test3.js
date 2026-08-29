// frida_test3.js - directly read runtime string pool by computing VA from module base.
// module_base already includes ImageBase; a file VA (e.g. 0x1ab65b8) maps to
// runtime = module_base + VA  (because VA = ImageBase + rva, and module_base = ImageBase + loadDelta,
// but frida's module.base is the actual load address = ImageBase + loadDelta; so runtime = module_base + rva,
// NOT module_base + VA). We must subtract ImageBase (0x400000) to get rva.
(function () {
  const IB = 0x400000;
  const main = Process.enumerateModules()[0];
  function rvaOf(va) { return va - IB; }
  // candidate file offsets (from earlier analysis) -> VA
  // XW-G4v1H72tgfJym file 0x1659482 -> VA 0x1a5a282
  const CAND = {
    "XW-G4v1H72tgfJym": 0x1a5a282,
    "YGQTOphnGIuyiAxH": 0x1a5a2b2,
    "X9ibISwpIp8jQ4Ya": 0x1a5a232,
    "Yd0uSVGrNJhCC2oE": 0x1b59dd9,
    "banner getEnvs": 0x1ab0000 + (0x13223a5 - 0x1300000) // approximate
  };
  // safer: compute from the file offsets we know precisely
  // VA = 0x1300000 + (fileoff - 0x12ff200) for .rdata region
  function vaFromFileOff(foff) { return 0x1300000 + (foff - 0x12ff200); }
  const FILEOFFS = {
    "XW-G4v1H72tgfJym": 0x1659482,
    "YGQTOphnGIuyiAxH": 0x16594b2,
    "X9ibISwpIp8jQ4Ya": 0x1659432,
    "XVJVzaJv8vKHzVCk": 0x1659472,
    "XW5SkOhLDjnOZP7J": 0x1659492,
    "XoL5lqbDWNW0e7QA": 0x1b59daf,
    "Xqp0kJBXWhwaTpB6": 0x1b59e18,
    "Yd0uSVGrNJhCC2oE": 0x1b59dd9,
    "Yd00NFGrNJhCC2oP": 0x1b59dee,
    "Yd0zTVGrNJhCC2oL": 0x1b59e03,
    "Yd0zylGrNJhCC2oN": 0x1b59e2d,
    "Yd0yklGrNJhCC2oH": 0x1b59e42,
    "Yd0y91GrNJhCC2oJ": 0x1b59e57
  };
  for (var k in FILEOFFS) {
    CAND[k] = vaFromFileOff(FILEOFFS[k]);
  }

  send("[*] module base=" + main.base + " size=" + main.size);
  for (var name in CAND) {
    var va = CAND[name];
    var rva = va - IB;
    var addr = main.base.add(rva);
    try {
      var str = addr.readUtf8String(40);
      send("[*] " + name + " rva=" + rva.toString(16) + " @ " + addr + " => " + JSON.stringify(str));
    } catch (e) {
      send("[err] " + name + " rva=" + rva.toString(16) + " : " + e);
    }
  }
  send("[*] done");
})();
