// frida_writefile.js - hook kernel32.WriteFile to capture outgoing buffers that contain
// credential material (x-client-secret / clientSecret / 16-char tokens / api-pan hosts).
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;
  function goStr(a){ try{var l=a.add(4).readU32(); if(l<=0||l>8192)return null; return a.readUtf8String(l);}catch(e){return null;} }

  // known ids to flag
  var KNOWN = ["XW-G4v1H72tgfJym","X9ibISwpIp8jQ4Ya","XoL5lqbDWNW0e7QA","Yd0uSVGrNJhCC2oE","YGQTOphnGIuyiAxH"];

  var wf = Module.getGlobalExportByName("WriteFile");
  if (!wf || wf.isNull()) { send("[!] WriteFile not found"); return; }
  var seen = {};
  Interceptor.attach(wf, {
    onEnter: function (args) {
      var buf = args[1]; var n = args[2].toInt32();
      if (n <= 0 || n > 200000) return;
      try {
        var u = new Uint8Array(buf.readByteArray(Math.min(n, 8000)));
        // quick ascii scan for interesting tokens
        var s = "";
        for (var i=0;i<u.length;i++){ var b=u[i]; s += (b>=32&&b<127)?String.fromCharCode(b):(b===10||b===13)?"\n":"."; }
        if (/client_secret|clientSecret|x-client-secret|x-client-id|api-pan|XL_USER|secret/i.test(s)) {
          // extract 16-char tokens not in KNOWN
          var toks = s.match(/[A-Za-z0-9_\-]{16,26}/g) || [];
          var newtok = [];
          for (var k=0;k<toks.length;k++){ if (KNOWN.indexOf(toks[k])<0 && /[A-Za-z]/.test(toks[k])) { if(!seen[toks[k]]){seen[toks[k]]=1; newtok.push(toks[k]);} } }
          if (newtok.length || /client_secret|clientSecret|x-client-secret/i.test(s)) {
            send("[WRITE] n="+n+" newTokens="+JSON.stringify(newtok)+"\n"+s.substring(0,4000));
          }
        }
      } catch(e){}
    }
  });
  send("[*] WriteFile hooked; capturing (daemon makes API calls after init)");
  setTimeout(function(){ send("[*] capture window done; total unique non-id tokens: "+Object.keys(seen).length); }, 15000);
})();
