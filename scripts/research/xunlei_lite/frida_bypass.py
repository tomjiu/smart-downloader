// frida_bypass.js: let xllite run past its cwd-rename self-test by faking MoveFileExW success,
// so we reach the code paths that call platformdetect.GetClientSecret / GetClientID.
// Then we (a) hook any function reading the known client_id string pool, and
// (b) capture HTTP request headers (WinHttp not imported, but Go uses its own net/http;
//     instead we hook the Go runtime's net stack is hard). We settle for:
//  - dumping the relocated string pool (recover sibling secrets physically near known ids),
//  - intercepting calls to the function that LEA's the client_id constant.
(function () {
  const KNOWN = [
    "X9ibISwpIp8jQ4Ya","XW-G4v1H72tgfJym","XVJVzaJv8vKHzVCk","XW5SkOhLDjnOZP7J",
    "YGQTOphnGIuyiAxH","XoL5lqbDWNW0e7QA","Xp6vsxz_7IYVw2BB","Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP","Yd0zTVGrNJhCC2oL","Yqp0kJBXWhwaTpB6","Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH","Yd0y91GrNJhCC2oJ","Yd00e1GrNJhCC2oR"
  ];
  function hex(s){let o="";for(let i=0;i<s.length;i++)o+=("0"+s.charCodeAt(i).toString(16)).slice(-2);return o;}
  function hexAscii(buf){const u8=new Uint8Array(buf);let lines=[];for(let off=0;off<u8.length;off+=32){let h="",a="";for(let i=0;i<32&&off+i<u8.length;i++){const b=u8[off+i];h+=("0"+b.toString(16)).slice(-2)+" ";a+=(b>=32&&b<127)?String.fromCharCode(b):".";}lines.push(("00000000"+off.toString(16)).slice(-8)+"  "+h+" "+a);}return lines.join("\n");}

  // 1) bypass rename self-test
  const moveEx = Module.getExportByName("kernel32.dll", "MoveFileExW");
  const replEx = Module.getExportByName("kernel32.dll", "ReplaceFileW");
  const moveExA = Module.getExportByName("kernel32.dll", "MoveFileExA");
  if (moveEx) Interceptor.attach(moveEx, {onEnter(){this.tag="MoveFileExW";}, onLeave(ret){ ret.replace(1); }});
  if (moveExA) Interceptor.attach(moveExA, {onEnter(){}, onLeave(ret){ ret.replace(1); }});
  if (replEx) Interceptor.attach(replEx, {onEnter(){}, onLeave(ret){ ret.replace(0); }});
  send("[*] rename bypass installed (MoveFileEx* -> success)");

  const main = Process.enumerateModules()[0];
  send("[*] module " + main.name + " base=" + main.base + " size=" + main.size);

  // 2) locate known strings, dump a window around the pcxllite client_id and around YGQTOphnGIuyiAxH
  setTimeout(function(){
    let results = {};
    for (const s of KNOWN) {
      let addrs = [];
      try {
        Memory.scan(main.base, main.size, hex(s), {
          onMatch(a,sz){ addrs.push(a.toString()); },
          onComplete(){}
        });
      } catch(e){}
      results[s] = addrs;
    }
    send("[*] LIVE string addresses:\n" + JSON.stringify(results, null, 1));
    // dump windows
    for (const s of ["XW-G4v1H72tgfJym","YGQTOphnGIuyiAxH"]) {
      let arr = results[s] || [];
      if (arr.length) {
        let a = ptr(arr[0]);
        try { send("[*] window(-512..+512) for "+s+" @ "+a+"\n"+hexAscii(a.sub(512).readByteArray(1024))); }
        catch(e){ send("[err] win "+s+" "+e); }
      }
    }
    send("[*] pool scan done");
  }, 2500);
})();
