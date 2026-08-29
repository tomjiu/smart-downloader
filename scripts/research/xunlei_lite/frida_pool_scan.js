// frida_pool_scan.js - now that the string pool is readable, find the client_secret by:
// (a) dumping a window around each known client_id to reveal physically-adjacent secret, and
// (b) scanning the readable pool for 16-24 char alphanumeric tokens that are NOT known client_ids.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;

  const KNOWN = [
    "X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Xqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ"
  ];
  var set = {}; KNOWN.forEach(function(k){set[k]=1;});

  function ph(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  // collect readable (non-exec) module ranges only (strings live in .rdata/.data)
  var all = [];
  ["r--","rw-"].forEach(function(prot){
    try { Process.enumerateRanges(prot).forEach(function(r){
      if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) all.push(r);
    }); } catch(e){}
  });
  send("[*] scanning "+all.length+" non-exec ranges");

  // 1) find the pcxllite client_id address, dump +/- 256 bytes
  var idAddr = null;
  all.forEach(function(r){
    try { Memory.scan(r.base, r.size, ph("XW-G4v1H72tgfJym"), { onMatch:function(a){ if(!idAddr) idAddr=a; }, onComplete:function(){} }); } catch(e){}
  });
  if (idAddr) {
    try {
      var buf = idAddr.sub(256).readByteArray(512);
      var u = new Uint8Array(buf); var lines=[];
      for (var off=0; off<u.length; off+=32){ var h="",a2=""; for(var i=0;i<32&&off+i<u.length;i++){var b=u[off+i];h+=("0"+b.toString(16)).slice(-2)+" ";a2+=(b>=32&&b<127)?String.fromCharCode(b):".";} lines.push(("00000000"+off.toString(16)).slice(-8)+"  "+h+" "+a2); }
      send("[*] pool window around pcxllite id @ "+idAddr+"\n"+lines.join("\n"));
    } catch(e){ send("[err window] "+e); }
  }

  // 2) scan whole pool for 16-24 char mixed tokens not in KNOWN (deferred)
  setTimeout(function () {
    var cand = {};
    all.forEach(function(r){
      try {
        var CHUNK = 0x100000;
        for (var cstart = 0; cstart < r.size; cstart += CHUNK) {
          var len = Math.min(CHUNK, r.size - cstart);
          var bytes;
          try { bytes = new Uint8Array(r.base.add(cstart).readByteArray(len)); } catch(e){ break; }
          var i = 0;
          while (i < bytes.length) {
            if (bytes[i] >= 0x30 && bytes[i] <= 0x7a) {
              var j = i; var tok = "";
              while (j < bytes.length && ((bytes[j]>=0x30&&bytes[j]<=0x39)||(bytes[j]>=0x41&&bytes[j]<=0x5a)||(bytes[j]>=0x61&&bytes[j]<=0x7a)||bytes[j]===0x5f||bytes[j]===0x2d)) { tok += String.fromCharCode(bytes[j]); j++; }
              if (tok.length >= 16 && tok.length <= 26 && !set[tok]) {
                var up=0,lo=0,dg=0; for(var k=0;k<tok.length;k++){var c=tok[k]; if(c>='A'&&c<='Z')up++; else if(c>='a'&&c<='z')lo++; else if(c>='0'&&c<='9')dg++;}
                if (up&&lo&&dg) cand[tok]=(cand[tok]||0)+1;
              }
              i = j;
            } else i++;
          }
        }
      } catch(e){}
    });
    var out = Object.keys(cand).sort(function(a,b){return cand[b]-cand[a];});
    send("[*] candidate non-id tokens in pool ("+out.length+"):");
    out.slice(0,120).forEach(function(t){ send("   "+t+" x"+cand[t]); });
    send("[*] done");
  }, 200);
})();
