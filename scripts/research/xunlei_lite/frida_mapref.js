// frida_mapref.js - find all memory locations that STORE a pointer to the pcxllite client_id
// string (0x1c7a282). In a Go map/struct the id is stored as (ptr,len); the adjacent entry is the
// secret. Dump 64 bytes around each reference to recover the secret pointer+value.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;
  const idPtr = 0x1c7a282; // runtime address of "XW-G4v1H72tgfJym"
  function hex(v){ return ("00000000"+v.toString(16)).slice(-8); }

  var all = [];
  ["r--","rw-"].forEach(function(p){ try{ Process.enumerateRanges(p).forEach(function(r){
    if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) all.push(r);
  }); }catch(e){} });

  // search for the 4-byte little-endian pointer value 0x1c7a282 in data ranges
  var needle = ""; for (var i=0;i<4;i++) needle += ("0"+( (idPtr>>(8*i))&0xff ).toString(16)).slice(-2);
  var hits = [];
  all.forEach(function(r){
    try {
      Memory.scan(r.base, r.size, needle, {
        onMatch: function(a){ hits.push(a); },
        onComplete: function(){}
      });
    } catch(e){}
  });
  send("[*] pointer-to-client_id (0x"+hex(idPtr)+") refs: "+hits.length);
  // also search by value scan using Memory.scanSync alternative: manual not needed.

  function goStrAt(a){ try { var len=a.add(4).readU32(); if(len<=0||len>4096) return null; return a.readUtf8String(len); } catch(e){ return null; } }
  function rh(a){ try { return a.readPointer().toString(); } catch(e){ return "?"; } }

  hits.forEach(function(h){
    // dump 32 bytes before and 64 after at each hit (the (ptr,len) pair + sibling)
    var start = h.sub(32);
    var u = new Uint8Array(start.readByteArray(128));
    var L=[];
    for (var off=0; off<u.length; off+=4){
      var v = u[off] | (u[off+1]<<8) | (u[off+2]<<16) | (u[off+3]<<24);
      var tag = (start.add(off).compare(h)===0) ? " <==IDPTR" : "";
      L.push(hex(start.add(off))+": "+hex(v)+tag);
    }
    send("[*] ref@"+h+"\n  "+L.join("\n  "));
    // interpret as (ptr,len): the id ptr then its len(=16), then next (ptr,len) = secret?
    try {
      var p1 = h.readPointer(); var l1 = h.add(4).readU32();
      var p2 = h.add(8).readPointer(); var l2 = h.add(12).readU32();
      send("    id=(ptr="+p1+",len="+l1+") next=(ptr="+p2+",len="+l2+") nextVal="+JSON.stringify(goStrAt(p2)));
    } catch(e){ send("    err "+e); }
  });
  send("[*] done");
})();
