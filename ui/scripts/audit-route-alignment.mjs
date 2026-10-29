#!/usr/bin/env node
/**
 * 前后端契约对齐审计（体检 A 层工具入库版）。
 *
 * 背景：#182 首检 / #202 复检的 A 层审计工具此前只活在会话沙箱里，
 * 复检 #202 时暴露其通道盲区——exportTorrent 走裸 fetch() 二进制通道
 * （blob 下载无法复用 req<T> JSON 助手），旧工具只认 req()/EventSource
 * 而漏报为「export 无 UI 入口」（已勘误 + e2e 护栏于 #191/#203）。
 * 本脚本是修复后版本入库，供后续体检/开发随时重跑：
 *
 *   npm run audit:routes
 *
 * 做三件事：
 *   1. daemon 路由面：正则解析 crates/daemon/src/http.rs 的 .route()
 *      声明（含链式多方法 get(x).post(y)；Rust 侧声明规整，正则可靠）
 *   2. UI 调用面：TypeScript AST 解析 ui/src/lib/daemon/store.ts 的
 *      req() / fetch(url(...)) / new EventSource(...) 三类调用首参
 *      （模板串 ${} → :x；query 剥离；method 取第二参缺省 GET。
 *      fetch 仅认 url() 包裹的字面路径——req 助手自身的 fetch(url(path))
 *      因 path 是标识符被 literalPath 判 null，天然去重不重复计数）
 *   3. 双向核验：
 *      - 破窗（UI 调用无对应路由）→ 任何一条即 exit 1（契约断裂，硬失败）
 *      - 盲区（路由无 UI 消费者）→ 对照 by-design 登记簿分类；未登记者
 *        列出待人工判定（可能是新缺口，也可能需要登记）→ exit 0（软提示，
 *        盲区本质是产品决策，不做硬门）
 *
 * by-design 登记簿（daemon-only / 运维端点，#190 已文档声明）：
 *   config/health/metrics/version/ws（基础设施）/ nas×5（NAS 托管运维）/
 *   xunlei-import（CLI 通道）/ DELETE security/bans（旧解禁通道，UI 已迁
 *   POST unban，DELETE 保留兼容既有调用方）。增删须同步 #190 的 README 声明。
 *
 * 维护提醒：
 *   - 新增 wire 端点走裸 fetch() 时无需改本脚本（通道已覆盖）；走其他
 *     通道（如 WebSocket 客户端）时需扩 visit() 的通道枚举
 *   - 误报排查：literalPath 对复杂表达式 ${}（条件/调用）保守判「路径
 *     完结」，若路由恰好带 query 变量可能漏配——用简单标识符 ${} 最稳
 */
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const REPO = path.resolve(__dirname, '..', '..');
const nodeRequire = createRequire(__dirname);
const ts = nodeRequire(
  nodeRequire.resolve('typescript', { paths: [path.join(REPO, 'ui')] }),
);

const httpSrc = fs.readFileSync(
  path.join(REPO, 'crates/daemon/src/http.rs'),
  'utf8',
);
const storeSrc = fs.readFileSync(
  path.join(REPO, 'ui/src/lib/daemon/store.ts'),
  'utf8',
);

/// by-design 盲区登记簿：daemon-only / 运维端点（无 UI 消费者是刻意的）。
const BY_DESIGN = new Set([
  // #213-4 插件页起 UI 消费 /config（启停态/档位快照）——从 by-design 移出，
  // 转为对齐通道（store.configSnapshot）。
  'GET /health',
  'GET /metrics',
  'GET /version',
  'GET /ws',
  'GET /nas/status',
  'POST /nas/install',
  'POST /nas/start',
  'POST /nas/stop',
  'POST /nas/token',
  'POST /tasks/xunlei-import',
  // 旧解禁通道（P2-1 审计修复）：DELETE 带 body 违反 HTTP 语义，UI 已迁
  // POST /security/bans/unban（#187 规范通道）；DELETE 路由保留兼容既有
  // CLI/脚本调用方（同 handler，见 http.rs 路由注释）——daemon-only 兼容面。
  'DELETE /security/bans',
  // #212 桌面壳身份验证探针：消费者是 Tauri 壳的 wait_daemon（Rust 裸
  // TCP，非前端 fetch）——按 UI 源码统计消费者恒为零，by-design。
  'GET /instance-token/:x',
]);

// ---- 1. daemon 路由（.route( 切段：段首路径 + 段身 handler 链）----
const routes = new Set();
const parts = httpSrc.split('.route(');
for (let i = 1; i < parts.length; i++) {
  const seg = parts[i];
  const pm = seg.match(/^\s*"([^"]+)"\s*,/);
  if (!pm) continue;
  const p = pm[1].replace(/:[a-zA-Z_]+/g, ':x');
  const body = i === parts.length - 1 ? seg.split(';')[0] : seg;
  for (const mm of body.matchAll(/\b(get|post|put|delete|patch)\s*\(/g)) {
    routes.add(`${mm[1].toUpperCase()} ${p}`);
  }
}
console.log(`[daemon] 路由数: ${routes.size}`);

// ---- 2. UI 调用点（AST；req / fetch(url(...)) / EventSource 三通道）----
const sf = ts.createSourceFile('store.ts', storeSrc, ts.ScriptTarget.ES2022, true);
const calls = new Set();

/** 模板字面量/字符串 → 规范化路径（${} 标识符段 → :x；query 剥离）。 */
function literalPath(node) {
  if (ts.isNoSubstitutionTemplateLiteral(node) || ts.isStringLiteral(node)) {
    return node.text.split('?')[0] || null;
  }
  if (ts.isTemplateExpression(node)) {
    const head = node.head.text;
    let p = head.split('?')[0];
    if (head.includes('?')) return p || null;
    for (const span of node.templateSpans) {
      const sub = span.expression;
      const isSegment = ts.isIdentifier(sub) || ts.isPropertyAccessExpression(sub);
      const mid = span.literal.text;
      if (!isSegment) break;
      p += ':x';
      const qIdx = mid.indexOf('?');
      if (qIdx >= 0) {
        p += mid.slice(0, qIdx);
        break;
      }
      p += mid;
    }
    return p || null;
  }
  return null;
}

function visit(node) {
  if (ts.isCallExpression(node) || ts.isNewExpression(node)) {
    const callee = node.expression.getText();
    const isReq = callee === 'req' || callee.endsWith('.req');
    const isFetch = callee === 'fetch' || callee.endsWith('.fetch');
    const isEventSource = callee.includes('EventSource');
    if ((isReq || isFetch) && node.arguments.length >= 1) {
      let pathArg = node.arguments[0];
      if (isFetch && ts.isCallExpression(pathArg)
          && (pathArg.expression.getText() === 'url'
              || pathArg.expression.getText().endsWith('.url'))
          && pathArg.arguments.length >= 1) {
        pathArg = pathArg.arguments[0];
      } else if (isFetch) {
        pathArg = null;
      }
      const p = pathArg ? literalPath(pathArg) : null;
      let method = 'GET';
      if (node.arguments.length >= 2 && ts.isObjectLiteralExpression(node.arguments[1])) {
        for (const prop of node.arguments[1].properties) {
          if (ts.isPropertyAssignment(prop) && prop.name.getText() === 'method'
              && ts.isStringLiteral(prop.initializer)) {
            method = prop.initializer.text.toUpperCase();
          }
        }
      }
      if (p && p.startsWith('/')) calls.add(`${method} ${p}`);
    }
    if (isEventSource && node.arguments.length >= 1) {
      const a = node.arguments[0];
      if (ts.isCallExpression(a) && a.arguments.length >= 1) {
        const p = literalPath(a.arguments[0]);
        if (p) calls.add(`GET ${p}`);
      }
    }
  }
  ts.forEachChild(node, visit);
}
visit(sf);
console.log(`[ui] 调用点数: ${calls.size}`);

// ---- 3. 双向核验 ----
const broken = [...calls].filter((c) => !routes.has(c)).sort();
const blindAll = [...routes].filter((r) => !calls.has(r)).sort();
const blindKnown = blindAll.filter((r) => BY_DESIGN.has(r));
const blindUnknown = blindAll.filter((r) => !BY_DESIGN.has(r));

console.log('\n=== 破窗（UI 调用无对应路由——契约断裂，硬失败）===');
if (broken.length === 0) {
  console.log('  0 ✅');
} else {
  for (const b of broken) console.log(`  ✗ ${b}`);
}

console.log(`\n=== 盲区：by-design 登记 ${blindKnown.length}/${BY_DESIGN.size} ===`);
const missingReg = [...BY_DESIGN].filter((r) => !blindAll.includes(r));
if (missingReg.length > 0) {
  console.log('  ⚠ 登记簿条目已获 UI 消费者（登记过期，请同步删除）：');
  for (const r of missingReg) console.log(`    - ${r}`);
}
console.log('\n=== 盲区：未登记（待人工判定：新缺口 or 需登记）===');
if (blindUnknown.length === 0) {
  console.log('  0 ✅（全部盲区已登记为 by-design）');
} else {
  for (const r of blindUnknown) console.log(`  ? ${r}`);
}

const verdict = broken.length === 0 ? 'PASS' : 'FAIL';
console.log(`\n结论：${verdict}（破窗 ${broken.length}；盲区 ${blindAll.length} = by-design ${blindKnown.length} + 未登记 ${blindUnknown.length}）`);
process.exit(broken.length === 0 ? 0 : 1);
