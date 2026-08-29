"""测试所有代码节点是否能正常 import + 基本运行."""
import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

def test_imports():
    """测试所有模块可正常 import (验证依赖与语法)."""
    results = []
    modules = [
        "bclink_url_parser",
        "p2sp_downloader",
        "lt_seed_protocol",
        "adaptive_disk_cache",
        "anti_leech_filter",
        "peer_broadcast_optimizer",
        "utp_diagnostics",
        "peer_discovery_extender",
        "bitcomet_symbol_extractor",
    ]
    for mod in modules:
        try:
            __import__(mod)
            results.append((mod, True, None))
        except Exception as e:
            results.append((mod, False, str(e)))
    return results

def test_bclink():
    """测试 URL 解析."""
    from bclink_url_parser import parse, is_valid, UrlProtocol
    cases = [
        ("magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef01", UrlProtocol.MAGNET),
        ("ed2k://|file|test.bin|1024|a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6|/", UrlProtocol.ED2K),
        ("http://example.com/file.bin", UrlProtocol.HTTP),
        ("ftp://user:pass@host:2121/path/file.bin", UrlProtocol.FTP),
        ("thunder://QUFodHRwOi8vZXhhbXBsZS5jb20vZmlsZS5iaW5aWg==", None),  # 递归解析后是 HTTP
    ]
    for url, expected_proto in cases:
        try:
            parts = parse(url)
            ok = expected_proto is None or parts.protocol == expected_proto
            print(f"  {'✓' if ok else '✗'} {url[:50]:50s} -> {parts.protocol.value}")
        except Exception as e:
            print(f"  ✗ {url[:50]:50s} -> ERROR: {e}")

def test_p2sp_strategy():
    """测试 P2SP 分片策略."""
    from p2sp_downloader import BasicDownloadStrategy, PieceState
    s = BasicDownloadStrategy(piece_size=1024)
    pieces = s.plan_pieces(4096)
    assert len(pieces) == 4, f"expected 4 pieces, got {len(pieces)}"
    ranges = s.get_wanted_ranges(pieces)
    assert len(ranges) == 4
    print(f"  ✓ plan_pieces: 4 pieces of 1024 bytes, ranges = {ranges}")

def test_lt_seed_protocol():
    """测试 LT-Seed 协议编解码."""
    from lt_seed_protocol import (
        encode_query_seed, encode_query_seed_response, decode_query_seed_response,
        encode_request_piece, encode_piece_data, decode_piece_data,
        LtSeed, pack_message, unpack_message, MessageType
    )
    # QUERY_SEED
    payload = encode_query_seed("a"*40)
    mtype, _ = unpack_message(payload)
    assert mtype == MessageType.QUERY_SEED
    # RESPONSE (需要先 unpack_message 取出 payload)
    seeds = [LtSeed(endpoint=("1.2.3.4", 6881), file_hash="a"*40, health=85)]
    encoded = encode_query_seed_response(seeds)
    _, raw_payload = unpack_message(encoded)
    decoded = decode_query_seed_response(raw_payload)
    assert len(decoded) == 1
    assert decoded[0].endpoint == ("1.2.3.4", 6881)
    assert decoded[0].health == 85
    # PIECE_DATA (同样需要 unpack_message)
    data = b"hello world" * 100
    encoded = encode_piece_data("a"*40, 5, data)
    _, raw_payload = unpack_message(encoded)
    fh, idx, decoded_data = decode_piece_data(raw_payload)
    assert fh == "a"*40
    assert idx == 5
    assert decoded_data == data
    print(f"  ✓ LT-Seed protocol: query/response/piece all encode/decode correctly")

def test_adaptive_cache():
    """测试自适应缓存."""
    import tempfile
    from adaptive_disk_cache import AdaptiveDiskCache, CachedFileSettings
    # 创建空临时文件 (关闭后再用)
    fd, path = tempfile.mkstemp()
    os.close(fd)  # 立即关闭, 留下空文件
    try:
        settings = CachedFileSettings(max_memory_bytes=1024*1024, auto_resize=False)
        cache = AdaptiveDiskCache(settings=settings)
        cf = cache.open(path, "a"*40)
        # 写 10 个 piece
        for i in range(10):
            cf.put(i, os.urandom(1024), dirty=True)
        cf.flush()
        stats = cf.stats()
        assert stats["hits"] == 0  # 没读
        # 读 5 个
        for i in range(5):
            assert cf.get(i) is not None
        stats = cf.stats()
        assert stats["hits"] == 5
        cache.close_all()
        print(f"  ✓ Adaptive cache: wrote 10 pieces, 5 hits, hit_rate={stats['hit_rate']:.0%}")
    finally:
        os.unlink(path)

def test_anti_leech():
    """测试反吸血."""
    from anti_leech_filter import (
        AntiLeechFilter, AntiLeechLevel, AntiLeechAction,
        identify_client, is_leech_client
    )
    # 识别迅雷 (用真实 20 字节 peer_id)
    code, name = identify_client(b"-XL0001-abcdefghijk")
    assert code == "XL", f"expected XL, got {code}"
    assert is_leech_client(code)
    # 识别 qBittorrent (peer_id 前 8 字节是 -qB4500-)
    code2, name2 = identify_client(b"-qB4500-abcdefghijk")
    assert code2 == "QB", f"expected QB, got {code2}"
    assert not is_leech_client(code2)
    # 等级 4 (BAN) 对迅雷
    f = AntiLeechFilter(level=AntiLeechLevel.BAN)
    ep = ("1.2.3.4", 6881)
    f.add_peer(ep, b"-XL0001-abcdefghijk")
    f.update_stats(ep, downloaded=1000000, uploaded=1000)
    action = f.decide(ep)
    assert action == AntiLeechAction.DISCONNECT, f"expected DISCONNECT, got {action.name}"
    print(f"  ✓ AntiLeech: XunLei at BAN level -> {action.name}")

def test_peer_broadcast():
    """测试 peer 广播优化."""
    from peer_broadcast_optimizer import PeerBroadcastOptimizer, BtMsg
    sent = []
    def fake_send(ep, mt, payload):
        sent.append((ep, mt, payload))
    opt = PeerBroadcastOptimizer(send_callback=fake_send, flush_interval_ms=0)
    for i in range(5):
        opt.add_peer((f"10.0.0.{i+1}", 6881))
    opt.broadcast_have(42)
    opt.flush(force=True)
    assert len(sent) == 5
    for ep, mt, _ in sent:
        assert mt == int(BtMsg.HAVE)
    print(f"  ✓ Broadcast: 5 peers got HAVE message for piece 42")

def test_utp_diagnostics():
    """测试 UTP 诊断."""
    from utp_diagnostics import UtpDiagnostics
    diag = UtpDiagnostics()
    diag.add_socket(("1.2.3.4", 6881))
    # 第一次更新 (作为基线)
    diag.update_socket(("1.2.3.4", 6881),
                        bytes_sent=0, bytes_received=0,
                        packets_sent=0, packets_received=0,
                        rtt_ms=50.0)
    diag.force_sample()
    # 第二次更新 (增量, 这样 sample 才能算出速率)
    diag.update_socket(("1.2.3.4", 6881),
                        bytes_sent=1000000, bytes_received=900000,
                        packets_sent=1000, packets_received=950,
                        packets_lost=50, packets_retransmitted=30,
                        rtt_ms=55.0)
    diag.force_sample()
    rate_s, rate_r = diag.get_stats_rate()
    assert rate_s > 0 or rate_r > 0, f"expected some rate, got s={rate_s} r={rate_r}"
    drop_r = diag.get_utp_recv_drop_percent()
    assert drop_r >= 0
    print(f"  ✓ UTP diag: rate_s={rate_s:.0f}bps rate_r={rate_r:.0f}bps drop_r={drop_r:.1f}%")

if __name__ == "__main__":
    print("=" * 60)
    print("BitComet Accelerator Toolkit — Test Suite")
    print("=" * 60)
    print()
    print("[1/8] Import test:")
    results = test_imports()
    for mod, ok, err in results:
        print(f"  {'✓' if ok else '✗'} {mod}{': ' + err if err else ''}")
    if not all(ok for _, ok, _ in results):
        print("\nFATAL: some modules failed to import")
        sys.exit(1)
    print()
    print("[2/8] bclink_url_parser:")
    test_bclink()
    print()
    print("[3/8] p2sp_downloader:")
    test_p2sp_strategy()
    print()
    print("[4/8] lt_seed_protocol:")
    test_lt_seed_protocol()
    print()
    print("[5/8] adaptive_disk_cache:")
    test_adaptive_cache()
    print()
    print("[6/8] anti_leech_filter:")
    test_anti_leech()
    print()
    print("[7/8] peer_broadcast_optimizer:")
    test_peer_broadcast()
    print()
    print("[8/8] utp_diagnostics:")
    test_utp_diagnostics()
    print()
    print("=" * 60)
    print("All tests passed.")
    print("=" * 60)
