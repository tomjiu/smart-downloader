// debug hook error
(function () {
  try {
    var ex = Module.getExportByName("kernel32.dll", "MoveFileExW");
    send("[*] MoveFileExW export: " + ex + " typeof=" + typeof ex);
    if (ex && !ex.isNull()) {
      Interceptor.attach(ex, {
        onEnter: function (args) {
          this.f = args[0]; this.t = args[1];
          try { send("[*] MoveFileExW enter: " + args[0] + " -> " + args[1]); } catch(e){}
        },
        onLeave: function (ret) {
          try { ret.replace(ptr("0x1")); } catch (e) { send("[err] replace: " + e); }
        }
      });
      send("[*] attached MoveFileExW OK");
    }
  } catch (e) { send("[err] outer: " + e + " | stack: " + (e && e.stack)); }
  send("[*] done");
})();
