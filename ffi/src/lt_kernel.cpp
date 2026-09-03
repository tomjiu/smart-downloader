// ffi/src/lt_kernel.cpp - hand-written C ABI impl (libtorrent 2.1.1, TORRENT_ABI_VERSION=100)
// Contract: ffi/lt.h. Memory rules (D13): output buffers Rust-allocated + capacity.
// 2.x notes: info_hashes()/info_hash(); pop_alerts(&vec); connect_peer() not add_peer();
//   peer_connect_alert naming; ABI>=100 excludes paused bool + flag_paused and old
//   create_torrent(file_storage)/torrent_info(char const*,int) ctors.

#include "lt.h"

#include <libtorrent/session.hpp>
#include <libtorrent/session_params.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/torrent_handle.hpp>
#include <libtorrent/torrent_status.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/file_storage.hpp>
#include <libtorrent/announce_entry.hpp>
#include <libtorrent/peer_info.hpp>
// 版本守卫依据：2.0.x（Debian trixie 2.0.11 / Ubuntu noble 2.0.10）缺 4 个 2.1 API
#include <libtorrent/version.hpp>
#include <libtorrent/read_resume_data.hpp>
#include <libtorrent/write_resume_data.hpp>
#include <libtorrent/alert.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/entry.hpp>
#include <libtorrent/bencode.hpp>
#include <libtorrent/bdecode.hpp>
#include <libtorrent/error_code.hpp>
#include <libtorrent/span.hpp>

#include <chrono>
#include <cstdint>
#include <cstring>
#include <deque>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

namespace {
constexpr size_t kAlertRingCap = 1024;
}

struct lt_session {
    lt::session ses;
    std::string save_path;
    std::mutex mtx;
    std::deque<lt_alert> ring;
    uint32_t dropped = 0;
    uint32_t mask = 0;
    std::string last_err;
    // resume 异步流（D16）：request_save_resume → save_resume_data_alert →
    //   drain 时 bencode 存此 map → lt_take_resume_data 拷贝出（cap 不足则 LT_ERR_BUFFER_TOO_SMALL）
    std::map<std::string, std::vector<char>> resume_map;
    // read_piece 轮询（v2）：lt_read_piece 触发 async read_piece；drain 时存 "ih:idx" → 数据
    std::map<std::string, std::vector<char>> read_map;

    explicit lt_session(const char* path)
        : ses(lt::session_params())
        , save_path(path ? path : "")
    {
        lt::settings_pack sp;
        sp.set_int(lt::settings_pack::alert_mask, lt::alert::all_categories);
        // M0 本地确定性 e2e：关闭 LSD/DHT/UPnP/NATPMP，
        // 避免本地多接口导致 peer fan-out 与重复 peer-id 互踢（M0 调试实测）
        sp.set_bool(lt::settings_pack::enable_lsd, false);
        sp.set_bool(lt::settings_pack::enable_dht, false);
        sp.set_bool(lt::settings_pack::enable_upnp, false);
        sp.set_bool(lt::settings_pack::enable_natpmp, false);
        // v1 内核默认纯 TCP：uTP 首连超时（~2s）+ 随机重试会让 e2e 时序抖动
        //（M1 peers 排查：uTP SYN 超时后 TCP 重连，快照窗口不确定）。uTP 留待后续策略开关。
        sp.set_bool(lt::settings_pack::enable_incoming_utp, false);
        sp.set_bool(lt::settings_pack::enable_outgoing_utp, false);
        ses.apply_settings(sp);
    }
};

namespace {

lt::torrent_handle find_handle(lt_session* s, const char* ih) {
    if (!s || !ih || std::strlen(ih) != 40) return {};
    lt::sha1_hash h;
    auto nibble = [](char c) -> int {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
    };
    for (int i = 0; i < 20; ++i) {
        const int hi = nibble(ih[i * 2]);
        const int lo = nibble(ih[i * 2 + 1]);
        if (hi < 0 || lo < 0) return {};
        h[i] = static_cast<unsigned char>((hi << 4) | lo);
    }
    return s->ses.find_torrent(h); // 2.x: find_torrent takes sha1_hash
}

void hex_encode_v1(const lt::sha1_hash& v1, char* out) {
    static const char* hex = "0123456789abcdef";
    const char* d = v1.data();
    for (int i = 0; i < 20; ++i) {
        const unsigned char b = static_cast<unsigned char>(d[i]);
        out[i * 2] = hex[b >> 4];
        out[i * 2 + 1] = hex[b & 0xF];
    }
    out[40] = '\0';
}

// alert flattening (D31 budget subset; others dropped + counted)
int map_alert_kind(const lt::alert* a) {
    switch (a->type()) {
        case lt::metadata_received_alert::alert_type:       return LT_ALERT_METADATA;
        case lt::torrent_finished_alert::alert_type:
        case lt::torrent_paused_alert::alert_type:
        case lt::torrent_error_alert::alert_type:           return LT_ALERT_STATE;
        case lt::save_resume_data_alert::alert_type:
        case lt::save_resume_data_failed_alert::alert_type: return LT_ALERT_RESUME;
        case lt::tracker_error_alert::alert_type:           return LT_ALERT_TRACKER;
        case lt::peer_connect_alert::alert_type:            // 2.x naming
        case lt::peer_disconnected_alert::alert_type:       return LT_ALERT_PEER;
        case lt::piece_finished_alert::alert_type:          return LT_ALERT_PIECE;
        default:                                            return 0;
    }
}

void fill_ih_from_torrent_alert(const lt::alert* a, char out[41]) {
    out[0] = '\0';
    const auto* ta = dynamic_cast<const lt::torrent_alert*>(a);
    if (ta && ta->handle.is_valid() && ta->handle.info_hashes().has_v1()) {
        hex_encode_v1(ta->handle.info_hashes().v1, out);
    }
}

void drain_session(lt_session* s) {
    std::vector<lt::alert*> alerts;
    s->ses.pop_alerts(&alerts);
    if (alerts.empty()) return;
    std::lock_guard<std::mutex> lk(s->mtx);
    for (const lt::alert* a : alerts) {
        // 异步数据落地（不产生 alert）：resume bencode / read_piece 数据
        // type() + static_cast：不依赖 RTTI（vcpkg libtorrent 可能无 RTTI）
        if (a->type() == lt::save_resume_data_alert::alert_type) {
            const auto* ra = static_cast<const lt::save_resume_data_alert*>(a);
            if (ra->handle.is_valid() && ra->handle.info_hashes().has_v1()) {
                char ih[41];
                hex_encode_v1(ra->handle.info_hashes().v1, ih);
                std::vector<char> buf;
                // ABI>=2：resume 数据在 params；resume_data 成员仅 ABI==1 可见
                const lt::entry e = lt::write_resume_data(ra->params);
                lt::bencode(std::back_inserter(buf), e);
                s->resume_map[ih] = std::move(buf);
            }
        } else if (a->type() == lt::read_piece_alert::alert_type) {
            const auto* rp = static_cast<const lt::read_piece_alert*>(a);
            if (rp->handle.is_valid() && rp->handle.info_hashes().has_v1()
                && rp->size > 0 && rp->buffer) {
                char ih[41];
                hex_encode_v1(rp->handle.info_hashes().v1, ih);
                const std::string key = std::string(ih) + ":"
                    + std::to_string(static_cast<int>(rp->piece));
                s->read_map[key].assign(rp->buffer.get(), rp->buffer.get() + rp->size);
            }
        }
        const int kind = map_alert_kind(a);
        if (kind == 0 || ((s->mask & kind) == 0)) { ++s->dropped; continue; }
        lt_alert fa{};
        fa.kind = kind;
        fa.at = std::chrono::duration_cast<std::chrono::milliseconds>(
                    a->timestamp().time_since_epoch()).count();
        fill_ih_from_torrent_alert(a, fa.ih);
        std::string m = a->message();
        // STATE 桶内区分子类型（D31：finished / paused / error 语义不同）。
        // 用 type() 而非 dynamic_cast（vcpkg libtorrent 可能不带 RTTI，dynamic_cast 静默失效）。
        if (a->type() == lt::torrent_finished_alert::alert_type) {
            m = "torrent finished";
        } else if (a->type() == lt::torrent_paused_alert::alert_type) {
            m = "torrent paused";
        } else if (a->type() == lt::save_resume_data_alert::alert_type) {
            m = "resume ready";
        } else if (a->type() == lt::save_resume_data_failed_alert::alert_type) {
            m = "resume failed";
        } else if (a->type() == lt::tracker_announce_alert::alert_type) {
            m = "tracker announce";
        }
        std::strncpy(fa.msg, m.c_str(), sizeof(fa.msg) - 1);
        fa.msg[sizeof(fa.msg) - 1] = '\0';
        if (a->type() == lt::save_resume_data_alert::alert_type) {
            fa.resume_ready = 1; // bencode data fetched via lt_take_resume_data
        }
        if (s->ring.size() >= kAlertRingCap) { s->ring.pop_front(); ++s->dropped; }
        s->ring.push_back(fa);
    }
}

} // namespace

extern "C" {

static void set_err(lt_session* s, std::string m);

lt_err lt_session_new(const char* save_path, const char* /*session_id*/, lt_session** out) {
    if (!out) return LT_ERR_ARG;
    try {
        *out = new lt_session(save_path);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

void lt_session_free(lt_session* s) { delete s; }

lt_err lt_apply_network(lt_session* s,
                        int proxy_type_in, const char* proxy_host_in, int proxy_port_in,
                        const char* proxy_user_in, const char* proxy_pass_in,
                        int64_t down_bytes, int64_t up_bytes) {
    if (!s) return LT_ERR_ARG;
    try {
        lt::settings_pack sp;
        if (proxy_type_in > 0 && proxy_host_in && proxy_host_in[0]) {
            sp.set_int(lt::settings_pack::proxy_type, proxy_type_in);
            sp.set_str(lt::settings_pack::proxy_hostname, proxy_host_in);
            sp.set_int(lt::settings_pack::proxy_port, proxy_port_in);
            if (proxy_user_in && proxy_user_in[0]) sp.set_str(lt::settings_pack::proxy_username, proxy_user_in);
            if (proxy_pass_in && proxy_pass_in[0]) sp.set_str(lt::settings_pack::proxy_password, proxy_pass_in);
        } else {
            sp.set_int(lt::settings_pack::proxy_type, 0);
        }
        if (down_bytes > 0) sp.set_int(lt::settings_pack::download_rate_limit, static_cast<int>(down_bytes));
        if (up_bytes > 0) sp.set_int(lt::settings_pack::upload_rate_limit, static_cast<int>(up_bytes));
        s->ses.apply_settings(sp);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_apply_discovery(lt_session* s, int enable_dht, int enable_lsd, int enable_upnp) {
    if (!s) return LT_ERR_ARG;
    try {
        // 会话默认全关（M0 确定性语义，见 lt_session_new）；此处显式覆盖三项。
        // enable_upnp 同时控制 enable_natpmp（端口映射族同进退，见 lt.h 契约注释）。
        lt::settings_pack sp;
        sp.set_bool(lt::settings_pack::enable_dht, enable_dht != 0);
        sp.set_bool(lt::settings_pack::enable_lsd, enable_lsd != 0);
        sp.set_bool(lt::settings_pack::enable_upnp, enable_upnp != 0);
        sp.set_bool(lt::settings_pack::enable_natpmp, enable_upnp != 0);
        s->ses.apply_settings(sp);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_add_magnet(lt_session* s, const char* magnet, const char** web_seeds, char* ih_out) {
    if (!s || !magnet || !ih_out) return LT_ERR_ARG;
    try {
        lt::add_torrent_params p = lt::parse_magnet_uri(magnet);
        p.save_path = s->save_path;
        // Bug A 修复（ABI1 兼容）：新增任务即 paused + 禁 auto_managed，
        // 从源头阻止 lt 队列在 metadata/checking 完成后自动复活。
        p.flags &= ~lt::torrent_flags::auto_managed;
        p.flags |= lt::torrent_flags::paused;
        if (web_seeds) {
            for (const char** ws = web_seeds; *ws != nullptr; ++ws) {
                p.url_seeds.emplace_back(*ws);
            }
        }
        const lt::torrent_handle h = s->ses.add_torrent(p);
        const lt::info_hash_t ih = h.info_hashes();
        if (!ih.has_v1()) {
            return LT_ERR_ENGINE; // v2-only magnet: M0 unsupported
        }
        hex_encode_v1(ih.v1, ih_out);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_add_peer(lt_session* s, const char* ih, const char* ip, uint16_t port) {
    if (!s || !ih || !ip) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        lt::error_code ec;
        lt::tcp::endpoint ep(lt::make_address(ip, ec), port);
        if (ec) return LT_ERR_ARG;
        h.connect_peer(ep); // 2.x method name
        return LT_OK;
    } catch (...) {
        set_err(s, "engine error");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_pause(lt_session* s, const char* ih) {
    if (!s || !ih) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        // qBittorrent 同思路：暂停时摘除 auto_managed，禁用 lt 队列自动复活，
        // 队列策略由上层（Rust 调度层）全权管理。上游同步时请重放本行。
        h.unset_flags(lt::torrent_flags::auto_managed);
        h.pause(); // 完成即停；torrent_paused_alert 为同步点（D19/D32）
        return LT_OK;
    } catch (...) {
        set_err(s, "engine error");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_status(lt_session* s, const char* ih, lt_torrent_status* out) {
    if (!s || !ih || !out) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const lt::torrent_status st = h.status();
        out->metadata_received = st.has_metadata ? 1 : 0;
        out->paused = (st.flags & lt::torrent_flags::paused) ? 1 : 0;
        /* E28: torrent 名透出（任务名回填链路）；metadata 前为空串。
           memset 预置 + strncpy 截断安全（缓冲区 256 字节恒 NUL 结尾）。 */
        std::memset(out->name, 0, sizeof(out->name));
        std::strncpy(out->name, st.name.c_str(), sizeof(out->name) - 1);
        switch (st.state) {
            case lt::torrent_status::downloading_metadata:
                out->state = 4;
                break;
            case lt::torrent_status::downloading:
            case lt::torrent_status::checking_files:
            case lt::torrent_status::checking_resume_data:
                out->state = 0;
                break;
            case lt::torrent_status::finished:
            case lt::torrent_status::seeding:
                out->state = 1;
                break;
            default:
                out->state = 3;
                break;
        }
        out->progress = st.progress;
        out->downloaded = st.total_wanted_done;
        out->total = st.total_wanted;
        out->down_rate = st.download_rate;
        out->up_rate = st.upload_rate;
        out->num_peers = st.num_peers;
        out->num_seeds = st.num_seeds;
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_set_alert_mask(lt_session* s, const char* /*ih*/, uint32_t mask) {
    if (!s) return LT_ERR_ARG;
    std::lock_guard<std::mutex> lk(s->mtx);
    s->mask = mask;
    return LT_OK;
}

lt_err lt_pop_alerts(lt_session* s, lt_alert* buf, size_t cap, size_t* out_count) {
    if (!s || !buf || !out_count) return LT_ERR_ARG;
    try {
        drain_session(s);
        std::lock_guard<std::mutex> lk(s->mtx);
        const size_t n = s->ring.size() < cap ? s->ring.size() : cap;
        for (size_t i = 0; i < n; ++i) buf[i] = s->ring[i];
        s->ring.erase(s->ring.begin(), s->ring.begin() + static_cast<std::ptrdiff_t>(n));
        *out_count = n;
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_alerts_dropped(lt_session* s, uint32_t* out) {
    if (!s || !out) return LT_ERR_ARG;
    std::lock_guard<std::mutex> lk(s->mtx);
    *out = s->dropped;
    return LT_OK;
}

// —— M1 全量（§8.3）——

static void set_err(lt_session* s, std::string m) {
    if (s && s->last_err != m) s->last_err = std::move(m);
}

static lt_err fill_ih(lt_session* s, const lt::add_torrent_params& p, const char** web_seeds, char* ih_out) {
    try {
        lt::torrent_handle h;
        if (p.ti) {
            lt::error_code ec;
            h = s->ses.add_torrent(p, ec);
            if (ec) { set_err(s, "add_torrent: " + ec.message()); return LT_ERR_IO; }
        } else {
            h = s->ses.add_torrent(p);
        }
        (void)web_seeds;
        const lt::info_hash_t ih = h.info_hashes();
        if (!ih.has_v1()) { set_err(s, "v2-only torrent unsupported"); return LT_ERR_ENGINE; }
        hex_encode_v1(ih.v1, ih_out);
        return LT_OK;
    } catch (...) {
        set_err(s, "engine error");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_err_str(lt_session* s, char* buf, size_t cap, size_t* out_len) {
    if (!s || !buf || !out_len) return LT_ERR_ARG;
    const std::string m = s->last_err.empty() ? "ok" : s->last_err;
    if (cap < m.size() + 1) { *out_len = m.size() + 1; return LT_ERR_BUFFER_TOO_SMALL; }
    std::memcpy(buf, m.c_str(), m.size() + 1);
    *out_len = m.size(); // 不含 NUL
    return LT_OK;
}

static void set_web_seeds(lt::add_torrent_params& p, const char** web_seeds) {
    if (web_seeds) {
        for (const char** ws = web_seeds; *ws != nullptr; ++ws) {
            p.url_seeds.emplace_back(*ws);
        }
    }
}

lt_err lt_add_torrent_file(lt_session* s, const uint8_t* meta, size_t len, const char** web_seeds, char* ih_out) {
    if (!s || !meta || !len || !ih_out) return LT_ERR_ARG;
    try {
        lt::error_code ec;
        lt::bdecode_node node = lt::bdecode(
            lt::span<const char>(reinterpret_cast<const char*>(meta), len), ec);
        if (ec) { set_err(s, "torrent bdecode: " + ec.message()); return LT_ERR_IO; }
        // ABI100：旧 ctor（bdecode_node 全文件/from_span）编译期移除；
        // 新 API = info-section 构造（from_info_section_t 标签）
        const lt::bdecode_node info = node.dict_find("info");
        if (!info) { set_err(s, "torrent parse: no info section"); return LT_ERR_IO; }
#if LIBTORRENT_VERSION_NUM >= 20100
        auto ti = std::make_shared<lt::torrent_info>(
            info, ec, lt::load_torrent_limits{}, lt::from_info_section);
#else
        // 2.0.x 无 from_info_section：回退全文件 bdecode_node ctor（node 即完整 .torrent）
        (void)info;
        auto ti = std::make_shared<lt::torrent_info>(node, ec);
#endif
        if (ec) { set_err(s, "torrent parse: " + ec.message()); return LT_ERR_IO; }
        lt::add_torrent_params p;
        p.ti = std::move(ti);
        p.save_path = s->save_path;
        p.flags &= ~lt::torrent_flags::auto_managed;
        p.flags |= lt::torrent_flags::paused;
        set_web_seeds(p, web_seeds);
        return fill_ih(s, p, web_seeds, ih_out);
    } catch (...) {
        set_err(s, "torrent parse exception");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_add_torrent_resume(lt_session* s, const uint8_t* resume_data, size_t len, const char** web_seeds, char* ih_out) {
    if (!s || !resume_data || !len || !ih_out) return LT_ERR_ARG;
    try {
        lt::error_code ec;
        lt::add_torrent_params p = lt::read_resume_data(
            lt::span<const char>(reinterpret_cast<const char*>(resume_data), len), ec);
        if (ec) { set_err(s, "resume parse: " + ec.message()); return LT_ERR_IO; }
        p.save_path = s->save_path;
        p.flags &= ~lt::torrent_flags::auto_managed;
        p.flags |= lt::torrent_flags::paused;
        set_web_seeds(p, web_seeds);
        return fill_ih(s, p, web_seeds, ih_out);
    } catch (...) {
        set_err(s, "resume parse exception");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_resume(lt_session* s, const char* ih) {
    if (!s || !ih) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        h.resume();
        return LT_OK;
    } catch (...) {
        set_err(s, "engine error");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_remove(lt_session* s, const char* ih, int delete_data) {
    if (!s || !ih) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        s->ses.remove_torrent(h, delete_data ? lt::session_handle::delete_files
                                             : lt::remove_flags_t{});
        return LT_OK;
    } catch (...) {
        set_err(s, "engine error");
        return LT_ERR_ENGINE;
    }
}

lt_err lt_piece_count(lt_session* s, const char* ih, int* out) {
    if (!s || !ih || !out) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        *out = h.status().num_pieces;
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_bitfield(lt_session* s, const char* ih, uint8_t* buf, size_t cap, size_t* out_len) {
    if (!s || !ih || !out_len) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const lt::torrent_status st = h.status();
        const int np = st.num_pieces;
        if (!st.has_metadata || np <= 0) { *out_len = 0; return LT_OK; }
        const size_t needed = static_cast<size_t>((np + 7) / 8);
        if (!buf || cap < needed) { *out_len = needed; return LT_ERR_BUFFER_TOO_SMALL; }
        std::memset(buf, 0, needed);
        for (int i = 0; i < np; ++i) {
            if (st.pieces.get_bit(lt::piece_index_t{i})) {
                buf[i / 8] |= static_cast<uint8_t>(1u << (i % 8));
            }
        }
        *out_len = needed;
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_file_count(lt_session* s, const char* ih, int* out) {
    if (!s || !ih || !out) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const std::shared_ptr<const lt::torrent_info> tf = h.torrent_file();
        if (!tf) { set_err(s, "metadata not available"); return LT_ERR_NOT_FOUND; }
        *out = tf->num_files();
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_file_progress(lt_session* s, const char* ih, int64_t* done_arr, int64_t* size_arr, int n) {
    if (!s || !ih || !done_arr || !size_arr || n <= 0) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const std::shared_ptr<const lt::torrent_info> tf = h.torrent_file();
        if (!tf) { set_err(s, "metadata not available"); return LT_ERR_NOT_FOUND; }
        const int nf = tf->num_files();
        if (n < nf) return LT_ERR_BUFFER_TOO_SMALL;
        std::vector<int64_t> prog;
        h.file_progress(prog);
        for (int i = 0; i < nf; ++i) {
            done_arr[i] = prog[i];
#if LIBTORRENT_VERSION_NUM >= 20100
            size_arr[i] = tf->files_impl().file_size(lt::file_index_t{i}); // ABI100：files() 仅 ABI<4
#else
            size_arr[i] = tf->files().file_size(lt::file_index_t{i});
#endif
        }
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_set_file_priorities(lt_session* s, const char* ih, const int* idx_arr, const int* prio_arr, int n) {
    if (!s || !ih || !idx_arr || !prio_arr || n <= 0) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const std::shared_ptr<const lt::torrent_info> tf = h.torrent_file();
        if (!tf) { set_err(s, "metadata not available"); return LT_ERR_NOT_FOUND; }
        const int nf = tf->num_files();
        // 两段式：先全量校验再逐条 file_priority(index, prio) 应用
        //（注意 libtorrent 异步记账：设后立即查可能读到旧值，以 file_prio_alert 为准）
        for (int i = 0; i < n; ++i) {
            if (idx_arr[i] < 0 || idx_arr[i] >= nf) { set_err(s, "file index out of range"); return LT_ERR_ARG; }
            if (prio_arr[i] < 0 || prio_arr[i] > 7) { set_err(s, "priority out of range (0..=7)"); return LT_ERR_ARG; }
        }
        for (int i = 0; i < n; ++i) {
            h.file_priority(lt::file_index_t{idx_arr[i]},
                            lt::download_priority_t{static_cast<std::uint8_t>(prio_arr[i])});
        }
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_get_file_priorities(lt_session* s, const char* ih, int* out_arr, int n) {
    if (!s || !ih || !out_arr || n <= 0) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const std::shared_ptr<const lt::torrent_info> tf = h.torrent_file();
        if (!tf) { set_err(s, "metadata not available"); return LT_ERR_NOT_FOUND; }
        const std::vector<lt::download_priority_t> prios = h.get_file_priorities();
        const int nf = static_cast<int>(prios.size());
        if (n < nf) return LT_ERR_BUFFER_TOO_SMALL;
        for (int i = 0; i < nf; ++i) {
            out_arr[i] = static_cast<int>(static_cast<std::uint8_t>(prios[i]));
        }
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

static void hex_encode_20(const char* data, char* out, size_t cap) {
    static const char* hex = "0123456789abcdef";
    for (int i = 0; i < 20 && static_cast<size_t>(i * 2 + 2) <= cap; ++i) {
        const unsigned char b = static_cast<unsigned char>(data[i]);
        out[i * 2] = hex[b >> 4];
        out[i * 2 + 1] = hex[b & 0xF];
    }
}

lt_err lt_peers(lt_session* s, const char* ih, lt_peer* buf, size_t cap, size_t* out_count) {
    if (!s || !ih || !out_count) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        std::vector<lt::peer_info> v;
        h.get_peer_info(v);
        // D13 内存契约：cap 不足 → 报所需尺寸 + LT_ERR_BUFFER_TOO_SMALL（Rust 扩容重试）
        // 空列表直接 OK（避免 cap=0 空转）
        const size_t total = v.size();
        if (total == 0) { *out_count = 0; return LT_OK; }
        if (!buf || cap < total) { *out_count = total; return LT_ERR_BUFFER_TOO_SMALL; }
        for (size_t i = 0; i < total; ++i) {
            const lt::peer_info& pi = v[i];
            lt_peer& o = buf[i];
            std::memset(&o, 0, sizeof(o));
#if LIBTORRENT_VERSION_NUM >= 20100
            // ABI100：ip 字段由 remote_endpoint() 提供（ip 成员仅 ABI==1）
            const lt::tcp::endpoint ep = pi.remote_endpoint();
#else
            const lt::tcp::endpoint ep = pi.ip;
#endif
            const std::string ipstr = ep.address().to_string();
            std::strncpy(o.ip, ipstr.c_str(), sizeof(o.ip) - 1);
            o.port = ep.port();
            const std::string pid = pi.pid.to_string();
            hex_encode_20(pid.c_str(), o.peer_id, sizeof(o.peer_id));
            std::strncpy(o.client, pi.client.c_str(), sizeof(o.client) - 1);
            o.progress_ppm = pi.progress_ppm;
            o.down_rate = pi.payload_down_speed;
            o.up_rate = pi.payload_up_speed;
            o.total_download = pi.total_download;
            o.total_upload = pi.total_upload;
            o.last_active_sec = std::chrono::duration_cast<std::chrono::seconds>(
                                    pi.last_active).count();
            const lt::peer_flags_t f = pi.flags; // ABI100：类型在命名空间级（peer_info 内 using 仅 ABI==1）
            if (f & lt::peer_info::seed) o.flags |= LT_PEER_SEED;
            if (f & lt::peer_info::upload_only) o.flags |= LT_PEER_UPLOADER;
            if (f & lt::peer_info::interesting) o.flags |= LT_PEER_INTERESTED;
            if (f & lt::peer_info::choked) o.flags |= LT_PEER_CHOKED;
            if (f & lt::peer_info::remote_choked) o.flags |= LT_PEER_REMOTE_CHOKED;
            if (f & lt::peer_info::snubbed) o.flags |= LT_PEER_SNUBBED;
            if (f & lt::peer_info::connecting) o.flags |= LT_PEER_CONNECTING;
            if (f & lt::peer_info::outgoing_connection) o.flags |= LT_PEER_LOCAL;
            if (f & lt::peer_info::utp_socket) o.flags |= LT_PEER_UTP;
        }
        *out_count = total;
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

/* —— tracker 运行时增删查（E29；契约与 lt_peers 同两段式）—— */
lt_err lt_list_trackers(lt_session* s, const char* ih, lt_tracker_info* out, int cap, int* out_len) {
    if (!s || !ih || !out_len) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const std::vector<lt::announce_entry> v = h.trackers();
        const size_t total = v.size();
        if (total == 0) { *out_len = 0; return LT_OK; }
        if (!out || (size_t)cap < total) { *out_len = (int)total; return LT_ERR_BUFFER_TOO_SMALL; }
        for (size_t i = 0; i < total; ++i) {
            lt_tracker_info& o = out[i];
            std::memset(&o, 0, sizeof(o));
            std::strncpy(o.url, v[i].url.c_str(), sizeof(o.url) - 1);
            o.tier = v[i].tier;
        }
        *out_len = (int)total;
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_remove_tracker(lt_session* s, const char* ih, const char* url) {
    if (!s || !ih || !url) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        // libtorrent 2.0 无 remove_tracker（1.1 曾提供）→ 版本可移植方案：
        // replace_trackers 过滤式删除（按 URL 精确匹配）。
        // daemon 语义要求"删不存在的 tracker"可定性 404（libtorrent 原生
        // 删除对无匹配静默 no-op，故先扫描确认存在）。
        const std::vector<lt::announce_entry> v = h.trackers();
        std::vector<lt::announce_entry> kept;
        kept.reserve(v.size());
        bool found = false;
        for (const lt::announce_entry& e : v) {
            if (e.url == url) { found = true; continue; }
            kept.push_back(e);
        }
        if (!found) { set_err(s, "tracker not found"); return LT_ERR_NOT_FOUND; }
        h.replace_trackers(kept);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_request_save_resume(lt_session* s, const char* ih) {
    if (!s || !ih) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        h.save_resume_data(lt::torrent_handle::flush_disk_cache);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_take_resume_data(lt_session* s, const char* ih, uint8_t* buf, size_t cap, size_t* out_len) {
    if (!s || !ih || !out_len) return LT_ERR_ARG;
    std::lock_guard<std::mutex> lk(s->mtx);
    const auto it = s->resume_map.find(ih);
    if (it == s->resume_map.end()) { set_err(s, "resume data not ready"); return LT_ERR_NOT_FOUND; }
    const size_t sz = it->second.size();
    if (!buf || cap < sz) { *out_len = sz; return LT_ERR_BUFFER_TOO_SMALL; }
    std::memcpy(buf, it->second.data(), sz);
    *out_len = sz;
    return LT_OK;
}

lt_err lt_ban_peer(lt_session* s, const char* /*ih*/, const char* /*ip*/, uint16_t /*port*/) {
    // v2：2.x 公开 API 无 per-endpoint ban（ban_ip 在 aux_ 内部）；v1 存根
    if (!s) return LT_ERR_ARG;
    set_err(s, "ban_peer: not implemented (2.x public API lacks endpoint ban)");
    return LT_ERR_ENGINE;
}

lt_err lt_add_url_seed(lt_session* s, const char* ih, const char* url) {
    if (!s || !ih || !url) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        h.add_url_seed(url);
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_add_tracker(lt_session* s, const char* ih, const char* url) {
    if (!s || !ih || !url) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        h.add_tracker(lt::announce_entry(url));
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_set_sequential(lt_session* s, const char* ih, int on) {
    if (!s || !ih) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
#if LIBTORRENT_VERSION_NUM >= 20100
        // 2.x：set_sequential_download 仅 ABI==1；全局开关由 range API 表达：
        // on → 从第 0 片起顺序下载；off → 无全局解除 API，no-op（等待 range 自然耗尽/寻址策略接管）
        if (on) {
            h.set_sequential_range(lt::piece_index_t{0});
        }
#else
        // 2.0.x 无 set_sequential_range：classic sequential_download flag（on/off 均可）
        if (on) h.set_flags(lt::torrent_flags::sequential_download);
        else h.unset_flags(lt::torrent_flags::sequential_download);
#endif
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_set_limits(lt_session* s, const char* ih, int64_t down_limit, int64_t up_limit) {
    if (!s || !ih) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const int64_t dl = down_limit > INT32_MAX ? INT32_MAX : down_limit;
        const int64_t ul = up_limit > INT32_MAX ? INT32_MAX : up_limit;
        h.set_download_limit(static_cast<int>(dl));
        h.set_upload_limit(static_cast<int>(ul));
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_read_piece(lt_session* s, const char* ih, int idx, uint8_t* buf, size_t buflen, size_t* out_len) {
    if (!s || !ih || !out_len) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        const std::string key = std::string(ih) + ":" + std::to_string(idx);
        // 触发 async read_piece（数据在下次 drain 时落地 read_map；轮询语义：未就绪 → NOT_FOUND）
        h.read_piece(lt::piece_index_t{idx});
        {
            std::lock_guard<std::mutex> lk(s->mtx);
            const auto it = s->read_map.find(key);
            if (it == s->read_map.end()) return LT_ERR_NOT_FOUND;
            const size_t sz = it->second.size();
            if (!buf || buflen < sz) { *out_len = sz; return LT_ERR_BUFFER_TOO_SMALL; }
            std::memcpy(buf, it->second.data(), sz);
            *out_len = sz;
            s->read_map.erase(it); // 一次性消费
        }
        return LT_OK;
    } catch (...) {
        return LT_ERR_ENGINE;
    }
}

lt_err lt_metadata(lt_session* s, const char* ih, uint8_t* buf, size_t cap, size_t* out_len) {
    if (!s || !ih || !out_len) return LT_ERR_ARG;
    try {
        const lt::torrent_handle h = find_handle(s, ih);
        if (!h.is_valid()) { set_err(s, "torrent not found"); return LT_ERR_NOT_FOUND; }
        // B-1：metadata 未就绪（magnet 尚未从 peer/DHT 拿到 info dict）→ NOT_FOUND
        const std::shared_ptr<const lt::torrent_info> ti = h.torrent_file();
        if (!ti) { set_err(s, "metadata not received"); return LT_ERR_NOT_FOUND; }
        // create_torrent(ti) → generate → bencode：由 torrent_info 重建标准 .torrent
        // 字节（info dict 原样 + announce 族回填；v1 torrent 为无损往返）。
        lt::create_torrent ct(*ti);
        const lt::entry e = ct.generate();
        std::vector<char> data;
        lt::bencode(std::back_inserter(data), e);
        const size_t sz = data.size();
        if (!buf || cap < sz) { *out_len = sz; return LT_ERR_BUFFER_TOO_SMALL; }
        std::memcpy(buf, data.data(), sz);
        *out_len = sz;
        return LT_OK;
    } catch (...) {
        set_err(s, "engine error");
        return LT_ERR_ENGINE;
    }
}

} // extern "C"