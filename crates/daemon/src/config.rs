/// serve 配置（TOML）：HTTP 监听地址 / 默认下载目录 / BT 引擎开关 / 单实例锁路径 /
/// 云兜底 Provider / 任务持久化。
/// 文件缺失时使用默认值（Config::default）；`--config <path>` 覆盖。
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerCfg,
    pub download: DownloadCfg,
    pub bt: BtCfg,
    pub xunlei: XunleiCfg,
    pub provider: ProviderCfg,
    pub provider_xunlei: ProviderXunleiCfg,
    pub lock: LockCfg,
    pub storage: StorageCfg,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ServerCfg {
    /// HTTP/WS 监听地址，如 `127.0.0.1:8787`。
    pub addr: String,
    /// 安全修复（V1/V13）：HTTP API Bearer token。配置后全端点（含 /ws 握手）
    /// 要求 `Authorization: Bearer <token>`；未配置时：回环监听放行（本机 CLI
    /// 兼容），**非回环监听拒绝启动**（fail-closed，防 0.0.0.0 裸奔）。
    /// 不参与热重载（避免认证态中途抖动）。敏感项：不出现在 `/config` 快照。
    pub http_token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DownloadCfg {
    /// 默认下载落盘根目录（三 add 入口的 dest 缺省值）。
    pub dest_root: PathBuf,
    /// 全局代理（HTTP 引擎 + BT 引擎共用）：`http://host:port` / `socks5://host:port` /
    /// `socks4://host:port`（BT 支持带凭据 `user:pass@`）；空 = 直连。启动时生效
    /// （proxy 不参与热重载，避免重建连接）。敏感项：不出现在 `/config` 快照。
    pub proxy: String,
    /// 全局下载限速（KiB/s，HTTP + BT 共用）；0 = 不限。启动时生效。
    pub max_download_kb_s: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BtCfg {
    /// 启用 BT 引擎（需编译时 --features bt）。
    pub enabled: bool,
    /// BT 落盘目录（须存在；默认与 dest_root 相同）。
    pub save_path: Option<PathBuf>,
    /// BT 上传限速（KiB/s）；0 = 不限。启动时生效。
    pub max_upload_kb_s: u32,
    /// 启用 DHT（去中心化 peer 发现）。默认关闭保持确定性；纯磁力无 tracker
    /// 冷启动可开启。启动时生效。
    pub enable_dht: bool,
    /// 启用 LSD（本地网络 peer 发现）。默认关闭保持确定性。启动时生效。
    pub enable_lsd: bool,
    /// 启用 UPnP/NAT-PMP 端口映射（两者同进退）。默认关闭保持确定性。启动时生效。
    pub enable_upnp: bool,
}

/// 迅雷 SDK 引擎配置（Windows-only）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct XunleiCfg {
    /// 启用迅雷 SDK 引擎（需编译时 --features xunlei）。
    pub enabled: bool,
    /// 迅雷 SDK 目录（包含 DownloadSDKProxy.dll 等文件）。
    pub sdk_dir: Option<PathBuf>,
    /// 落盘目录（须存在；默认与 dest_root 相同）。
    pub save_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProviderCfg {
    /// 云兜底总开关（`POST /tasks/:id/fallback` 需要 ≥1 个可用 provider）。
    /// 默认关：不自动烧配额；显式开启才注入 provider 列表。
    pub enabled: bool,
    /// 开发/演示用 MockProvider（仅有的现成实现；真实 provider 待迅雷线落地）。
    /// 仅当 `enabled=true` 时生效。
    pub mock: bool,
}

/// 迅雷云盘 Provider（XunleiProvider）装配配置。
/// 默认关：需显式 `enabled=true` 才把 XunleiProvider 注入 provider 列表。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProviderXunleiCfg {
    pub enabled: bool,
    /// 登录态 JSON 路径（examples 会写入；daemon 只读加载 + 续期回写）。
    /// 缺省时 daemon 用 `xunlei_auth.json`。
    pub token_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LockCfg {
    /// 单实例锁文件路径（重复启动 → 拒绝）。
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StorageCfg {
    /// 任务持久化文件（add/remove/状态变更自动落盘；启动时恢复）。
    pub tasks_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerCfg {
                addr: "127.0.0.1:8787".into(),
                http_token: None,
            },
            download: DownloadCfg {
                dest_root: PathBuf::from("./downloads"),
                proxy: String::new(),
                max_download_kb_s: 0,
            },
            bt: BtCfg {
                enabled: true,
                save_path: None,
                max_upload_kb_s: 0,
                enable_dht: false,
                enable_lsd: false,
                enable_upnp: false,
            },
            xunlei: XunleiCfg::default(),
            provider: ProviderCfg {
                enabled: false,
                mock: false,
            },
            provider_xunlei: ProviderXunleiCfg::default(),
            lock: LockCfg {
                path: PathBuf::from("./daemon.lock"),
            },
            storage: StorageCfg {
                tasks_path: PathBuf::from("./tasks.json"),
            },
        }
    }
}

impl Config {
    /// 从 TOML 文件加载；文件不存在 → 默认值；解析失败 → Err（含行号）。
    pub fn load(path: Option<&std::path::Path>) -> Result<Config, String> {
        let Some(p) = path else {
            return Ok(Config::default());
        };
        let text = std::fs::read_to_string(p).map_err(|e| format!("读取配置 {p:?} 失败: {e}"))?;
        toml::from_str(&text).map_err(|e| format!("配置解析失败 {p:?}: {e}"))
    }

    /// 判定 addr 是否仅绑定回环地址（127.x/::1/localhost）。
    /// serve 启动检查用：非回环 + 无 http_token → 拒绝启动（V1 fail-closed）。
    pub fn is_loopback_addr(addr: &str) -> bool {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host == "localhost"
            || host.starts_with("127.")
            || host == "::1"
            || host == "[::1]"
    }

    /// BT 实际落盘目录（save_path 或默认 dest_root）。
    pub fn bt_save_path(&self) -> PathBuf {
        self.bt
            .save_path
            .clone()
            .unwrap_or_else(|| self.download.dest_root.clone())
    }

    /// 迅雷实际落盘目录（save_path 或默认 dest_root）。
    pub fn xunlei_save_path(&self) -> PathBuf {
        self.xunlei
            .save_path
            .clone()
            .unwrap_or_else(|| self.download.dest_root.clone())
    }

    /// 精简配置快照（`GET /config` 返回；不含敏感项——proxy 可能带凭据故隐藏；
    /// serve 注入 + 热重载共用）。
    pub fn snapshot_json(&self, tasks_path: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "dest_root": self.download.dest_root,
            "bt_save_path": self.bt_save_path(),
            "bt_enabled": self.bt.enabled,
            "bt_enable_dht": self.bt.enable_dht,
            "bt_enable_lsd": self.bt.enable_lsd,
            "bt_enable_upnp": self.bt.enable_upnp,
            "xunlei_enabled": self.xunlei.enabled,
            "listen_addr": self.server.addr,
            // 安全修复（V1）：仅暴露是否启用认证（布尔），token 本身绝不出快照
            "http_token_enabled": self
                .server
                .http_token
                .as_deref()
                .map(|t| !t.is_empty())
                .unwrap_or(false),
            "persist_path": tasks_path,
            "max_download_kb_s": self.download.max_download_kb_s,
            "max_upload_kb_s": self.bt.max_upload_kb_s,
            "proxy_enabled": !self.download.proxy.is_empty(),
            "provider_enabled": self.provider.enabled,
            "provider_xunlei_enabled": self.provider_xunlei.enabled,
            // 仅暴露「登录态文件是否存在」布尔，不泄露路径字符串本身。
            "provider_xunlei_token_exists": self
                .provider_xunlei
                .token_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_token_not_in_snapshot() {
        // 安全回归（V1）：快照只给布尔，绝不泄露 token 本身
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[server]\nhttp_token = \"s3cret\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.server.http_token.as_deref(), Some("s3cret"));
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["http_token_enabled"], true);
        let raw = snap.to_string();
        assert!(!raw.contains("s3cret"), "token 不得出现在快照: {raw}");
    }

    #[test]
    fn loopback_addr_detection() {
        assert!(Config::is_loopback_addr("127.0.0.1:8787"));
        assert!(Config::is_loopback_addr("127.9.1.1:80"));
        assert!(Config::is_loopback_addr("localhost:8787"));
        assert!(Config::is_loopback_addr("[::1]:8787"));
        assert!(!Config::is_loopback_addr("0.0.0.0:8787"));
        assert!(!Config::is_loopback_addr("192.168.1.5:8787"));
        assert!(!Config::is_loopback_addr(":::8787"));
    }

    #[test]
    fn default_values() {
        let c = Config::default();
        assert_eq!(c.server.addr, "127.0.0.1:8787");
        assert_eq!(c.download.dest_root, PathBuf::from("./downloads"));
        assert!(c.bt.enabled);
        assert_eq!(c.lock.path, PathBuf::from("./daemon.lock"));
        assert_eq!(c.bt_save_path(), c.download.dest_root);
        // 发现层开关默认全关（不改变现有行为）
        assert!(!c.bt.enable_dht);
        assert!(!c.bt.enable_lsd);
        assert!(!c.bt.enable_upnp);
    }

    #[test]
    fn bt_discovery_defaults_off_in_snapshot() {
        // 快照含三键且默认 false
        let c = Config::default();
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["bt_enable_dht"], false);
        assert_eq!(snap["bt_enable_lsd"], false);
        assert_eq!(snap["bt_enable_upnp"], false);
    }

    #[test]
    fn bt_discovery_toml_overrides() {
        // TOML 开启值解析 + 快照反映开启值
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            r#"
[bt]
enable_dht = true
enable_lsd = true
enable_upnp = true
"#,
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.bt.enable_dht);
        assert!(c.bt.enable_lsd);
        assert!(c.bt.enable_upnp);
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["bt_enable_dht"], true);
        assert_eq!(snap["bt_enable_lsd"], true);
        assert_eq!(snap["bt_enable_upnp"], true);

        // 部分开启：仅 dht
        let p2 = dir.path().join("config2.toml");
        std::fs::write(&p2, "[bt]\nenable_dht = true\n").unwrap();
        let c2 = Config::load(Some(&p2)).unwrap();
        assert!(c2.bt.enable_dht);
        assert!(!c2.bt.enable_lsd);
        assert!(!c2.bt.enable_upnp);
    }

    #[test]
    fn parse_toml_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            r#"
[server]
addr = "0.0.0.0:9999"

[download]
dest_root = "/data/dl"
proxy = "socks5://u:p@127.0.0.1:1080"
max_download_kb_s = 2048

[bt]
enabled = false
save_path = "/data/bt"
max_upload_kb_s = 512

[provider]
enabled = true
mock = true

[lock]
path = "/tmp/sd.lock"
"#,
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.server.addr, "0.0.0.0:9999");
        assert_eq!(c.download.dest_root, PathBuf::from("/data/dl"));
        assert_eq!(c.download.proxy, "socks5://u:p@127.0.0.1:1080");
        assert_eq!(c.download.max_download_kb_s, 2048);
        assert!(!c.bt.enabled);
        assert_eq!(c.bt_save_path(), PathBuf::from("/data/bt"));
        assert_eq!(c.bt.max_upload_kb_s, 512);
        assert!(c.provider.enabled);
        assert!(c.provider.mock);
        assert_eq!(c.lock.path, PathBuf::from("/tmp/sd.lock"));
        // 快照：含限速、代理仅暴露开关（不泄露凭据）、provider 开关
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["max_download_kb_s"], 2048);
        assert_eq!(snap["max_upload_kb_s"], 512);
        assert!(snap["proxy_enabled"].as_bool().unwrap());
        assert!(snap["provider_enabled"].as_bool().unwrap());
        assert!(
            !snap.as_object().unwrap().contains_key("proxy"),
            "快照不得含代理 URL"
        );
    }

    #[test]
    fn provider_xunlei_default_off() {
        // 默认关闭、无 token_path，快照不得暴露路径，仅给布尔。
        let c = Config::default();
        assert!(!c.provider_xunlei.enabled);
        assert!(c.provider_xunlei.token_path.is_none());
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_xunlei_enabled"], false);
        assert_eq!(snap["provider_xunlei_token_exists"], false);
        assert!(
            !snap.as_object().unwrap().contains_key("provider_xunlei_token_path"),
            "快照不得含 token 路径"
        );
    }

    #[test]
    fn provider_xunlei_enabled_with_token_path() {
        let dir = tempfile::tempdir().unwrap();
        // 写一个真实的登录态文件，验证快照能反映其存在性。
        let auth = dir.path().join("xunlei_auth.json");
        std::fs::write(&auth, r#"{"access_token":"a","refresh_token":"r","device_id":"d","captcha_token":"c","user_id":"1","access_token_expires_at":0,"captcha_token_expires_at":0}"#).unwrap();
        let missing = dir.path().join("no_such.json");
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            format!(
                "[provider_xunlei]\nenabled = true\ntoken_path = \"{}\"\n",
                auth.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.provider_xunlei.enabled);
        assert_eq!(c.provider_xunlei.token_path, Some(auth.clone()));
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_xunlei_enabled"], true);
        assert_eq!(snap["provider_xunlei_token_exists"], true);

        // 不存在的 token_path：enabled 仍 true，但 token_exists 为 false。
        let p2 = dir.path().join("config2.toml");
        std::fs::write(
            &p2,
            format!(
                "[provider_xunlei]\nenabled = true\ntoken_path = \"{}\"\n",
                missing.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let c2 = Config::load(Some(&p2)).unwrap();
        assert!(c2.provider_xunlei.enabled);
        let snap2 = c2.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap2["provider_xunlei_enabled"], true);
        assert_eq!(snap2["provider_xunlei_token_exists"], false);
    }

    #[test]
    fn missing_file_uses_default() {
        assert_eq!(Config::load(None).unwrap().server.addr, "127.0.0.1:8787");
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        assert!(
            Config::load(Some(&missing)).is_err(),
            "缺失文件应报错（显式路径）"
        );
    }

    #[test]
    fn partial_toml_keeps_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[server]\naddr = \"127.0.0.1:8080\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.server.addr, "127.0.0.1:8080");
        assert_eq!(
            c.download.dest_root,
            PathBuf::from("./downloads"),
            "未写字段用默认"
        );
    }

    #[test]
    fn bad_toml_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "server = [unclosed").unwrap();
        let err = Config::load(Some(&p)).unwrap_err();
        assert!(err.contains("解析失败"), "应报解析错误: {err}");
    }
}
