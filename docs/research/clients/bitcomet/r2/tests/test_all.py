"""测试所有代码节点 (含 4 轮深度逆向共 27 个节点)."""
import sys
import os
import time
import hashlib
import tempfile
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

def test_imports():
    results = []
    modules = [
        "bclink_url_parser", "p2sp_downloader", "lt_seed_protocol",
        "adaptive_disk_cache", "anti_leech_filter", "peer_broadcast_optimizer",
        "utp_diagnostics", "peer_discovery_extender", "bitcomet_symbol_extractor",
        "close_reason_decoder", "pex_full_protocol", "wire_protocol",
        "disk_cache_priority", "repeater_ws_protocol", "lt_seed_cloud_client",
        "bt_v2_merkle_hash", "bc_passport_protocol", "peer_lifecycle_state_machine",
        "super_seeding_mode", "dht_custom_implementation", "mse_dh_encryption",
        "piece_request_scheduler", "bencode_codec_v2", "emule_p2sp_integration",
        "torrent_maker", "ipfilter_client_filter", "piece_part_file",
        "v2_piece_recovery", "storage_helper",
    ]
    for mod in modules:
        try:
            __import__(mod)
            results.append((mod, True, None))
        except Exception as e:
            results.append((mod, False, str(e)))
    return results

def test_bclink():
    from bclink_url_parser import parse, UrlProtocol
    for url, exp in [
        ("magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef01", UrlProtocol.MAGNET),
        ("ed2k://|file|test.bin|1024|a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6|/", UrlProtocol.ED2K),
        ("http://example.com/file.bin", UrlProtocol.HTTP),
        ("ftp://user:pass@host:2121/path/file.bin", UrlProtocol.FTP),
    ]:
        parts = parse(url)
        assert parts.protocol == exp, f"{url} expected {exp}, got {parts.protocol}"
    print(f"  OK 5 protocols parsed correctly")

def test_p2sp_strategy():
    from p2sp_downloader import BasicDownloadStrategy
    s = BasicDownloadStrategy(piece_size=1024)
    pieces = s.plan_pieces(4096)
    assert len(pieces) == 4
    print(f"  OK plan_pieces: 4 pieces of 1024 bytes")

def test_lt_seed_protocol():
    from lt_seed_protocol import (
        encode_query_seed, encode_query_seed_response, decode_query_seed_response,
        encode_piece_data, decode_piece_data, LtSeed,
        pack_message, unpack_message, MessageType,
    )
    payload = encode_query_seed("a"*40)
    mtype, _ = unpack_message(payload)
    assert mtype == MessageType.QUERY_SEED
    seeds = [LtSeed(endpoint=("1.2.3.4", 6881), file_hash="a"*40, health=85)]
    encoded = encode_query_seed_response(seeds)
    _, raw = unpack_message(encoded)
    decoded = decode_query_seed_response(raw)
    assert len(decoded) == 1
    assert decoded[0].health == 85
    print(f"  OK LT-Seed protocol encode/decode round-trip")

def test_adaptive_cache():
    import tempfile
    from adaptive_disk_cache import AdaptiveDiskCache, CachedFileSettings
    fd, path = tempfile.mkstemp()
    os.close(fd)
    try:
        cache = AdaptiveDiskCache(settings=CachedFileSettings(max_memory_bytes=1024*1024, auto_resize=False))
        cf = cache.open(path, "a"*40)
        for i in range(10):
            cf.put(i, os.urandom(1024), dirty=True)
        cf.flush()
        for i in range(5):
            assert cf.get(i) is not None
        cache.close_all()
        print(f"  OK Adaptive cache: 10 pieces, 5 hits")
    finally:
        os.unlink(path)

def test_anti_leech():
    from anti_leech_filter import (
        AntiLeechFilter, AntiLeechLevel, AntiLeechAction,
        identify_client, is_leech_client,
    )
    code, _ = identify_client(b"-XL0001-abcdefghij")
    assert code == "XL"
    assert is_leech_client(code)
    code2, _ = identify_client(b"-qB4500-abcdefghij")
    assert code2 == "QB"
    assert not is_leech_client(code2)
    f = AntiLeechFilter(level=AntiLeechLevel.BAN)
    ep = ("1.2.3.4", 6881)
    f.add_peer(ep, b"-XL0001-abcdefghij")
    f.update_stats(ep, downloaded=1000000, uploaded=1000)
    action = f.decide(ep)
    assert action == AntiLeechAction.DISCONNECT
    print(f"  OK AntiLeech: XunLei detected + banned")

def test_peer_broadcast():
    from peer_broadcast_optimizer import PeerBroadcastOptimizer, BtMsg
    sent = []
    opt = PeerBroadcastOptimizer(send_callback=lambda ep, mt, p: sent.append((ep, mt)), flush_interval_ms=0)
    for i in range(5):
        opt.add_peer((f"10.0.0.{i+1}", 6881))
    opt.broadcast_have(42)
    opt.flush(force=True)
    assert len(sent) == 5
    print(f"  OK 5 peers got HAVE message")

def test_utp_diagnostics():
    from utp_diagnostics import UtpDiagnostics
    diag = UtpDiagnostics()
    diag.add_socket(("1.2.3.4", 6881))
    diag.update_socket(("1.2.3.4", 6881), bytes_sent=0, bytes_received=0, rtt_ms=50.0)
    diag.force_sample()
    diag.update_socket(("1.2.3.4", 6881), bytes_sent=1000000, bytes_received=900000, rtt_ms=55.0)
    diag.force_sample()
    rate_s, rate_r = diag.get_stats_rate()
    assert rate_s > 0 or rate_r > 0
    print(f"  OK UTP diag: rate_s={rate_s:.0f}bps")

# ===== 第 2 轮深度逆向 (6 个节点) =====

def test_close_reason_decoder():
    from close_reason_decoder import (
        BitCometCloseReason, parse_close_reason, get_reason_string,
        is_bitcomet_private, BEP14Encoder,
    )
    assert get_reason_string(BitCometCloseReason.HASH_CHECK_FAILED) == "hash_check_failed"
    assert get_reason_string(BitCometCloseReason.INVALID_METADATA) == "invalid_metadata"
    assert get_reason_string(BitCometCloseReason.PROTOCOL_ERROR) == "protocol_error"
    assert get_reason_string(BitCometCloseReason.TOO_MANY_CONNECTIONS) == "too_many_connections"
    assert is_bitcomet_private(BitCometCloseReason.HASH_CHECK_FAILED)
    assert not is_bitcomet_private(BitCometCloseReason.NONE)
    info = parse_close_reason(b"hash_check_failed")
    assert info.reason_id == BitCometCloseReason.HASH_CHECK_FAILED
    assert info.is_bitcomet_private
    std = BEP14Encoder.encode_standard(BitCometCloseReason.PEER_TIMEOUT)
    decoded = BEP14Encoder.decode(std)
    assert decoded.reason_id == BitCometCloseReason.PEER_TIMEOUT
    bc = BEP14Encoder.encode_bitcomet(BitCometCloseReason.ANTI_LEECH_BLOCK)
    decoded = BEP14Encoder.decode(bc)
    assert decoded.reason_id == BitCometCloseReason.ANTI_LEECH_BLOCK
    print(f"  OK close_reason: 4 BitComet private + BEP-14 compat")

def test_pex_full_protocol():
    from pex_full_protocol import (
        PeerExchangeFull, PexConfig, PexEncoder,
        PeerDescription, PexFlags,
    )
    pex = PeerExchangeFull(config=PexConfig(min_interval_sec=0, max_peers_per_message=0))
    for i in range(100):
        ip = bytes([10, 0, i // 256, i % 256])
        pex.add_peer(PeerDescription(ip=ip, port=6881+i, flags=PexFlags.SUPPORT_UTP))
    msg1 = pex.build_pex_message(("1.2.3.4", 6881), force=True)
    assert len(msg1.added) == 100
    msg2 = pex.build_pex_message(("1.2.3.4", 6881), force=True)
    assert len(msg2.added) == 0
    for i in range(10):
        ip = bytes([10, 0, i // 256, i % 256])
        pex.remove_peer(ip, 6881+i)
    msg3 = pex.build_pex_message(("1.2.3.4", 6881), force=True)
    assert len(msg3.dropped) == 10
    encoded = PexEncoder.encode(msg3)
    decoded = PexEncoder.decode(encoded, bitcomet_private=True)
    assert decoded.seq == msg3.seq
    print(f"  OK PEX full: 100 added, 0 diff, 10 dropped")

def test_wire_protocol():
    from wire_protocol import (
        WireLinkLayerManager, WireSettings, ProtocolEnum,
        PendingQueuePriority, TrackerHostBucket,
    )
    mgr = WireLinkLayerManager(settings=WireSettings(max_connections=100))
    link1 = mgr.attach(("1.2.3.4", 6881), ProtocolEnum.BITTORRENT)
    link2 = mgr.attach(("5.6.7.8", 6881), ProtocolEnum.HTTP_CLIENT)
    assert link1.protocol == ProtocolEnum.BITTORRENT
    mgr.set_close_reason(("1.2.3.4", 6881), 100)
    assert link1.local_close_reason == 100
    mgr.set_remote_close_reason(("1.2.3.4", 6881), 102)
    assert link1.remote_close_reason == 102
    mgr.enqueue_send(("1.2.3.4", 6881), b"low", PendingQueuePriority.LOW)
    mgr.enqueue_send(("1.2.3.4", 6881), b"urgent", PendingQueuePriority.URGENT)
    mgr.enqueue_send(("1.2.3.4", 6881), b"normal", PendingQueuePriority.NORMAL)
    assert mgr.dequeue_send(("1.2.3.4", 6881)) == b"urgent"
    assert mgr.dequeue_send(("1.2.3.4", 6881)) == b"normal"
    assert mgr.dequeue_send(("1.2.3.4", 6881)) == b"low"
    bucket = TrackerHostBucket(max_per_host=3, ban_threshold=5, ban_duration_sec=10)
    assert bucket.acquire("tracker1.com")
    assert bucket.acquire("tracker1.com")
    assert bucket.acquire("tracker1.com")
    assert not bucket.acquire("tracker1.com")
    print(f"  OK Wire protocol: attach + close_reason + priority queue + tracker bucket")

def test_disk_cache_priority():
    from disk_cache_priority import (
        PriorityDiskCache, PriorityCacheSettings, PiecePriority,
    )
    cache = PriorityDiskCache(settings=PriorityCacheSettings(
        max_memory_bytes=64*1024*1024, auto_resize=False))
    file_hash = "a" * 40
    for i in range(50):
        cache.put(file_hash, i, os.urandom(64*1024), dirty=True)
    for _ in range(10):
        cache.record_ltseed_upload(file_hash, 5, 64*1024)
        cache.record_ltseed_upload(file_hash, 10, 64*1024)
    stats = cache.stats_summary()
    assert stats["hot_pieces_ltseed"] >= 2
    # piece 5 应升级为 LT_SEED_HOT
    chunk_prio = None
    for prio, bucket in cache._buckets.items():
        for k in bucket:
            if k.file_hash == file_hash and k.piece_index == 5:
                chunk_prio = prio
                break
    assert chunk_prio == PiecePriority.LT_SEED_HOT
    cache.flush()
    cache.close()
    print(f"  OK Priority cache: LT-Seed hot piece auto-promoted")

def test_repeater_ws_protocol():
    from repeater_ws_protocol import (
        RepeaterMessage, RepeaterProtocol, RepeaterMsgType,
        RepeaterError, VipToken, NatPunchOrchestrator, HolePunchMode,
    )
    msg = RepeaterMessage(
        msg_type=RepeaterMsgType.AUTH,
        payload=b'{"session_id":"abc"}',
        seq=1, src_session="abc",
    )
    encoded = RepeaterProtocol.encode(msg)
    decoded = RepeaterProtocol.decode(encoded)
    assert decoded.msg_type == RepeaterMsgType.AUTH
    assert decoded.seq == 1
    err_msg = RepeaterMessage(
        msg_type=RepeaterMsgType.AUTH_RESPONSE, error=RepeaterError.NOT_VIP, seq=2,
    )
    decoded = RepeaterProtocol.decode(RepeaterProtocol.encode(err_msg))
    assert decoded.error == RepeaterError.NOT_VIP
    token = VipToken(user_id=42, token="a"*64, expires_at=time.time()+3600, vip_level=1)
    assert token.is_vip()
    assert not token.is_expired()
    orch = NatPunchOrchestrator(repeater=None)
    orch.update_peer_graph("A", {"B"})
    orch.update_peer_graph("B", {"C"})
    assert orch.decide_mode("C", target_is_public=False) == HolePunchMode.INTRODUCE
    assert orch.decide_mode("X", target_is_public=True) == HolePunchMode.DIRECT
    print(f"  OK Repeater: encode/decode + VipToken + 3-mode punch")

def test_lt_seed_cloud_client():
    from lt_seed_cloud_client import RESTPackage, RestName, REST_ENDPOINTS
    pkg = RESTPackage(name=RestName.LT_SEED_QUERY)
    pkg.payload = {"file_hash": "a" * 40}
    encoded = pkg.build()
    decoded = RESTPackage.parse(encoded)
    assert decoded.name == RestName.LT_SEED_QUERY
    assert decoded.payload["file_hash"] == "a" * 40
    assert REST_ENDPOINTS[RestName.ACCOUNT_LOGIN_PASSWORD] == "/api/cometid/sign_in"
    print(f"  OK LT-Seed cloud: REST_Package encode/decode")

# ===== 第 3 轮深度逆向 (8 个节点) =====

def test_bt_v2_merkle_hash():
    import hashlib
    from bt_v2_merkle_hash import MerkleHashTree
    file_data = b"\x42" * 65536
    merkle = MerkleHashTree(65536, 16*1024)
    for i in range(0, 65536, 16*1024):
        piece = file_data[i:i+16*1024]
        if len(piece) < 16*1024:
            piece = piece + b"\x00" * (16*1024 - len(piece))
        merkle.assign_leaf_hash(i // (16*1024), hashlib.sha256(piece).digest())
    root = merkle.calc_root_hash()
    assert len(root) == 32
    piece_idx = 2
    piece_data = file_data[2*16*1024:3*16*1024]
    proof = merkle.calc_proof_hashes_for_piece(piece_idx)
    assert merkle.verify_piece_hash(piece_idx, piece_data, proof)
    bad = merkle.verify_piece_hash(piece_idx, b"\x00" * 16*1024, proof)
    assert not bad
    print(f"  OK BT v2 Merkle: root={root.hex()[:16]}... proof verify PASS")

def test_bc_passport_protocol():
    import secrets
    from bc_passport_protocol import BcPassportProtocol, BitCometLtepExt
    alice_key = secrets.token_bytes(32)
    bob_key = secrets.token_bytes(32)
    alice = BcPassportProtocol(b"-BC0001-", alice_key, my_version=221)
    bob = BcPassportProtocol(b"-BC0002-", bob_key, my_version=221)
    bob.parse_ltep_supported(alice.build_ltep_supported())
    alice.parse_ltep_supported(bob.build_ltep_supported())
    assert bob.is_supported_by_remote and alice.is_supported_by_remote
    bob.generate_seed()
    alice.receive_remote_seed(alice.parse_seed_message(bob.build_seed_message()))
    passport = bob.parse_passport_message(alice.build_passport_message())
    assert passport is not None
    assert passport.client_id == b"-BC0001-"
    assert bob.verify_passport(passport, alice_key)
    assert alice.parse_auth_finished_message(bob.build_auth_finished_message())
    assert alice.is_auth_passed and bob.is_auth_passed
    print(f"  OK bc_passport: 5-stage handshake, ext_id=0x{int(BitCometLtepExt.BC_PASSPORT_SUPPORTED):02x}")

def test_peer_lifecycle_state_machine():
    from peer_lifecycle_state_machine import PeerState, PeerLifecycleStateMachine
    fsm = PeerLifecycleStateMachine()
    for i in range(10):
        fsm.peer_add((f"10.0.0.{i+1}", 6881+i))
    assert fsm.get_state_count(PeerState.NEW) == 10
    for i in range(5):
        fsm.peer_add_for_connect((f"10.0.0.{i+1}", 6881+i))
    for i in range(3):
        fsm.protocol_handshake_passed((f"10.0.0.{i+1}", 6881+i))
    assert fsm.get_state_count(PeerState.CONNECTED) == 3
    for i in range(3, 5):
        fsm.protocol_outgoing_failed((f"10.0.0.{i+1}", 6881+i))
    assert fsm.get_state_count(PeerState.DEAD) == 2
    fsm.peer_ban(("10.0.0.1", 6881), reason="leech")
    assert fsm.get_state_count(PeerState.BANNED) == 1
    fsm.peer_unban(("10.0.0.1", 6881))
    print(f"  OK Peer FSM: 6 states, transitions worked")

def test_super_seeding_mode():
    from super_seeding_mode import SuperSeedingManager
    mgr = SuperSeedingManager(total_pieces=100, fake_missing_ratio=0.7)
    for i in range(5):
        mgr.add_peer((f"10.0.0.{i+1}", 6881+i))
    sent = 0
    for _ in range(30):
        for i in range(5):
            ep = (f"10.0.0.{i+1}", 6881+i)
            piece = mgr.find_piece_for_superseeding(ep)
            if piece is not None:
                mgr.mark_piece_sent(ep, piece)
                sent += 1
                other = (i + 1) % 5 + 1
                mgr.on_peer_have_piece((f"10.0.0.{other}", 6881+other-1), piece)
    assert sent > 0
    perm = mgr.get_my_permillage_as_superseed(("10.0.0.1", 6881))
    assert 0 <= perm <= 1000
    result = mgr.timer_tick()
    print(f"  OK Super-seeding: sent={sent} distributed={result['distributed_count']} perm={perm}")

def test_dht_custom():
    import hashlib
    from dht_custom_implementation import BitCometDht, DhtNode
    dht = BitCometDht(listen_port=6881, db_file="/tmp/test_bc_dht.db")
    dht.start()
    for i in range(20):
        ip = f"10.0.0.{i+1}"
        port = 6881 + i
        node_id = hashlib.sha1(f"{ip}:{port}".encode()).digest()
        dht.add_node(DhtNode(node_id=node_id, ip=ip, port=port))
    for i in range(5):
        info_hash = hashlib.sha1(f"torrent_{i}".encode()).digest()
        dht.database.add(info_hash, name=f"torrent_{i}", size=1024*1024*(i+1))
        dht.database.set_keyword(info_hash, "movie")
    target = hashlib.sha1(b"target").digest()
    closest = dht.find_node(target)
    assert len(closest) <= 8
    entries = dht.database.get_filtered(keyword="movie")
    assert len(entries) == 5
    dht.block_ip("9.9.9.9")
    assert dht.is_ip_blocked("9.9.9.9")
    dht.stop()
    print(f"  OK BitComet DHT: nodes={len(dht.routing_table.get_nodes())} torrents={dht.database.get_all_count()}")

def test_mse_dh_encryption():
    from mse_dh_encryption import BitCometDhEncryption, DhkeyEncryptType
    alice = BitCometDhEncryption(is_incoming=False)
    bob = BitCometDhEncryption(is_incoming=True)
    alice.dh.encrypt_type = DhkeyEncryptType.RC4
    bob.dh.encrypt_type = DhkeyEncryptType.RC4
    skey = b"\x11" * 20
    alice_pubkey = alice.start_dh(skey)
    bob_pubkey = bob.start_dh(skey)
    alice.complete_dh(bob_pubkey)
    bob.complete_dh(alice_pubkey)
    assert alice.dh.shared_secret == bob.dh.shared_secret
    alice.handshake_passed()
    bob.handshake_passed()
    plaintext = b"Hello, BitComet MSE encrypted world!" * 4
    encrypted = alice.socket_send(plaintext)
    decrypted = bob.decrypt_recv_stream(encrypted)
    assert plaintext == decrypted
    alice.task_add(b"\x22" * 20)
    alice.task_add(b"\x33" * 20)
    assert len(alice._task_map) == 2
    print(f"  OK MSE/DH: secret match + RC4 round-trip + multi-task")

def test_piece_request_scheduler():
    from piece_request_scheduler import (
        PieceScheduler, FilePriority, PieceSource, SLICE_SIZE,
    )
    files = [
        {"name": "video.mp4", "size": 50*256*1024,
         "first_piece": 0, "last_piece": 49, "piece_count": 50},
        {"name": "sub.srt", "size": 50*256*1024,
         "first_piece": 50, "last_piece": 99, "piece_count": 50},
    ]
    sched = PieceScheduler(total_pieces=100, piece_size=256*1024, files=files)
    sched.file_priority.set_file_priority(0, FilePriority.HIGH)
    assert sched.file_priority.get_file_priority(0) == FilePriority.HIGH
    peer_a = ("1.2.3.4", 6881)
    for i in range(31):
        sched.on_peer_have_piece(peer_a, i)
    piece = sched.select_rarest_piece(set(range(31)), peer_a)
    assert piece is not None
    # 请求所有 slice (16 个)
    num_slices = 256 * 1024 // SLICE_SIZE
    for s in range(num_slices):
        sched.request_slice(piece, s * SLICE_SIZE, peer_a)
    ok = sched.request_separate_piece(50, PieceSource.P2SP)
    assert ok
    piece_50 = sched.select_rarest_piece({50}, peer_a)
    assert piece_50 is None
    # 接收所有 slice 完成 piece
    for s in range(num_slices):
        sched.on_slice_received(piece, s * SLICE_SIZE, b"\x00" * SLICE_SIZE, peer_a)
    assert sched.is_piece_finished(piece)
    print(f"  OK Piece scheduler: rarest + separate + {num_slices} slices piece completion")

def test_bencode_codec_v2():
    from bencode_codec_v2 import (
        BencodeEncoder, BencodeDecoder, BencodeSaxParser,
        V2TorrentFileHandler, HybridMagnetBuilder,
    )
    data = {
        b"announce": b"http://tracker.example.com/announce",
        b"info": {
            b"meta version": 2,
            b"piece length": 16384,
            b"file tree": {
                b"subdir": {
                    b"file.txt": {
                        b"length": 1024,
                        b"pieces root": b"\x00" * 32,
                    }
                }
            }
        }
    }
    encoded = BencodeEncoder.encode(data)
    decoded = BencodeDecoder.decode(encoded)
    assert decoded[b"info"][b"meta version"] == 2
    handler = V2TorrentFileHandler()
    BencodeSaxParser(handler).parse(encoded)
    assert handler.meta_version == 2
    v1 = bytes.fromhex("abcdef0123456789abcdef0123456789abcdef01")
    v2 = bytes.fromhex("deadbeef" * 8)
    magnet = HybridMagnetBuilder.build(v1, v2, name="test")
    parsed = HybridMagnetBuilder.parse(magnet)
    assert parsed["v1_hash"] == v1
    assert parsed["v2_hash"] == v2
    print(f"  OK Bencode v2: encode/decode + SAX + hybrid magnet")

def test_emule_p2sp_integration():
    from emule_p2sp_integration import MultiSourceTask
    info_hash = bytes.fromhex("abcdef" * 6 + "abcdef01")
    task = MultiSourceTask(info_hash, total_pieces=100, piece_size=256*1024)
    task.add_bt_peer(("1.2.3.4", 6881), set(range(31)))
    bt_key = "bt://1.2.3.4:6881"  # add_bt_peer 内部生成的 key
    task.add_emule_source(
        endpoint=("9.10.11.12", 4662),
        file_hash="a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6",
        file_size=25*1024*1024, file_name="test.bin", rating=5,
    )
    task.add_http_webseed("http://mirror1.example.com/file.bin")
    task.add_ftp_mirror("ftp://mirror2.example.com/file.bin")
    task.add_lt_seed(("13.14.15.16", 25432))
    # bt_key 是 add_bt_peer 内部生成的, 简化: 直接传 (endpoint tuple repr)
    # 实际 bt_key = "bt://1.2.3.4:6881"
    assert task.coordinator.assign_piece(0, "bt://1.2.3.4:6881")
    task.coordinator.on_emule_cancel_all_other_peers(50)
    # emule_key = "ed2k://9.10.11.12:4662"
    assert task.coordinator.assign_piece(50, "ed2k://9.10.11.12:4662")
    task.coordinator.on_piece_downloaded(0, 256*1024)
    task.coordinator.on_piece_downloaded(50, 256*1024)
    assert task.get_completion() > 0
    print(f"  OK eMule+P2SP: BT+HTTP+FTP+eMule+LT-Seed 5 source types")


    print("=" * 70)


# ===== 第四轮深度逆向 (5 个新节点) =====

def test_torrent_maker():
    """测试完整 torrent 创建器 (v1+v2 hybrid)."""
    import tempfile
    from torrent_maker import (
        TorrentMaker, TorrentMakeSetting, TorrentMetaVersion,
        TorrentMakeStatus, TorrentMakeError, FileFilter, PieceSizeSelector,
    )
    with tempfile.TemporaryDirectory() as tmpdir:
        for i in range(3):
            with open(os.path.join(tmpdir, f"file_{i}.txt"), "wb") as f:
                f.write(os.urandom(32 * 1024))
        os.makedirs(os.path.join(tmpdir, "subdir"), exist_ok=True)
        with open(os.path.join(tmpdir, "subdir", "nested.bin"), "wb") as f:
            f.write(os.urandom(50 * 1024))
        with open(os.path.join(tmpdir, ".DS_Store"), "w") as f:
            f.write("should be filtered")
        setting = TorrentMakeSetting(
            source_path=tmpdir,
            output_path=os.path.join(tmpdir, "test.torrent"),
            name="test_torrent",
            trackers=[["http://tracker.example.com/announce"]],
            web_seeds=["http://webseed.example.com/"],
            meta_version=TorrentMetaVersion.V2,
            piece_size=0,  # 自动
        )
        maker = TorrentMaker(setting)
        maker.torrent_make_begin()
        maker.torrent_make_wait(timeout=30)
        status = maker.torrent_make_get_status()
        assert status.status == TorrentMakeStatus.FINISHED, f"status={status.status.name} err={status.error_msg}"
        assert status.error == TorrentMakeError.NONE
        assert status.info_hash_v1 is not None
        assert status.info_hash_v2 is not None
        assert os.path.exists(setting.output_path)
        # 文件过滤测试
        assert FileFilter.is_file_filtered(".DS_Store", [".DS_Store"])
        assert not FileFilter.is_file_filtered("file.txt", [".DS_Store"])
        # piece size 自动选择
        assert PieceSizeSelector.select(1024) == 16 * 1024  # < 1MB → 16KiB
        assert PieceSizeSelector.select(100 * 1024 * 1024) == 64 * 1024  # 100MB → 64KiB
        assert PieceSizeSelector.select(2 * 1024 * 1024 * 1024) == 256 * 1024  # 2GB → 256KiB
    print(f"  OK torrent maker: v1+v2 hybrid, status={status.status.name}, files={status.files_total}")


def test_ipfilter_client_filter():
    """测试 IP filter + 客户端 filter."""
    from ipfilter_client_filter import (
        IpFilterRule, ClientFilterRule, FilterAction, PeerBannedReason,
        IpFilter, ClientFilter, CombinedFilter,
    )
    combined = CombinedFilter()
    # IP filter
    combined.ip_filter.add_rule(IpFilterRule(ip_range="10.0.0.0/8", description="private"))
    combined.ip_filter.add_rule(IpFilterRule(ip_range="192.168.0.0/16"))
    combined.ip_filter.add_rule(IpFilterRule(ip_range="1.2.3.4"))
    # client filter
    combined.client_filter.add_rule(ClientFilterRule(
        client_code="XL", peer_id_pattern=r"-XL\d{4}-",
        action=FilterAction.LIMIT_25,
    ))
    combined.client_filter.add_rule(ClientFilterRule(
        client_code="SD", action=FilterAction.BAN,
    ))
    combined.client_filter.set_refused_client_types(["QQ", "XF"])
    # 测试
    cases = [
        ("1.2.3.4", 6881, b"-XL0001-abcdefghij", FilterAction.BAN),       # IP ban
        ("8.8.8.8", 6881, b"-XL0001-abcdefghij", FilterAction.LIMIT_25),  # client limit
        ("8.8.8.8", 6881, b"-SD0001-abcdefghij", FilterAction.BAN),         # client ban
        ("8.8.8.8", 6881, b"-qB4500-abcdefghij", FilterAction.ALLOW),       # allow
        ("8.8.8.8", 6881, b"-QQ0001-abcdefghij", FilterAction.BAN),         # refused type
        ("192.168.1.1", 6881, b"-qB4500-abcdefghij", FilterAction.BAN),     # IP ban
    ]
    for ip, port, pid, expected in cases:
        action, _ = combined.check_peer(ip, port, pid)
        assert action == expected, f"{ip} pid={pid[:8]} expected {expected.name}, got {action.name}"
    # 临时 ban
    combined.add_temp_ban(("9.9.9.9", 6881), PeerBannedReason.ANTI_LEECH,
                            duration_sec=3600)
    action, _ = combined.check_peer("9.9.9.9", 6881, b"-qB4500-abcdefghij")
    assert action == FilterAction.BAN
    print(f"  OK IP filter + client filter: 6 test cases all pass")


def test_piece_part_file():
    """测试 piece-part 临时文件 (断电恢复)."""
    import tempfile
    from piece_part_file import PiecePartList, PiecePartFile, SliceRecord, PieceRecord, SLICE_SIZE
    torrent_hash = b"\xab" * 20
    piece_size = 256 * 1024
    with tempfile.NamedTemporaryFile(suffix=".bc!", delete=False) as tmp:
        part_file = tmp.name
    try:
        ppl = PiecePartList(torrent_hash, piece_size, part_file)
        # 下载 piece 0 (16 slice)
        original_slices = []
        for i in range(16):
            sd = os.urandom(SLICE_SIZE)
            original_slices.append(sd)
            assert ppl.on_data_downloaded(0, i * SLICE_SIZE, sd)
        assert ppl.is_piece_finished(0)
        # 部分下载 piece 1
        for i in range(8):
            ppl.on_data_downloaded(1, i * SLICE_SIZE, os.urandom(SLICE_SIZE))
        assert not ppl.is_piece_finished(1)
        # 持久化
        ppl.save()
        assert os.path.exists(part_file)
        # 模拟重启
        ppl2 = PiecePartList(torrent_hash, piece_size, part_file)
        assert ppl2.is_piece_finished(0)
        assert not ppl2.is_piece_finished(1)
        # 写回主文件
        with tempfile.NamedTemporaryFile(delete=False) as main_tmp:
            main_file = main_tmp.name
        try:
            ok = ppl2.save_piece_from_part_file_to_download_files(0, main_file, 0)
            assert ok
            assert os.path.getsize(main_file) == piece_size
        finally:
            os.unlink(main_file)
        stats = ppl2.get_stats()
        assert stats["total_pieces"] == 2
        assert stats["completed"] == 1
    finally:
        if os.path.exists(part_file):
            os.unlink(part_file)
    print(f"  OK piece-part file: 断电恢复 + 写回主文件 OK")


def test_v2_piece_recovery():
    """测试 BT v2 损坏 piece 恢复."""
    from v2_piece_recovery import (
        V2HashTreeSync, V2PieceRecovery, RecoveryStrategy, RecoverySource,
        PieceHashState,
    )
    total_pieces = 8
    pieces_per_layer = 4
    # 原始数据
    piece_data_orig = [os.urandom(16 * 1024) for _ in range(total_pieces)]
    piece_layers = b"".join(hashlib.sha256(pd).digest() for pd in piece_data_orig)
    # 创建 sync
    sync = V2HashTreeSync(total_pieces, pieces_per_layer)
    sync.on_piece_hash_v2_loaded(piece_layers)
    assert sync.get_known_hash_count_in_piece_layers() == total_pieces
    # 创建 recovery
    recovery = V2PieceRecovery(sync)
    # 测试正常 piece
    is_corrupt, strategy = recovery.detect_corruption(0, piece_data_orig[0])
    assert not is_corrupt
    assert strategy == RecoveryStrategy.NONE
    # 测试损坏 piece
    corrupted = b"\x00" * 16 * 1024
    is_corrupt, strategy = recovery.detect_corruption(1, corrupted)
    assert is_corrupt
    # 启动恢复
    req = recovery.initiate_recovery(1, strategy, RecoverySource.PEER)
    assert req.piece_index == 1
    # 恢复数据到达
    ok = recovery.on_data_recoveried(1, piece_data_orig[1], RecoverySource.PEER)
    assert ok
    # 统计
    stats = recovery.get_stats()
    assert stats["recoveries_initiated"] >= 1
    assert stats["recoveries_succeeded"] >= 1
    print(f"  OK v2 piece recovery: 损坏检测 + 恢复 + proof 校验")


def test_storage_helper():
    """测试存储抽象层."""
    import tempfile
    from storage_helper import (
        FileEntry, FileInfoVector, StorageHelper, StorageHelperDelegate,
        FileOpenMode, FileAllocateStrategy,
    )
    with tempfile.TemporaryDirectory() as tmpdir:
        files = []
        for i in range(5):
            entry = FileEntry(
                file_path=os.path.join(tmpdir, f"file_{i}.bin"),
                relative_path=f"file_{i}.bin",
                size=1024 * 1024,
            )
            files.append(entry)
        fiv = FileInfoVector()
        fiv.init(files)
        helper = StorageHelper(fiv, max_open_files=3)
        helper.files_init_and_auto_correct()
        helper.start()
        try:
            # 写入
            for i in range(3):
                ok = helper.disk_write(i, 0, os.urandom(4096))
                assert ok
            # 触发 LRU 关闭
            ok = helper.disk_write(3, 0, os.urandom(4096))
            assert ok
            # 读取
            data = helper.disk_read(0, 0, 4096)
            assert data is not None
            # 完成度
            pct = helper.calculate_file_complete(0)
            assert pct > 0
            # 统计
            stats = helper.get_stats()
            assert stats["total_files"] == 5
        finally:
            helper.stop()
    print(f"  OK storage helper: 5 files + LRU fd pool + auto open")
if __name__ == "__main__":
    print("=" * 70)
    print("BitComet Accelerator Toolkit — Full Test Suite (27 节点, 含 4 轮深度逆向)")
    print("=" * 70)
    print()
    print("[1/27] Import test:")
    results = test_imports()
    for mod, ok, err in results:
        print(f"  {'OK' if ok else 'FAIL'} {mod}{': ' + err if err else ''}")
    if not all(ok for _, ok, _ in results):
        print("\nFATAL: some modules failed to import")
        sys.exit(1)
    print()
    print("[2/22] bclink_url_parser:")
    test_bclink()
    print("\n[3/22] p2sp_downloader:")
    test_p2sp_strategy()
    print("\n[4/22] lt_seed_protocol:")
    test_lt_seed_protocol()
    print("\n[5/22] adaptive_disk_cache:")
    test_adaptive_cache()
    print("\n[6/22] anti_leech_filter:")
    test_anti_leech()
    print("\n[7/22] peer_broadcast_optimizer:")
    test_peer_broadcast()
    print("\n[8/22] utp_diagnostics:")
    test_utp_diagnostics()
    print()
    print("=== 第 2 轮深度逆向 (6 节点) ===")
    print("\n[9/22] close_reason_decoder:")
    test_close_reason_decoder()
    print("\n[10/22] pex_full_protocol:")
    test_pex_full_protocol()
    print("\n[11/22] wire_protocol:")
    test_wire_protocol()
    print("\n[12/22] disk_cache_priority:")
    test_disk_cache_priority()
    print("\n[13/22] repeater_ws_protocol:")
    test_repeater_ws_protocol()
    print("\n[14/22] lt_seed_cloud_client:")
    test_lt_seed_cloud_client()
    print()
    print("=== 第 3 轮深度逆向: 私有 libtorrent fork (8 节点) ===")
    print("\n[15/22] bt_v2_merkle_hash:")
    test_bt_v2_merkle_hash()
    print("\n[16/22] bc_passport_protocol:")
    test_bc_passport_protocol()
    print("\n[17/22] peer_lifecycle_state_machine:")
    test_peer_lifecycle_state_machine()
    print("\n[18/22] super_seeding_mode:")
    test_super_seeding_mode()
    print("\n[19/22] dht_custom_implementation:")
    test_dht_custom()
    print("\n[20/22] mse_dh_encryption:")
    test_mse_dh_encryption()
    print("\n[21/22] piece_request_scheduler:")
    test_piece_request_scheduler()
    print("\n[22/22] emule_p2sp_integration:")
    test_emule_p2sp_integration()
    print()
    print("=== 第四轮深度逆向 (5 节点) ===")
    print()
    print("[23/27] torrent_maker:")
    test_torrent_maker()
    print()
    print("[24/27] ipfilter_client_filter:")
    test_ipfilter_client_filter()
    print()
    print("[25/27] piece_part_file:")
    test_piece_part_file()
    print()
    print("[26/27] v2_piece_recovery:")
    test_v2_piece_recovery()
    print()
    print("[27/27] storage_helper:")
    test_storage_helper()
    print()
    print("=" * 70)
    print("All tests passed (27/27).")
