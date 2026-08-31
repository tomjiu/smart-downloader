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

    // —— 客户端模式：过滤 --server / --token，其余交给 Cli 解析 ——
    let mut server = "http://127.0.0.1:8787".to_string();
    // 安全修复（V1 配套）：token 优先级 CLI --token > 环境变量 SMART_DL_HTTP_TOKEN
    //（与 serve 同 env，用户配置一次两边生效）；均无 = None（回环未配置模式）。
    let mut token: Option<String> = std::env::var("SMART_DL_HTTP_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
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
        if args[i] == "--token" {
            let Some(v) = args.get(i + 1) else {
                eprintln!("--token 缺少值");
                std::process::exit(2);
            };
            token = Some(v.clone()).filter(|t| !t.is_empty());
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
            eprintln!("用法: smart-dl-daemon <add|pause|resume|remove|list|status|logs|fallback|xunlei-login|import-xunlei> [args] [--server URL] [--token TOKEN] [--json]");
            std::process::exit(2);
        }
    };

    // —— xunlei-login：本地执行（无需 daemon 进程），分发前拦截 ——
    if let smart_dl_daemon::cli::CliCommand::XunleiLogin { mode, token_path, port, tier } = &cli.command {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime 创建失败");
        if let Err(e) = rt.block_on(smart_dl_daemon::xunlei_login::run(*mode, token_path.clone(), *port, tier.clone())) {
            eprintln!("xunlei-login 失败: {e}");
            std::process::exit(1);
        }
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime 创建失败");
    let client = smart_dl_daemon::client::CliClient::new(&server, token.as_deref());
    match rt.block_on(client.run(&cli.command, cli.json)) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
