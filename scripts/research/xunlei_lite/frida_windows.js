// frida_windows.js - just dump a +/-512 byte window around each known id's RUNTIME address
// (computed from the r-- ranges we already located). Fast, no heavy loops.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;
  function ph(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}
  var all = [];
  ["r--","rw-"].forEach(function(prot){ try { Process.enumerateRanges(prot).forEach(function(r){
    if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) all.push(r);
  }); } catch(e){} });

  const IDS = {
    "X9ibISwpIp8jQ4Ya":1,"XW-G4v1H72tgfJym":1,"XVJVzaJv8vKHzVCk":1,"XW5SkOhLDjnOZP7J":1,
    "YGQTOphnGIuyiAxH":1,"XoL5lqbDWNW0e7QA":1,"Xp6vsxz_7IYVw2BB":1,"Yd0uSVGrNJhCC2oE":1,
    "Yd00NFGrNJhCC2oP":1,"Yd0zTVGrNJhCC2oL":1,"Xqp0kJBXWhwaTpB6":1,"Yd0zylGrNJhCC2oN":1,
    "Yd0yklGrNJhCC2oH":1,"Yd0y91GrNJhCC2oJ":1
  };
  var addr = {};
  all.forEach(function(r){
    for (var name in IDS) {
      if (addr[name]) continue;
      try { Memory.scan(r.base, r.size, ph(name), { onMatch:function(a){ addr[name]=a; }, onComplete:function(){} }); } catch(e){}
    }
  });

  function hexdump(a, n) {
    var u = new Uint8Array(a.readByteArray(n)); var lines=[];
    for (var off=0; off<u.length; off+=32){ var h="",s2=""; for(var i=0;i<32&&off+i<u.length;i++){var b=u[off+i];h+=("0"+b.toString(16)).slice(-2)+" ";s2+=(b>=32&&b<127)?String.fromCharCode(b):".";} lines.push(("00000000"+off.toString(16)).slice(-8)+"  "+h+" "+s2); }
    return lines.join("\n");
  }
  for (var name in addr) {
    var a = addr[name];
    try { send("[*] WINDOW "+name+" @ "+a+"\n"+hexdump(a.sub(384), 768)); } catch(e){ send("[err] "+name+" "+e); }
  }
  send("[*] done");
})();
