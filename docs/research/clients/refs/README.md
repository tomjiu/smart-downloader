# 参考实现克隆区

本目录用于本地克隆竞品/参考实现源码做对照研究（不入库）：

```bash
git clone https://github.com/aria2/aria2.git refs/aria2
git clone https://github.com/ikatson/rqbit.git refs/rqbit
```

> 历史注记：此前两路径以 gitlink（mode 160000）残留在索引中但无 `.gitmodules`，
> 属损坏的 submodule 引用，导致 CI cleanup 阶段
> `fatal: No url found for submodule path ...` 报错。已改为此 README 说明 +
> 手动克隆的方式（见 PR114-AUDIT.md §4.1）。
