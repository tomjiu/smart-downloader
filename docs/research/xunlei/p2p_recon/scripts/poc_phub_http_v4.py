"""
PoC v4（已废弃）: 基于 v2 假设的 PHub 包格式 — 前 13 字节不加密 + MD5 派生 AES key

反汇编确认:
  - 包数据 [0:4] + [5:9] → MD5 → AES-128 key
  - 前 13 字节不加密 (sub rax, 0xd)
  - 从 [13:] 开始 AES-ECB 加密
  - cmd_id 可能是 0x22 (从 DoDecode cmp 看出)
  - 另一个命令字 0x1771 (6001) 从调用 8 看出

⚠️  v2 假设已被 v3 证伪：
  - PHub HTTP 生产包不使用 MD5(seq) 派生 AES key
  - 正确模型 = RSA-1024 包装随机 16B AES key（每请求 XPF_RandomBytes）
  - 规范见 `scripts/research/cloud_delivery/v3/PHUB_PROTOCOL_SPEC_V3.md`
  - XUDT 帧仍用 MD5(8_byte_header)，见 `scripts/research/cloud_delivery/phub_line/XUDT_KEY_DERIVATION_SOLVED.md`
"""
import struct, hashlib, time, urllib.request, urllib.error, ssl, os, base64
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.backends import default_backend

PHUB_HOST = "pr-phub.sandai.net"
DCDN_HOST = "dcdnhub-xcloud.sandai.net"

def aes_ecb_encrypt(key, data):
    pad_len = 16 - (len(data) % 16)
    data = data + bytes([pad_len] * pad_len)
    cipher = Cipher(algorithms.AES(key), modes.ECB(), backend=default_backend())
    return cipher.encryptor().update(data) + cipher.encryptor().finalize()

def md5_bytes(data):
    return hashlib.md5(data).digest()


def build_phub_packet(cmd_id: int, seq: int, body: bytes) -> bytes:
    """构造完整 PHub 包
    
    格式 (反汇编推断):
      [0:4]   cmd_id (uint32 LE, 不加密)
      [4]     protocol_flag (uint8, 不加密, 0=PRF_DEFAULT)
      [5:9]   sequence (uint32 LE, 不加密)
      [9:13]  reserved/header_len (4 字节, 不加密)
      [13:]   AES-ECB 加密的 body
    
    AES key = MD5(cmd_id_bytes + sequence_bytes)  (8 字节 → 16 字节)
    """
    # 前 9 字节 (key 派生源)
    header_9 = struct.pack("<I", cmd_id)       # [0:4] cmd_id
    header_9 += struct.pack("B", 0)             # [4] protocol_flag = 0
    header_9 += struct.pack("<I", seq)          # [5:9] sequence
    
    # AES key = MD5(header[0:4] + header[5:9])
    key_input = header_9[0:4] + header_9[5:9]   # 8 字节
    aes_key = md5_bytes(key_input)                # 16 字节
    
    # 前 13 字节不加密: [0:9] + [9:13] (4 字节 reserved)
    unencrypted_header = header_9 + struct.pack("<I", 0)  # 13 字节
    
    # body 加密
    encrypted_body = aes_ecb_encrypt(aes_key, body)
    
    # 完整包 = 不加密头 + 加密 body
    packet = unencrypted_header + encrypted_body
    return packet, aes_key


def send_to_phub(body: bytes, host: str = PHUB_HOST) -> tuple:
    """发送 HTTP POST 到 PHub"""
    try:
        req = urllib.request.Request(
            f"http://{host}/",
            data=body,
            headers={
                'Host': host,
                'User-Agent': 'curl/7.64',
                'Content-Type': 'application/octet-stream',
            },
            method='POST',
        )
        r = urllib.request.urlopen(req, timeout=10)
        return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except Exception as e:
        return -1, str(e).encode()


def main():
    print("="*70)
    print("PoC v4: 正确 PHub 包格式 (前 13 字节不加密 + MD5 AES key)")
    print("="*70)
    
    test_infohash = bytes.fromhex("b773873096c5174a94cc0632d463033b4d46ae50")
    peerid = b"-XL0019-" + os.urandom(12)
    
    # 尝试各种 cmd_id
    # 从反汇编看到:
    #   - DoDecode 里 cmp [rax], 0x22 (34) → 可能是响应 cmd_id
    #   - 调用 8 里 mov edi, 0x1771 (6001) → 另一个命令字
    #   - HUB_PROTO__CMD_ID__CMD_QUERYPEERREQ → 值未知
    
    cmd_candidates = [
        (0x22, "cmp [rax], 0x22 from DoDecode"),
        (0x19, "mov edx, 0x19 from DoDecode"),
        (0x1771, "mov edi, 0x1771 (6001) from 调用8"),
        (0, "CMD_DEFAULT"),
        (1, "CMD_QUERYPEERREQ (guess 1)"),
        (2, "CMD_QUERYPEERREQ (guess 2)"),
        (3, "CMD_QUERYPEERREQ (guess 3)"),
        (0x10, "CMD_QUERYPEERREQ (guess 0x10)"),
        (0x100, "CMD_QUERYPEERREQ (guess 0x100)"),
    ]
    
    # body: 构造一个简单的 QueryPeer 请求
    # 字段 (从 HUB_PROTO 推断):
    #   task_scene (4B) = TSC_DL = 5 (guess)
    #   task_mode (4B) = TMD_UNSPECIFIED = 0
    #   hub_type (4B) = HT_PHUB = 1 (guess)
    #   peer_flag (4B) = PEF_DEFAULT = 0
    #   info_hash (20B)
    #   peerid (20B)
    body = struct.pack("<I", 5)       # task_scene = TSC_DL
    body += struct.pack("<I", 0)      # task_mode = TMD_UNSPECIFIED
    body += struct.pack("<I", 1)      # hub_type = HT_PHUB
    body += struct.pack("<I", 0)      # peer_flag = PEF_DEFAULT
    body += test_infohash              # 20 bytes info_hash
    body += peerid[:20]                # 20 bytes peerid
    
    print(f"\nbody ({len(body)} bytes): {body[:32].hex()}...")
    
    for cmd_id, desc in cmd_candidates:
        packet, aes_key = build_phub_packet(cmd_id, seq=0, body=body)
        
        print(f"\n--- cmd_id=0x{cmd_id:x} ({desc}) ---")
        print(f"  AES key: {aes_key.hex()}")
        print(f"  packet head (13B unencrypted): {packet[:13].hex()}")
        print(f"  packet encrypted body ({len(packet)-13}B): {packet[13:29].hex()}...")
        
        status, resp = send_to_phub(packet)
        resp_str = resp.decode('utf-8', errors='ignore')[:200]
        print(f"  HTTP {status}: {resp_str}")
        
        # 检查是否还是 "decrypt request failed"
        if b'decrypt' not in resp and b'Decode' not in resp:
            print(f"  ★★★ 服务器响应变化! 可能成功! ★★★")
    
    # 也试 DCDN (带 base64 包装)
    print(f"\n{'='*60}")
    print("DCDN 测试 (base64 包装)")
    print(f"{'='*60}")
    
    for cmd_id, desc in cmd_candidates[:3]:
        packet, aes_key = build_phub_packet(cmd_id, seq=0, body=body)
        encoded = base64.b64encode(packet)
        
        print(f"\n--- cmd_id=0x{cmd_id:x} ({desc}) + base64 ---")
        status, resp = send_to_phub(encoded, host=DCDN_HOST)
        resp_str = resp.decode('utf-8', errors='ignore')[:200]
        print(f"  HTTP {status}: {resp_str}")
        
        if b'decrypt' not in resp and b'Decode' not in resp:
            print(f"  ★★★ 服务器响应变化! 可能成功! ★★★")
    
    # 也试只发前 13 字节 (不加密 body)
    print(f"\n{'='*60}")
    print("只发前 13 字节不加密头 (无 body)")
    print(f"{'='*60}")
    for cmd_id in [0x22, 0x1771, 0]:
        header_13 = struct.pack("<I", cmd_id) + b'\x00' + struct.pack("<I", 0) + struct.pack("<I", 0)
        status, resp = send_to_phub(header_13)
        print(f"  cmd_id=0x{cmd_id:x}: HTTP {status}: {resp.decode('utf-8', errors='ignore')[:100]}")


if __name__ == "__main__":
    main()
