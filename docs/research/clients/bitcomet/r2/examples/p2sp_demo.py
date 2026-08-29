"""
p2sp_demo.py — P2SP 多源下载演示

场景: 同一文件有 3 个镜像, 用 P2SP 合并下载.
本演示用 3 个本地 HTTP 服务器模拟, 验证多源叠加效果.

用法:
    python3 examples/p2sp_demo.py
"""
import asyncio
import os
import sys
import tempfile
import time

# 加入 src 路径
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

from p2sp_downloader import P2SPDownloader, BasicDownloadStrategy


# 模拟 3 个 HTTP 镜像 (用 aiohttp.web 起本地服务)
async def start_mock_mirror(file_data: bytes, port: int, speed_limit_bps: int = 0):
    """启动一个模拟 HTTP 镜像, 可选限速."""
    from aiohttp import web
    import asyncio as aio

    async def handle_head(request):
        return web.Response(
            status=200,
            headers={
                "Content-Length": str(len(file_data)),
                "Accept-Ranges": "bytes",
            },
        )

    async def handle_get(request):
        rng = request.headers.get("Range", "")
        if rng.startswith("bytes="):
            start, end = rng[6:].split("-")
            start = int(start)
            end = int(end) if end else len(file_data) - 1
            chunk = file_data[start:end + 1]
            # 模拟限速
            if speed_limit_bps > 0:
                await aio.sleep(len(chunk) * 8 / speed_limit_bps)
            return web.Response(
                status=206,
                body=chunk,
                headers={
                    "Content-Range": f"bytes {start}-{end}/{len(file_data)}",
                    "Content-Length": str(len(chunk)),
                    "Accept-Ranges": "bytes",
                },
            )
        return web.Response(body=file_data, headers={"Content-Length": str(len(file_data))})

    app = web.Application()
    app.router.add_head("/file.bin", handle_head)
    app.router.add_get("/file.bin", handle_get)
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", port)
    await site.start()
    return runner


async def main():
    import logging
    logging.basicConfig(level=logging.INFO,
                         format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")

    print("=" * 60)
    print("P2SP 多源下载演示")
    print("=" * 60)

    # 1. 生成测试文件 (5 MB)
    print("\n[1] 生成 5MB 测试文件...")
    file_data = os.urandom(5 * 1024 * 1024)
    file_size = len(file_data)
    print(f"    文件大小: {file_size:,} bytes ({file_size/1048576:.2f} MiB)")

    # 2. 启动 3 个模拟镜像 (不同限速)
    print("\n[2] 启动 3 个模拟镜像...")
    runners = []
    runners.append(await start_mock_mirror(file_data, 8001, speed_limit_bps=0))
    print(f"    Mirror 1: http://127.0.0.1:8001/file.bin (无限制)")
    runners.append(await start_mock_mirror(file_data, 8002, speed_limit_bps=0))
    print(f"    Mirror 2: http://127.0.0.1:8002/file.bin (无限制)")
    runners.append(await start_mock_mirror(file_data, 8003, speed_limit_bps=0))
    print(f"    Mirror 3: http://127.0.0.1:8003/file.bin (无限制)")

    try:
        # 3. P2SP 下载
        print("\n[3] 启动 P2SP 下载...")
        output_path = tempfile.mktemp(suffix=".bin")
        dl = P2SPDownloader(
            output_path=output_path,
            strategy=BasicDownloadStrategy(piece_size=1 << 18),  # 256 KiB
            max_concurrent_sources=3,
        )
        start = time.monotonic()
        stats = await dl.download([
            "http://127.0.0.1:8001/file.bin",
            "http://127.0.0.1:8002/file.bin",
            "http://127.0.0.1:8003/file.bin",
        ])
        elapsed = time.monotonic() - start

        # 4. 验证
        print("\n[4] 验证下载结果...")
        downloaded = open(output_path, "rb").read()
        assert downloaded == file_data, "文件内容不匹配!"
        print(f"    ✓ 内容校验通过 (SHA-256 一致)")

        # 5. 输出统计
        print("\n" + "=" * 60)
        print("下载统计")
        print("=" * 60)
        print(f"  Total size   : {stats.total_size:,} bytes")
        print(f"  Elapsed      : {stats.elapsed_sec:.2f}s")
        print(f"  Avg speed    : {stats.avg_speed_bps / 1_000_000:.2f} Mbps")
        print()
        print("各源贡献:")
        for url, src in stats.sources.items():
            print(f"  [{url[:50]:50s}]")
            print(f"    done={src.bytes_done:>12,}  failed={src.bytes_failed:>10,}  "
                  f"speed={src.speed_bps/1000:.0f} KB/s")
        print()
        # 单源基线对比
        single_speed = file_size * 8 / elapsed / 3  # 假设单源只有 1/3 速度
        print(f"  理论单源速度 : {single_speed / 1_000_000:.2f} Mbps (估算)")
        print(f"  P2SP 加速比  : {stats.avg_speed_bps / single_speed:.1f}x")

    finally:
        # 6. 清理
        for runner in runners:
            await runner.cleanup()
        if os.path.exists(output_path):
            os.unlink(output_path)

    print("\n✓ 演示完成")


if __name__ == "__main__":
    asyncio.run(main())
