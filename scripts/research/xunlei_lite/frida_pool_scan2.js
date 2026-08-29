// frida_pool_scan2.js - deferred pool scan for secret candidates.
// Scan all readable module ranges for 16-26 char mixed-case+digit tokens that are NOT known
// client_ids. Run deferred (setTimeout) so script load() returns immediately.
(function () {
  const main = Process.enumerateModules()[0];
  const base = main.base;

  const KNOWN = [
    "X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Xqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ","Yd00e1GrNJhCC2oR"
  ];
  const knownSet = {};
  KNOWN.forEach(function(k){ knownSet[k] = 1; });

  const MAXLEN = 26, MINLEN = 16;
  const cand = {}; // token -> count

  function isMixed(t) {
    let up=0, lo=0, dg=0;
    for (let i=0;i<t.length;i++){
      const c=t.charCodeAt(i);
      if (c>=65&&c<=90) up++;
      else if (c>=97&&c<=122) lo++;
      else if (c>=48&&c<=57) dg++;
      else if (c!==95 && c!==45) return false; // allow _ and -
    }
    return up>0 && lo>0 && dg>0;
  }

  setTimeout(function () {
    let ranges = [];
    ["r--","rw-"].forEach(function(prot){
      try { Process.enumerateRanges(prot).forEach(function(r){
        if (r.base.compare(base)>=0 && r.base.add(r.size).compare(base.add(main.size))<=0) ranges.push(r);
      }); } catch(e){}
    });
    send("[*] scanning " + ranges.length + " ranges (deferred)");

    let rangesDone = 0;
    ranges.forEach(function(r){
      const CHUNK = 0x400000; // 4MB chunks
      let off = 0;
      function scanChunk(){
        if (off >= r.size) { rangesDone++; if (rangesDone % 10 === 0) send("[*] progress "+rangesDone+"/"+ranges.length); return; }
        const len = Math.min(CHUNK, r.size - off);
        let bytes;
        try { bytes = new Uint8Array(r.base.add(off).readByteArray(len)); } catch(e){ off = r.size; rangesDone++; return; }
        // tokenize ascii runs
        let i = 0;
        while (i < bytes.length) {
          const b = bytes[i];
          if ((b>=48&&b<=57)||(b>=65&&b<=90)||(b>=97&&b<=122)||b===95||b===45) {
            let j = i; let tok = "";
            while (j < bytes.length) {
              const c = bytes[j];
              if ((c>=48&&c<=57)||(c>=65&&c<=90)||(c>=97&&b<=122)||c===95||c===45) { tok += String.fromCharCode(c); j++; }
              else break;
            }
            if (tok.length>=MINLEN && tok.length<=MAXLEN && !knownSet[tok] && isMixed(tok)) {
              cand[tok] = (cand[tok]||0)+1;
            }
            i = j;
          } else i++;
        }
        off += len;
        // yield every chunk to keep the frida message pump alive
        if (off < r.size) {
          setTimeout(scanChunk, 0);
        } else {
          scanChunk();
        }
      }
      scanChunk();
    });

    // After all ranges: report. Use a final timer to ensure completion.
    const finish = setInterval(function(){
      if (rangesDone >= ranges.length) {
        clearInterval(finish);
        const out = Object.keys(cand).sort(function(a,b){return cand[b]-cand[a];});
        send("[*] candidate secret tokens (non-id, mixed): " + out.length);
        out.slice(0,120).forEach(function(t){ send("   " + t + " x" + cand[t]); });
        send("[*] DONE");
      }
    }, 500);
  }, 300);
})();
