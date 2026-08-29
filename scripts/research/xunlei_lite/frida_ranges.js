// frida_ranges.js - dump module memory ranges and locate the client_id string by scanning each
// readable range sequentially with a correct byte pattern.
(function () {
  const main = Process.enumerateModules()[0];
  send("[*] base=" + main.base + " size=" + main.size);
  var ranges = [];
  Process.enumerateRanges("r--").forEach(function (r) {
    if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) {
      ranges.push(r.base.toString(16) + "+" + r.size.toString(16) + " " + r.protection);
    }
  });
  send("[*] module readable ranges (" + ranges.length + "):\n" + ranges.join("\n"));

  // scan each readable range for "XW-G4v1H72tgfJym"
  var pat = "XW-G4v1H72tgfJym";
  var ph = "";
  for (var i=0;i<pat.length;i++) ph += ("0"+pat.charCodeAt(i).toString(16)).slice(-2);
  var hits = [];
  Process.enumerateRanges("r--").forEach(function (r) {
    if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) {
      try {
        Memory.scan(r.base, r.size, ph, {
          onMatch: function (a) { hits.push(a.toString()); },
          onComplete: function () {}
        });
      } catch (e) { send("[err scan] " + e); }
    }
  });
  send("[*] 'XW-G4v1H72tgfJym' hits: " + JSON.stringify(hits));
  for (var h = 0; h < hits.length; h++) {
    var a = ptr(hits[h]);
    try { send("   @" + a + " => " + a.readUtf8String(40)); } catch(e){ send("   @"+a+" err "+e); }
  }
  send("[*] done");
})();
