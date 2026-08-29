// frida_errscan_all.js - enumerate EVERY module range regardless of protection and scan for the
// GetClientSecret error string; also for the client_id literal (sanity). Report which range holds it.
(function () {
  const base = ptr(0x620000);
  const size = 0x3259000;
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  var all = [];
  try { Process.enumerateRanges("-").forEach(function(r){
    if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(size))<=0) all.push(r);
  }); } catch(e){ send("[enum - err] "+e); }
  // fallback: try each prot flag
  ["r--","rw-","r-x","x--","rwx","-w-","--x"].forEach(function(p){ try { Process.enumerateRanges(p).forEach(function(r){
    if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(size))<=0) {
      var dup = all.some(function(x){return x.base.compare(r.base)===0;});
      if(!dup) all.push(r);
    }
  }); } catch(e){} });

  send("[*] total module ranges enumerated: " + all.length);
  var protos = {};
  all.forEach(function(r){ protos[r.protection]=(protos[r.protection]||0)+1; });
  send("[*] protections: " + JSON.stringify(protos));

  var targets = {
    "errstr": "PlatformConfig GetClientSecret empty. name:%v",
    "clientid": "XW-G4v1H72tgfJym"
  };
  for (var t in targets) {
    var ph = hex(targets[t]);
    var hits = [];
    all.forEach(function(r){
      try { Memory.scan(r.base, r.size, ph, { onMatch:function(a){ hits.push(r.protection+"@"+a.toString()); }, onComplete:function(){} }); } catch(e){}
    });
    send("[*] "+t+" hits: " + JSON.stringify(hits));
  }
  send("[*] done");
})();
