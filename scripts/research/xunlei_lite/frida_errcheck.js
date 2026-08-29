// frida_errcheck.js - verify the GetClientSecret error string is mapped & find its runtime addr.
(function () {
  const base = ptr(0x620000);
  const size = 0x3259000;
  const addr = ptr(0x1dc57a9);
  try { send("[read 0x1dc57a9] " + addr.readUtf8String(60)); }
  catch (e) { send("[err read] " + e); }

  var all = [];
  ["r--","rw-","x--","r-x","rwx"].forEach(function(p){
    try { Process.enumerateRanges(p).forEach(function(r){
      if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(size))<=0) all.push(r.protection+"@"+r.base+"+"+r.size);
    }); } catch(e){}
  });
  send("[ranges count] " + all.length);

  var ph = "";
  var s = "PlatformConfig GetClientSecret empty. name:%v";
  for (var i=0;i<s.length;i++) ph += ("0"+s.charCodeAt(i).toString(16)).slice(-2);
  var found = [];
  ["r--","rw-","r-x","x--"].forEach(function(p){
    try { Process.enumerateRanges(p).forEach(function(r){
      if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(size))<=0){
        try { Memory.scan(r.base, r.size, ph, { onMatch:function(a){ found.push(a.toString()); }, onComplete:function(){} }); } catch(e){}
      }
    }); } catch(e){}
  });
  send("[errstr scan hits] " + JSON.stringify(found));
})();
