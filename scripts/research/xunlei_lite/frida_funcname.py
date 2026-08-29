// frida_funcname.py - check whether the Go funcnametab strings are mapped at runtime, and if so,
// locate "(*PlatformConfig).GetClientSecret" and anchor the function via its LEA, then hook it.
(function () {
  const base = ptr(0x620000);
  const size = 0x3259000;
  function hex(s){var o="";for(var i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}

  var ranges = [];
  ["r--","rw-","r-x","x--","rwx"].forEach(function(p){try{Process.enumerateRanges(p).forEach(function(r){if(r.base.compare(base)>=0&&r.base.add(r.size).compare(base.add(size))<=0)ranges.push(r);});}catch(e){}});

  var targets = {
    "funcnameGS": "(*PlatformConfig).GetClientSecret",
    "funcnameGI": "(*PlatformConfig).GetClientID",
    "funcnameGR": "(*PlatformConfig).GetRawConfig"
  };
  for (var t in targets) {
    var ph = hex(targets[t]);
    var hits = [];
    ranges.forEach(function(r){ try{Memory.scan(r.base,r.size,ph,{onMatch:function(a){hits.push(a.toString());},onComplete:function(){}});}catch(e){} });
    send("[*] "+t+" hits: "+JSON.stringify(hits));
  }
  send("[*] done");
})();
