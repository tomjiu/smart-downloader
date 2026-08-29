// frida_ranges2.js - scan ALL module ranges (incl rw-) for the client_id literal, since the
// binary appears to decrypt strings into writable memory at runtime.
(function () {
  const main = Process.enumerateModules()[0];
  var pat = "XW-G4v1H72tgfJym";
  var ph = "";
  for (var i=0;i<pat.length;i++) ph += ("0"+pat.charCodeAt(i).toString(16)).slice(-2);
  var hits = [];
  ["r--","rw-","rwx"].forEach(function (prot) {
    Process.enumerateRanges(prot).forEach(function (r) {
      if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) {
        try {
          Memory.scan(r.base, r.size, ph, {
            onMatch: function (a) { hits.push(r.protection+"@"+a.toString()); },
            onComplete: function () {}
          });
        } catch (e) {}
      }
    });
  });
  send("[*] ALL-range hits for client_id: " + JSON.stringify(hits));

  // also try a few other candidate ids to see which are present
  var ids = ["X9ibISwpIp8jQ4Ya","XoL5lqbDWNW0e7QA","Yd0uSVGrNJhCC2oE","YGQTOphnGIuyiAxH","pcxllite"];
  for (var k=0;k<ids.length;k++){
    var p2=""; for(var i2=0;i2<ids[k].length;i2++) p2+=("0"+ids[k].charCodeAt(i2).toString(16)).slice(-2);
    var h2=[];
    ["r--","rw-","rwx"].forEach(function(prot){
      Process.enumerateRanges(prot).forEach(function(r){
        if (r.base.compare(main.base) >= 0 && r.base.add(r.size).compare(main.base.add(main.size)) <= 0) {
          try{ Memory.scan(r.base,r.size,p2,{onMatch:function(a){h2.push(r.protection+"@"+a.toString());},onComplete:function(){}});}catch(e){}
        }
      });
    });
    send("[*] "+ids[k]+" hits: "+JSON.stringify(h2));
  }
  send("[*] done");
})();
