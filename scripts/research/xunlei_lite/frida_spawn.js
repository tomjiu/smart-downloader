// frida_spawn.js - spawn xllite with rename bypass pre-installed, then once running,
// locate GetClientSecret by finding code that loads the pcxllite client_id string
// (XW-G4v1H72tgfJym). On 386 the string is referenced via LEA reg,[disp32]; target = inst_end+disp32.
// We scan .text for that LEA and instrument the enclosing function to dump any Go string it returns.
(function () {
  function bypass(name) {
    try {
      var ex = Module.getGlobalExportByName(name);
      if (ex && !ex.isNull()) {
        Interceptor.attach(ex, { onEnter: function(){}, onLeave: function(r){ try{r.replace(ptr("0x1"));}catch(e){} } });
        return true;
      }
    } catch (e) {}
    return false;
  }
  ["MoveFileExW","MoveFileExA","MoveFileW","MoveFileA","ReplaceFileW"].forEach(function(n){ if(bypass(n)) send("[*] bypass "+n); });

  const main = Process.enumerateModules()[0];
  const base = main.base;
  // pcxllite client_id runtime addr (proven: base + file_offset for this region)
  const ID_FOFF = 0x1659482;
  const idAddr = base.add(ID_FOFF);
  send("[*] pcxllite client_id expected @ " + idAddr);

  // Wait for the process to be fully up, then scan .text for LEA targeting idAddr.
  setTimeout(function () {
    var cs = null;
    try { cs = new CpuContext(); } catch(e){}
    // Use Memory.scan over .text (module base + image size minus headers)
    // We need the .text range. Enumerate ranges of the module that are executable.
    var found = [];
    Process.enumerateRanges("x--").forEach(function(r){
      if (r.base.compare(base) >= 0 && r.base.add(r.size).compare(base.add(main.size)) <= 0) {
        try {
          Memory.scan(r.base, r.size, "8d", {  // LEA opcode
            onMatch: function(a){
              // decode modrm to see if disp32 form: 8D /r with mod=00,rm=101 => disp32 (0x05,15,1d,25,2d,35,3d)
              try {
                var modrm = a.add(1).readU8();
                var rm = modrm & 7; var mod = (modrm >> 6) & 3;
                if (mod === 0 && rm === 5) {
                  var disp = a.add(2).readS32();
                  var instEnd = a.add(6);
                  var target = instEnd.add(disp);
                  if (target.compare(idAddr) === 0) {
                    found.push(a.toString());
                  }
                }
              } catch(e){}
            },
            onComplete: function(){}
          });
        } catch(e){}
      }
    });
    send("[*] LEA->client_id hits: " + found.length + " " + JSON.stringify(found));
    if (found.length) {
      var lea = ptr(found[0]);
      // instrument the function prologue: search backward for a RET or function start is hard;
      // instead, set a stalker? Simpler: instrument the function that contains this LEA by
      // placing a hook at lea (which is mid-function) - not safe. Instead use the LEA to identify
      // the function, then hook the function entry: find preceding 'PUSH EBP; MOV EBP,ESP' (55 89 E5)
      // or 'SUB ESP,imm' prologue.
      var prologue = lea;
      for (var i = 0; i < 0x2000; i += 1) {
        var p = lea.sub(i);
        try {
          var b0 = p.readU8(); var b1 = p.add(1).readU8();
          // function entry often: 55 89 E5 (push ebp; mov ebp,esp) or 83 EC xx (sub esp)
          if (b0 === 0x55 && b1 === 0x89 && p.add(2).readU8() === 0xe5) { prologue = p; break; }
        } catch(e){ break; }
      }
      send("[*] function prologue candidate @ " + prologue + " (lea=" + lea + ")");
    }
    send("[*] scan done");
  }, 6000);
})();
