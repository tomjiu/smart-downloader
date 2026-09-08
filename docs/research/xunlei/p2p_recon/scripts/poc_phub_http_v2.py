"""
PoC v2: 完整 PHub/DCDN HTTP 客户端
基于新发现: body 是 Base64(AES-ECB(原始 protobuf 数据))
"""
import struct
import hashlib
import socket
import ssl
import time
import urllib.request, urllib.error
import os
import base64
from pathlib import Path

# 沿用 v1 的常量
CLIENT_ID = "Xp6vsxz_7IYVw2BB"
CLIENT_SECRET = "<redacted>"
CLIENT_VERSION = "8.31.0.9726"
PACKAGE_NAME = "com.xunlei.downloadprovider"
APPID = "40"
APPKEY = "34a062aaa22f906fca4fefe9fb3a3021"
USER_AGENT = ("ANDROID-com.xunlei.downloadprovider/8.31.0.9726 netWorkType/5G "
              "appid/40 deviceName/Xiaomi_M2004j7ac deviceModel/M2004J7AC "
              "OSVersion/12 protocolVersion/301 platformVersion/10 sdkVersion/512000 "
              "Oauth2Client/0.9 (Linux 4_14_186-perf-gddfs8vbb238b) (JAVA 0)")
ALGORITHMS = [
    "9uJNVj/wLmdwKrJaVj/omlQ", "Oz64Lp0GigmChHMf/6TNfxx7O9PyopcczMsnf",
    "Eb+L7Ce+Ej48u", "jKY0", "ASr0zCl6v8W4aidjPK5KHd1Lq3t+vBFf41dqv5+fnOd",
    "wQlozdg6r1qxh0eRmt3QgNXOvSZO6q/GXK", "gmirk+ciAvIgA/cxUUCema47jr/YToixTT+Q6O",
    "5IiCoM9B1/788ntB", "P07JH0h6qoM6TSUAK2aL9T5s2QBVeY9JWvalf", "+oK0AN",
]

PHUB_HOST = "pr-phub.sandai.net"
SHUB_HOST = "hub5btmain.sandai.net"
DCDN_HOST = "dcdnhub-xcloud.sandai.net"

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.backends import default_backend


def aes_ecb_encrypt(key, data):
    pad_len = 16 - (len(data) % 16)
    data = data + bytes([pad_len] * pad_len)
    cipher = Cipher(algorithms.AES(key), modes.ECB(), backend=default_backend())
    enc = cipher.encryptor()
    return enc.update(data) + enc.finalize()


def aes_ecb_decrypt(key, data):
    cipher = Cipher(algorithms.AES(key), modes.ECB(), backend=default_backend())
    dec = cipher.decryptor()
    plain = dec.update(data) + dec.finalize()
    if plain and 1 <= plain[-1] <= 16:
        pad_len = plain[-1]
        if plain[-pad_len:] == bytes([pad_len] * pad_len):
            plain = plain[:-pad_len]
    return plain


def md5_hex(s):
    return hashlib.md5(s.encode() if isinstance(s, str) else s).hexdigest()


def sha1_hex(s):
    return hashlib.sha1(s.encode() if isinstance(s, str) else s).hexdigest()


def generate_device_id(seed):
    if len(seed) == 32:
        return seed
    return md5_hex(seed)


def generate_device_sign(device_id):
    base = f"{device_id}{PACKAGE_NAME}{APPID}{APPKEY}"
    sha1 = sha1_hex(base)
    md5 = md5_hex(sha1)
    return f"div101.{device_id}{md5}"


def try_dcdn_with_base64_aes(device_id):
    """DCDN: base64(AES-ECB(body))"""
    print("\n=== DCDN: base64(AES-ECB(body)) 尝试 ===")
    
    # 构造一个简单的 ping body (推测格式)
    body = b'\x00' * 32  # placeholder
    
    aes_keys = [
        ("device_id_md5", hashlib.md5(device_id.encode()).digest()),
        ("appkey_md5", hashlib.md5(APPKEY.encode()).digest()),
        ("appkey_first16", APPKEY.encode()[:16]),
        ("device_sign_md5", bytes.fromhex(generate_device_sign(device_id)[-32:])[:16]),
        ("client_id_md5", hashlib.md5(CLIENT_ID.encode()).digest()),
        ("client_secret_md5", hashlib.md5(CLIENT_SECRET.encode()).digest()),
        ("appkey_full_32", APPKEY.encode()[:32].ljust(32, b'\x00')[:32]),
    ]
    
    for name, key in aes_keys:
        if len(key) not in [16, 24, 32]:
            continue
        # AES 加密
        encrypted = aes_ecb_encrypt(key, body)
        # Base64 编码
        encoded = base64.b64encode(encrypted)
        
        # POST 到 DCDN
        try:
            req = urllib.request.Request(
                f"http://{DCDN_HOST}/",
                data=encoded,
                headers={
                    'Host': DCDN_HOST,
                    'User-Agent': 'curl/7.64',
                    'Content-Type': 'application/octet-stream',
                },
                method='POST',
            )
            r = urllib.request.urlopen(req, timeout=10)
            resp = r.read()
            print(f"  [{name}] key={key.hex()}")
            print(f"    HTTP {r.status}, resp: {resp[:200]}")
        except urllib.error.HTTPError as e:
            resp = e.read()
            print(f"  [{name}] key={key.hex()}")
            print(f"    HTTP {e.code}, resp: {resp[:200]}")
        except Exception as e:
            print(f"  [{name}] err: {e}")


def try_phub_various_payloads(device_id):
    """PHub: 尝试不同的 body 编码方式"""
    print("\n=== PHub: 各种 body 编码尝试 ===")
    
    # 1. 完全空 body
    print("\n--- 空 body ---")
    try:
        req = urllib.request.Request(
            f"http://{PHUB_HOST}/",
            data=b'',
            headers={
                'Host': PHUB_HOST,
                'User-Agent': 'curl/7.64',
                'Content-Type': 'application/octet-stream',
            },
            method='POST',
        )
        r = urllib.request.urlopen(req, timeout=10)
        print(f"  HTTP {r.status}: {r.read()[:200]}")
    except urllib.error.HTTPError as e:
        print(f"  HTTP {e.code}: {e.read()[:200]}")
    
    # 2. 直接 base64 编码 (无 AES)
    print("\n--- 仅 Base64 编码 (无 AES) ---")
    body = b'\x00' * 32
    encoded = base64.b64encode(body)
    try:
        req = urllib.request.Request(
            f"http://{PHUB_HOST}/",
            data=encoded,
            headers={
                'Host': PHUB_HOST,
                'User-Agent': 'curl/7.64',
                'Content-Type': 'application/octet-stream',
            },
            method='POST',
        )
        r = urllib.request.urlopen(req, timeout=10)
        print(f"  HTTP {r.status}: {r.read()[:200]}")
    except urllib.error.HTTPError as e:
        resp = e.read()
        print(f"  HTTP {e.code}: {resp[:200]}")
    
    # 3. AES-ECB 加密(无 base64)
    print("\n--- 仅 AES-ECB 加密 (无 base64) ---")
    aes_keys = [
        ("device_id_md5", hashlib.md5(device_id.encode()).digest()),
        ("appkey_first16", APPKEY.encode()[:16]),
    ]
    for name, key in aes_keys:
        if len(key) != 16:
            continue
        encrypted = aes_ecb_encrypt(key, b'\x00' * 32)
        try:
            req = urllib.request.Request(
                f"http://{PHUB_HOST}/",
                data=encrypted,
                headers={
                    'Host': PHUB_HOST,
                    'User-Agent': 'curl/7.64',
                    'Content-Type': 'application/octet-stream',
                },
                method='POST',
            )
            r = urllib.request.urlopen(req, timeout=10)
            print(f"  [{name}] HTTP {r.status}: {r.read()[:200]}")
        except urllib.error.HTTPError as e:
            resp = e.read()
            print(f"  [{name}] HTTP {e.code}: {resp[:200]}")
    
    # 4. 带 captcha_sign 头
    print("\n--- 带 X-Captcha-Token 头 ---")
    # 先调云盘拿 captcha_token
    try:
        import json
        cap_req = urllib.request.Request(
            "https://xluser-ssl.xunlei.com/v1/shield/captcha/init",
            data=json.dumps({
                "action": "GET:/v1/user/me",
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": device_id,
                "meta": {"email": "test@test.com"},
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor"
            }).encode(),
            headers={
                'Content-Type': 'application/json',
                'User-Agent': USER_AGENT,
            },
            method='POST',
        )
        cap_r = urllib.request.urlopen(cap_req, timeout=10)
        cap_data = json.loads(cap_r.read())
        cap_token = cap_data.get("captcha_token", "")
        print(f"  拿到 captcha_token: {cap_token[:50]}...")
        
        # 用这个 token 调 PHub
        req = urllib.request.Request(
            f"http://{PHUB_HOST}/",
            data=b'',
            headers={
                'Host': PHUB_HOST,
                'User-Agent': USER_AGENT,
                'Content-Type': 'application/octet-stream',
                'X-Captcha-Token': cap_token,
                'X-Device-ID': device_id,
                'X-Client-ID': CLIENT_ID,
                'X-Client-Version': CLIENT_VERSION,
            },
            method='POST',
        )
        r = urllib.request.urlopen(req, timeout=10)
        print(f"  HTTP {r.status}: {r.read()[:200]}")
    except urllib.error.HTTPError as e:
        resp = e.read()
        print(f"  HTTP {e.code}: {resp[:200]}")
    except Exception as e:
        print(f"  err: {e}")


def main():
    print("="*70)
    print("PoC v2: PHub/DCDN HTTP 客户端 - 多种编码尝试")
    print("="*70)
    
    device_id = generate_device_id("smart-dl-test-001")
    print(f"\n[1] device_id: {device_id}")
    print(f"    device_sign: {generate_device_sign(device_id)}")
    
    # 测 PHub 各种 payload
    try_phub_various_payloads(device_id)
    
    # 测 DCDN base64 + AES
    try_dcdn_with_base64_aes(device_id)


if __name__ == "__main__":
    main()
