/* ffi/lt.h — FFI 契约（v0.6 全量；M0 子集已冻结验收，M1 补齐 ~28 函数）
 * 手写 C ABI：C++ 侧 lt_kernel.cpp 实现；Rust 侧 bindgen 生成声明。
 * 内存规则（D13）：输出缓冲 Rust 预分配 + capacity；C++ 写入 ≤cap；
 *   无 new[]/静态缓冲/所有权转移；字符串定长数组，Rust 立即拷贝。
 * 2.x 事实（ABI100）：无 paused bool / flags_t / flag_paused；
 *   暂停同步点 = torrent_paused alert（§10.1 实测结论）。
 */
#ifndef SMART_DL_LT_H
#define SMART_DL_LT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct lt_session lt_session;

typedef enum {
    LT_OK = 0,
    LT_ERR_ARG,
    LT_ERR_ENGINE,
    LT_ERR_IO,
    LT_ERR_NOT_FOUND,
    LT_ERR_BUFFER_TOO_SMALL
} lt_err;

/* —— 会话生命周期（2）—— */
lt_err lt_session_new(const char* save_path, const char* session_id, lt_session** out);
void   lt_session_free(lt_session* s);
lt_err lt_err_str(lt_session* s, char* buf, size_t cap, size_t* out_len); /* 最近一次错误人类可读 */

/* —— 全局网络策略（代理 + 速率）——
   proxy_type: 0=none 1=socks4 2=socks5 3=socks5+pw 4=http 5=http+pw（对齐 settings_pack）
   proxy_host/proxy_user/proxy_pass 可 NULL；proxy_port 忽略当 type=0。
   down_bytes/up_bytes: ≤0 表示不修改该项（bytes/s；0 本身 = 不限速）。 */
lt_err lt_apply_network(lt_session* s,
                        int proxy_type, const char* proxy_host, int proxy_port,
                        const char* proxy_user, const char* proxy_pass,
                        int64_t down_bytes, int64_t up_bytes);

/* —— 发现层开关（DHT / LSD / UPnP）——
   三个开关独立设置（0=关 1=开）；enable_upnp 同时控制 enable_natpmp
   （端口映射族同进退，不提供单独 NAT-PMP 开关）。
   会话默认全关（M0 确定性语义，见 lt_session_new）；本函数后置覆盖。 */
lt_err lt_apply_discovery(lt_session* s, int enable_dht, int enable_lsd, int enable_upnp);

/* —— 添加/移除（5）—— */
lt_err lt_add_magnet(lt_session* s, const char* magnet, const char** web_seeds, char* ih_out /*41 字节*/);
lt_err lt_add_torrent_file(lt_session* s, const uint8_t* meta, size_t len, const char** web_seeds, char* ih_out);
lt_err lt_add_torrent_resume(lt_session* s, const uint8_t* resume_data, size_t len, const char** web_seeds, char* ih_out);
lt_err lt_pause(lt_session* s, const char* ih);   /* 完成即停；同步点 = torrent_paused alert（D19/D32） */
lt_err lt_resume(lt_session* s, const char* ih);
lt_err lt_remove(lt_session* s, const char* ih, int delete_data);

/* —— 状态/进度查询（6）—— */
typedef struct {
    int     state;             /* libtorrent torrent_status::state；0 下载 1 完成 3 错误 4 元数据获取中。
                                  ABI100 无 paused 状态/无 flags_t：暂停以后续 torrent_paused alert 为同步点
                                  （状态停在暂停前值），暂停态由引擎层维护（§10.1 验证结论）。 */
    float   progress;          /* 0..1（已下载/总字节，metadata 前恒 0） */
    int64_t downloaded, total, down_rate, up_rate;
    int     num_peers, num_seeds;   /* 已连接数（F2） */
    int     metadata_received;      /* F2 三阶段评估前提 */
    int     paused;                 /* torrent_handle::pause() 后的 paused 标志 */
} lt_torrent_status;

lt_err lt_status(lt_session* s, const char* ih, lt_torrent_status* out);
lt_err lt_piece_count(lt_session* s, const char* ih, int* out);
lt_err lt_bitfield(lt_session* s, const char* ih, uint8_t* buf, size_t cap, size_t* out_len); /* 位打包，LSB 先 */
lt_err lt_file_count(lt_session* s, const char* ih, int* out);
lt_err lt_file_progress(lt_session* s, const char* ih, int64_t* done_arr, int64_t* size_arr, int n);

/* —— 子文件优先级（BT 多文件；P1 任务级能力）——
   priority 语义同 libtorrent：0 = 不下载（skip）、1 = 低、4 = 默认、7 = 最高。
   idx_arr/prio_arr 等长（n 项，逐条 set）；n/容量必须 >= 文件数（与
   lt_file_progress 同口径，否则 BUFFER_TOO_SMALL）。需要 metadata。 */
lt_err lt_set_file_priorities(lt_session* s, const char* ih, const int* idx_arr, const int* prio_arr, int n);
lt_err lt_get_file_priorities(lt_session* s, const char* ih, int* out_arr, int n);

/* —— 富 peer（1）—— */
typedef struct {
    char     ip[64];
    uint16_t port;
    char     peer_id[64];      /* hex */
    char     client[128];
    uint32_t progress_ppm;
    int64_t  down_rate, up_rate;
    int64_t  total_download, total_upload;
    int64_t  last_active_sec;  /* <0 = 从未活跃 */
    uint32_t flags;            /* LT_PEER_* 位 */
} lt_peer;
#define LT_PEER_SEED          (1u << 0)
#define LT_PEER_UPLOADER      (1u << 1) /* upload_only */
#define LT_PEER_INTERESTED    (1u << 2)
#define LT_PEER_CHOKED        (1u << 3) /* 我们 choke 对方 */
#define LT_PEER_REMOTE_CHOKED (1u << 4) /* 对方 choke 我们 */
#define LT_PEER_SNUBBED       (1u << 5)
#define LT_PEER_CONNECTING    (1u << 6)
#define LT_PEER_LOCAL         (1u << 7) /* outgoing 连接 */
#define LT_PEER_UTP           (1u << 8)

lt_err lt_peers(lt_session* s, const char* ih, lt_peer* buf, size_t cap, size_t* out_count);

/* —— alert 队列（D31 预算 ≤12 种；扁平化值拷贝，所有权归 wrapper；溢出计数）—— */
typedef enum {
    LT_ALERT_TRACKER  = 1,
    LT_ALERT_PEER     = 2,
    LT_ALERT_ERROR    = 4,
    LT_ALERT_METADATA = 8,
    LT_ALERT_STATE    = 16,
    LT_ALERT_RESUME   = 32,
    LT_ALERT_PIECE    = 64
} lt_alert_mask;

typedef struct {
    int   kind;            /* 对应 mask 位 */
    char  ih[41];          /* 相关 torrent infohash（非 torrent 类为空串） */
    char  msg[512];        /* 人类可读；STATE 桶区分 finished/paused/error（§8.5） */
    int64_t at;            /* 毫秒时间戳 */
    int   resume_ready;    /* RESUME 时：1=可调 lt_take_resume_data */
} lt_alert;

lt_err lt_set_alert_mask(lt_session* s, const char* ih, uint32_t mask);
lt_err lt_pop_alerts(lt_session* s, lt_alert* buf, size_t cap, size_t* out_count);
lt_err lt_alerts_dropped(lt_session* s, uint32_t* out);

/* —— resume 异步流（D16：request→alert→take；数据 C++ 侧持有至 take 拷贝出）—— */
lt_err lt_request_save_resume(lt_session* s, const char* ih);
lt_err lt_take_resume_data(lt_session* s, const char* ih, uint8_t* buf, size_t cap, size_t* out_len);

/* —— 控制/限制（6）—— */
lt_err lt_ban_peer(lt_session* s, const char* ih, const char* ip, uint16_t port); /* v2：Session IP ban 实现 */
lt_err lt_add_peer(lt_session* s, const char* ih, const char* ip, uint16_t port); /* 本地 seeder 直连注入 */
lt_err lt_add_url_seed(lt_session* s, const char* ih, const char* url);
lt_err lt_add_tracker(lt_session* s, const char* ih, const char* url);
lt_err lt_set_sequential(lt_session* s, const char* ih, int on);
lt_err lt_set_limits(lt_session* s, const char* ih, int64_t down_limit, int64_t up_limit); /* 字节/秒；0=不限 */

/* —— 块读取（v2；async read_piece → 轮询取数）—— */
lt_err lt_read_piece(lt_session* s, const char* ih, int idx, uint8_t* buf, size_t buflen, size_t* out_len);

/* —— torrent 元数据导出（B-1：magnet → .torrent）——
   须已收到 metadata（magnet 场景等 status.metadata_received）；
   未就绪/任务不存在 → LT_ERR_NOT_FOUND（err_str 区分）；
   cap 不足 → LT_ERR_BUFFER_TOO_SMALL（out_len=实际长度，Rust 扩容重试）。
   产物 = 标准 .torrent bencode（info dict + announce 族；由 create_torrent(ti).generate() 生成）。 */
lt_err lt_metadata(lt_session* s, const char* ih, uint8_t* buf, size_t cap, size_t* out_len);

#ifdef __cplusplus
}
#endif

#endif /* SMART_DL_LT_H */