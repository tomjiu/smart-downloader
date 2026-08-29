//! smart-dl-daemon 二进制入口：
//! - `smart-dl-daemon serve [--config <path>]`：daemon 服务
//! - 其他命令（add/list/status/...）：客户端模式，连接 serve 的 HTTP API
//!   （`--server <url>`，默认 http://127.0.0.1:8787）

use smart_dl_daemon::serve;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    // —— serve 子命令 ——
    if args.get(1).map(|s| s.as_str()) == Some("serve") {
        let cfg_path = match serve::parse_args(&args[2..]) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("参数错误: {e}");
                eprintln!("用法: smart-dl-daemon serve [--config <path>]");
                std::process::exit(2);
            }
        };
        let cfg = match smart_dl_daemon::config::Config::load(cfg_path.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("配置错误: {e}");
                std::process::exit(2);
            }
        };
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime 创建失败");
        if let Err(e) = rt.block_on(serve::run(cfg, cfg_path)) {
            eprintln!("daemon 退出: {e}");
            std::process::exit(1);
        }
        return;
    }

    // —— 客户端模式：过滤 --server，其余交给 Cli 解析 ——
    let mut server = "http://127.0.0.1:8787".to_string();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--server" {
            let Some(v) = args.get(i + 1) else {
                eprintln!("--server 缺少地址");
                std::process::exit(2);
            };
            server = v.clone();
            i += 2;
            continue;
        }
        rest.push(args[i].clone());
        i += 1;
    }

    let mut full: Vec<String> = vec!["smart-dl".into()];
    full.extend(rest);
    let cli = match smart_dl_daemon::cli::Cli::from_args(&full) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数错误: {e}");
            eprintln!("用法: smart-dl-daemon <add|pause|resume|remove|list|status|logs|import-xunlei> [args] [--server URL] [--json]");
            std::process::exit(2);
        }
    };

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime 创建失败");
    let client = smart_dl_daemon::client::CliClient::new(&server);
    match rt.block_on(client.run(&cli.command, cli.json)) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
