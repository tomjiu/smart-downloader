// ffi/src/seed_main.cpp - local test seeder (M0 E2E, no external tools/tracker)
// Behavior: generate 2MB deterministic file -> create v1 torrent -> seed on a port
//   -> print "SEED <magnet> PORT <port>" then run forever.
// Usage: seed_main <port> <save_dir>

#include <libtorrent/session.hpp>
#include <libtorrent/session_params.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/version.hpp>
#include <libtorrent/torrent_handle.hpp>
#include <libtorrent/torrent_status.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/entry.hpp>
#include <libtorrent/bencode.hpp>
#include <libtorrent/bdecode.hpp>
#include <libtorrent/span.hpp>
#include <libtorrent/alert.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/error_code.hpp>

#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <string>
#include <thread>
#include <chrono>
#include <fstream>
#include <memory>
#include <iterator>

namespace {
constexpr const char* kFileName = "m0_test.bin";
constexpr std::int64_t kFileSize = 2 * 1024 * 1024; // 2MB

void hex_encode(const lt::sha1_hash& h, char out[41]) {
    static const char* hex = "0123456789abcdef";
    const char* d = h.data();
    for (int i = 0; i < 20; ++i) {
        const unsigned char b = static_cast<unsigned char>(d[i]);
        out[i * 2] = hex[b >> 4];
        out[i * 2 + 1] = hex[b & 0xF];
    }
    out[40] = '\0';
}

void make_file(const std::string& dir) {
    std::ofstream f(dir + "/" + kFileName, std::ios::binary | std::ios::trunc);
    std::uint64_t x = 0x123456789abcdef0ULL; // fixed-seed deterministic
    std::uint64_t remain = kFileSize;
    while (remain > 0) {
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        const std::uint64_t chunk = remain > 8 ? 8 : remain;
        f.write(reinterpret_cast<const char*>(&x), static_cast<std::streamsize>(chunk));
        remain -= chunk;
    }
}

// ABI=100: only info_section 4-arg ctor survives (from_span/throwing ctors gated out)
lt::torrent_info make_torrent_info(lt::create_torrent& t) {
    lt::entry e = t.generate();
    std::string buf;
    lt::bencode(std::back_inserter(buf), e);
    lt::error_code ec;
    lt::bdecode_node root;
    lt::bdecode(buf.data(), buf.data() + buf.size(), root, ec);
    if (ec) throw std::runtime_error("bdecode failed: " + ec.message());
    lt::bdecode_node info_section = root.dict_find("info");
    if (!info_section) throw std::runtime_error("torrent has no info dict");
#if LIBTORRENT_VERSION_NUM >= 20100
    lt::torrent_info ti(info_section, ec, lt::load_torrent_limits(), lt::from_info_section);
#else
    // 2.0.x 无 from_info_section：回退全文件 bdecode_node ctor（root 即完整 .torrent）
    lt::torrent_info ti(root, ec);
#endif
    if (ec) throw std::runtime_error("torrent_info failed: " + ec.message());
    return ti;
}
} // namespace

int main(int argc, char** argv) {
    const int port = argc > 1 ? std::atoi(argv[1]) : 16889;
    const std::string dir = argc > 2 ? argv[2] : ".";

    try {
        make_file(dir);

#if LIBTORRENT_VERSION_NUM >= 20100
        // ABI=100: create_torrent(file_storage&) excluded; use create_file_entry vector
        lt::create_torrent t(std::vector<lt::create_file_entry>{
            lt::create_file_entry(kFileName, kFileSize)});
#else
        // 2.0.x：无 create_file_entry，create_torrent(file_storage&) 可用
        lt::file_storage fs;
        fs.add_file(kFileName, kFileSize);
        lt::create_torrent t(fs);
#endif
        lt::set_piece_hashes(t, dir); // hashes from disk (2-arg overload, throws)
        lt::torrent_info ti = make_torrent_info(t);
        const lt::sha1_hash v1 = ti.info_hashes().v1;

        lt::settings_pack sp;
        // 2.x: listen_port removed; use listen_interfaces string
        // 本地测试仅需 localhost TCP，避免 VPN/虚拟网卡 + uTP 绑定失败导致 listen_failed_alert 误杀
        sp.set_str(lt::settings_pack::listen_interfaces, "127.0.0.1:" + std::to_string(port) + "/tcp");
        sp.set_bool(lt::settings_pack::enable_upnp, false);
        sp.set_bool(lt::settings_pack::enable_natpmp, false);
        // 本地确定性 e2e：关闭 LSD/DHT，避免 0.0.0.0 监听被多接口宣告后
        // 客户端 fan-out 到全部本地地址、同 peer-id 重复连接被踢（M0 调试实测）
        sp.set_bool(lt::settings_pack::enable_lsd, false);
        sp.set_bool(lt::settings_pack::enable_dht, false);
        // 与客户端内核一致：纯 TCP（uTP 首连退避让 e2e 时序抖动，M1 peers 排查）
        sp.set_bool(lt::settings_pack::enable_incoming_utp, false);
        sp.set_bool(lt::settings_pack::enable_outgoing_utp, false);
        // SEED 行 = 监听已就绪的同步点：等到 listen_succeeded_alert 才打印
        //（客户端首连若被拒，libtorrent 重试间隔很长 → 测试假死；M1 peers 排查实测）
        // 全量 mask：保持 SEED-ALERT 诊断可见
        sp.set_int(lt::settings_pack::alert_mask,
                   static_cast<int>(static_cast<std::uint32_t>(lt::alert_category::all)));
        lt::session ses(sp);

        lt::add_torrent_params p;
        p.ti = std::make_shared<lt::torrent_info>(ti);
        p.save_path = dir;
        ses.add_torrent(p); // file already complete -> seeds

        bool listening = false;
        for (int i = 0; i < 100 && !listening; ++i) {
            std::vector<lt::alert*> als;
            ses.pop_alerts(&als);
            for (const lt::alert* a : als) {
                if (a->type() == lt::listen_failed_alert::alert_type) {
                    auto* fa = static_cast<const lt::listen_failed_alert*>(a);
                    if (fa->socket_type == lt::socket_type_t::tcp) {
                        std::fprintf(stderr, "seed_main listen failed: %s\n", a->message().c_str());
                        return 1;
                    }
                    // uTP/其他传输 listen 失败不影响本地 TCP 测试（M0 e2e 仅需 TCP）
                    std::fprintf(stderr, "seed_main non-tcp listen ignored: %s\n", a->message().c_str());
                }
                if (a->type() == lt::listen_succeeded_alert::alert_type) {
                    listening = true;
                }
            }
            if (!listening) {
                std::this_thread::sleep_for(std::chrono::milliseconds(50));
            }
        }
        if (!listening) {
            std::fprintf(stderr, "seed_main: 5s 内未收到 listen_succeeded_alert\n");
            return 1;
        }

        char ih[41];
        hex_encode(v1, ih);
        std::printf("SEED magnet:?xt=urn:btih:%s PORT %d\n", ih, port);
        std::fflush(stdout);

        lt::torrent_handle th = ses.find_torrent(v1);
        for (;;) {
            std::this_thread::sleep_for(std::chrono::seconds(1));
            // diagnostic: print alerts + torrent status so M0 e2e debugging sees the seeder side
            std::vector<lt::alert*> alerts;
            ses.pop_alerts(&alerts);
            for (const lt::alert* a : alerts) {
                if (dynamic_cast<const lt::peer_connect_alert*>(a) ||
                    dynamic_cast<const lt::peer_disconnected_alert*>(a) ||
                    dynamic_cast<const lt::peer_error_alert*>(a) ||
                    dynamic_cast<const lt::torrent_finished_alert*>(a) ||
                    dynamic_cast<const lt::torrent_error_alert*>(a)) {
                    std::printf("SEED-ALERT: %s\n", a->message().c_str());
                    std::fflush(stdout);
                }
            }
            if (th.is_valid()) {
                const lt::torrent_status st = th.status();
                std::printf("SEED-STATUS: peers=%d seeds=%d progress=%.4f state=%d\n",
                            st.num_peers, st.num_seeds, st.progress,
                            static_cast<int>(st.state));
                std::fflush(stdout);
            }
        }
    } catch (const std::exception& e) {
        std::fprintf(stderr, "seed_main error: %s\n", e.what());
        return 1;
    }
}