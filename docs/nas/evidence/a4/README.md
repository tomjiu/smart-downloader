# A4 校准原始证据（Task 31，2026-08-31）

| 文件 | 内容 |
|------|------|
| `a4_final.json` | 首个任务全字段（PHASE_TYPE_ERROR / error=下载(90120) / real_path）+ 轮询 + DELETE 阻塞 |
| `a4_run3.json` | 常规直链重测：创建 200→1s 内 90120 失败、配额 used=0 |
| `a4_run4.json` | rlimit 提升轮：403 task_create_count_limit（每日 3 任务限制实锤）+ 引擎线程/fd 曲线 |
| `a4_diag.json` | filters 形态矩阵 + apply 三载荷回执 + 任务持久化验证 |
| `engine_syno.log` | PLATFORM=群晖 强设实验：platformdetect panic 全文（平台结构字段泄露） |
