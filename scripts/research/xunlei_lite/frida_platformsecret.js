// frida_platformsecret.js - R4 for xllite.exe
// Attach to running xllite, hook GetClientSecret/GetClientID (by platform name),
// and WinHttp* to capture api-pan request headers (G2).
// Go string = {ptr, len} struct.
function goStr(addr) {
  if (addr.isNull()) return "";
  try {
    const len = addr.add(8).readU64().toNumber();
    if (len <= 0 || len > 1 << 20) return "";
    return addr.readUtf8String(len);
  } catch (e) { return "<err:" + e + ">"; }
}

function findFunc(mod, sub) {
  // Go symbol names aren't in the PE export table; search the module's symbol
  // metadata via the module's .rdata string table is hard. Instead, pattern-match
  // the UTF-16/utf8 of known path strings is unreliable. We rely on WinHttp hooks
  // for headers, and intercept platformdetect by scanning for the function via
  // the known string "GetClientSecret empty" nearby? Too fragile.
  // Fallback: hook WinHttp only (robust) and log all api-pan headers + bodies.
  return null;
}

function pWide(p, max) {
  max = max || 8192;
  try { return p.readUtf16String(max) || ""; } catch (e) { return ""; }
}
function getExp(fn) {
  try { return Module.getGlobalExportByName(fn); } catch (e) { return null; }
}

const reqInfo = {};
const connServer = {};

function installWinHttp() {
  const connect = getExp("WinHttpConnect");
  if (connect) Interceptor.attach(connect, {
    onEnter(args) { this.server = pWide(args[1], 256); },
    onLeave(ret) { if (!ret.isNull()) connServer[ret.toString()] = this.server; }
  });
  const openReq = getExp("WinHttpOpenRequest");
  if (openReq) Interceptor.attach(openReq, {
    onEnter(args) {
      this.hConn = args[0].toString();
      this.verb = pWide(args[1], 16);
      this.path = pWide(args[2], 4096);
    },
    onLeave(ret) {
      if (!ret.isNull()) reqInfo[ret.toString()] = {
        server: connServer[this.hConn] || "?",
        verb: this.verb || "GET",
        path: this.path
      };
    }
  });
  ["WinHttpAddRequestHeadersW", "WinHttpAddRequestHeaders"].forEach(function (fn) {
    const a = getExp(fn);
    if (!a) return;
    Interceptor.attach(a, {
      onEnter(args) {
        const info = reqInfo[args[0].toString()];
        const hdrs = pWide(args[1], 12000);
        const interesting = info && ((info.server || "").indexOf("api-pan") !== -1
          || (info.server || "").indexOf("xluser") !== -1
          || (info.server || "").indexOf("xunlei") !== -1);
        const hasX = hdrs.indexOf("X-Client") !== -1 || hdrs.indexOf("x-client") !== -1
          || hdrs.indexOf("client_id") !== -1;
        if (interesting || hasX) {
          send("\n==== HDRS " + (info ? info.verb + " https://" + info.server + info.path : "(no url)"));
          send(hdrs.substring(0, 6000));
        }
      }
    });
  });
  send("[*] WinHttp hooks installed");
}

function main() {
  send("[*] frida_platformsecret attached to " + Process.enumerateModules()[0].name);
  installWinHttp();
  send("[*] NOTE: Go symbol interception for GetClientSecret skipped (no stable export); rely on WinHttp header capture for G2.");
}
setTimeout(main, 0);
