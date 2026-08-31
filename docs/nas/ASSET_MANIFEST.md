# 引擎资产权威清单（ASSET MANIFEST）

> 2026-08-31 从官方源 `https://down.sandai.net/nas/nasxunlei-DSM7-x86_64.spk`
> 重获并归档。版本 **3.23.5-0814080017**（INFO.package checksum `620a415d15b4bcaaf546b3d5bfce778e`），
> 与 A2–A5 校准所用版本一致（A2 校准对象 62,765,544 字节逐字节对上）。
> 沙盒重建事故后（热态工作区 `~/.nas-engine-test/` 全灭），本清单为唯一权威基线。

## 归档物

| 文件 | 大小 | SHA256 |
|------|------|--------|
| `research_bin/nas/spk/nasxunlei-DSM7-x86_64.spk` | 27,084,800 | `2874ea8aabf4b7f3b966bf761929661950a86db9b55bc349fd11b181b39a1863` |
| ↳ package.tgz (xz) | — | `07a1bba28c9eba1c3673b2df914066dc9262111f42a6d25b1e5f3674cbe18482` |
| ↳ bin/bin/xunlei-pan-cli.3.23.5.amd64 | 62,765,544 | `fb1fe3401923120bdf47f54a52e8e4c253c6ab11aecc2d89f796074eab12334f` |
| ↳ bin/bin/xunlei-pan-cli-launcher.amd64 | 19,726,336 | `fb59f7b474f58411a063d2577000ae05bbef0031b699802b21f692c22f212e80` |
| ↳ bin/bin/version | 6 | `83e6eaf506d4b24da333a6fea4ca05c6eea553a9703b967ad1887aa80a0feb24` |
| ↳ bin/bin/version_code | 7 | `27f1f9dfd53b7088b4a395bf309a5236c01fd372b10327172c6198f0e1ecc5f6` |

version 文件内容：`3.23.5` / version_code：`3023005`。
SPK 内另含 `pkg/ui/`（DSM 前端 Main.js 等）与 `scripts/service-setup`
（官方 env 配方 6 项，见 A5 FINDINGS §1.2）。

## SPK 结构备注

- 外层 = POSIX tar（非加密非压缩）；`package.tgz` 实为 **xz** 流（名字骗人）。
- 解包四件套即引擎全部：无平台检测文件（A5 XOR 全谱扫描定案的物证基础）。

## 恢复命令（一条链）

```bash
python3 scripts/nas/a6_ops.py extract     # 仓库 SPK -> ~/.nas-engine-test/engine/bin/bin + SHA256 校验
python3 scripts/nas/a6_ops.py envconfig   # BinDir envconfig 群晖配方 (A5)
python3 scripts/nas/a6_ops.py status      # 巡检
```

引擎二进制不直接入 git（62MB 超告警线），以 27MB 官方 SPK 单对象归档，
解包即得全部二进制且哈希可验——比 LFS 少一层配额依赖。

## 上游链路

- 官方下载源：`https://down.sandai.net/nas/nasxunlei-DSM7-x86_64.spk`（cnk3x/xunlei
  容器镜像同款默认值，`XL_SPK`）。该 URL 指向最新版，**复跑哈希前先核对版本号**。
- 社区参考：cnk3x/xunlei（README 与 mockEnv 配方）。
