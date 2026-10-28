import http from 'node:http';

/** R6: 本地 RSS 2.0 feed 服务——按 rss crate 真实解析契约构造：
 *  条目 URL 三级兜底：link 文本 → link href → http 形态 guid（v1 不读 enclosure，
 *  对齐 torrent feed 惯例：link 即下载目标）。 */
const PORT = parseInt(process.argv[2] || '9920', 10);
const FPORT = parseInt(process.argv[3] || '9903', 10);

const XML = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
<channel>
  <title>distro-announce</title>
  <link>http://127.0.0.1:${PORT}/</link>
  <description>测试用发行版公告 feed（torrent-feed 惯例：link=下载目标）</description>
  <item>
    <title>debian-13.7.0 netinst released</title>
    <link>http://127.0.0.1:${FPORT}/small.bin</link>
    <guid isPermaLink="false">debian-13.7.0-netinst</guid>
    <pubDate>Sun, 26 Oct 2026 00:00:00 GMT</pubDate>
  </item>
  <item>
    <title>ubuntu mirror sync completed</title>
    <link>http://127.0.0.1:${FPORT}/pause.bin</link>
    <guid isPermaLink="false">ubuntu-mirror-sync</guid>
    <pubDate>Sun, 26 Oct 2026 01:00:00 GMT</pubDate>
  </item>
  <item>
    <title>debian-beta snapshot build</title>
    <guid isPermaLink="false">http://127.0.0.1:${FPORT}/delete.bin</guid>
    <pubDate>Sun, 26 Oct 2026 02:00:00 GMT</pubDate>
  </item>
</channel>
</rss>`;

http.createServer((req, res) => {
  if (req.url === '/feed.xml' || req.url === '/') {
    res.writeHead(200, { 'content-type': 'application/rss+xml; charset=utf-8' });
    res.end(XML);
  } else {
    res.writeHead(404); res.end('not found');
  }
}).listen(PORT, '127.0.0.1', () => console.log(`rss server on :${PORT}`));
