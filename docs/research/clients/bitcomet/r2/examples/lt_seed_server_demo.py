"""
lt_seed_server_demo.py — LT-Seed 服务端演示

场景: 用户已下载完成一个文件, 启动 LT-Seed 服务端, 把它作为种子源暴露.
其他客户端可通过 LT-Seed 协议取分片.

用法:
    python3 examples/lt_seed_server_demo.py /path/to/my_file.bin
"""
import asyncio
import hashlib
import logging
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))
from lt_seed_protocol import LtSeedServer, compute_file_sha1, LT_SEED_DEFAULT_PORT


async def main():
    if len(sys.argv) < 2:
        print("Usage: lt_seed_server_demo.py <file_path> [--port PORT]")
        sys.exit(1)

    file_path = sys.argv[1]
    port = LT_SEED_DEFAULT_PORT
    if "--port" in sys.argv:
        port = int(sys.argv[sys.argv.index("--port") + 1])

    if not os.path.exists(file_path):
        print(f"Error: {file_path} not found")
        sys.exit(1)

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )

    print("=" * 60)
    print("LT-Seed 服务端演示")
    print("=" * 60)

    # 1. 计算文件 SHA-1 (BitComet LT-Seed 兼容)
    print(f"\n[1] 计算文件 SHA-1 (BitComet LT-Seed 索引)...")
    start = time.monotonic()
    file_hash = compute_file_sha1(file_path)
    elapsed = time.monotonic() - start
    file_size = os.path.getsize(file_path)
    print(f"    文件路径: {file_path}")
    print(f"    文件大小: {file_size:,} bytes ({file_size/1048576:.2f} MiB)")
    print(f"    SHA-1   : {file_hash}")
    print(f"    耗时    : {elapsed:.2f}s")

    # 2. 启动 LT-Seed 服务端
    print(f"\n[2] 启动 LT-Seed 服务端, 监听端口 {port}...")
    server = LtSeedServer(listen_port=port)
    server.add_file(file_path)
    await server.start()

    print(f"\n✓ LT-Seed 服务端已启动")
    print(f"  监听端口: {port}")
    print(f"  文件 hash: {file_hash}")
    print(f"  本机 endpoint: 127.0.0.1:{port}")
    print(f"\n其他客户端可通过以下方式查询:")
    print(f"  from lt_seed_protocol import LtSeedClient")
    print(f"  client = LtSeedClient(seed_servers=[('127.0.0.1', {port})])")
    print(f"  seeds = await client.query_seeds('{file_hash}')")
    print(f"  data = await client.fetch_piece('{file_hash}', 0)")

    print(f"\n按 Ctrl+C 停止...")
    try:
        while True:
            await asyncio.sleep(5)
            print(f"  [stats] served {server.stats['pieces_served']} pieces "
                  f"({server.stats['bytes_served']:,} bytes), "
                  f"{server.stats['connections']} connections")
    except KeyboardInterrupt:
        print("\n停止服务端...")
        await server.stop()
        print("✓ 已停止")


if __name__ == "__main__":
    asyncio.run(main())
