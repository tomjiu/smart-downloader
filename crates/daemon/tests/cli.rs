//! M6: CLI 命令集（D26）——8 命令 add/pause/resume/remove/list/status/logs/config
//! + fallback 手动兜底（Q-B9 入口）+ --json 输出。

use smart_dl_daemon::cli::{Cli, CliCommand, CliError};

fn parse(args: &[&str]) -> Result<CliCommand, CliError> {
    let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Cli::parse(&v)
}

#[test]
fn parse_add_http_url() {
    match parse(&["smart-dl", "add", "http://x/file.bin"]).unwrap() {
        CliCommand::Add { url, dest } => {
            assert_eq!(url, "http://x/file.bin");
            assert_eq!(dest, None);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn parse_add_with_dest_and_json() {
    match parse(&["smart-dl", "--json", "add", "http://x/f", "-o", "out/"]).unwrap() {
        CliCommand::Add { url, dest: Some(d) } => {
            assert_eq!(url, "http://x/f");
            assert_eq!(d, "out/");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn parse_lifecycle_commands() {
    assert_eq!(
        parse(&["smart-dl", "pause", "t1"]).unwrap(),
        CliCommand::Pause {
            task_id: "t1".into()
        }
    );
    assert_eq!(
        parse(&["smart-dl", "resume", "t1"]).unwrap(),
        CliCommand::Resume {
            task_id: "t1".into()
        }
    );
    assert_eq!(
        parse(&["smart-dl", "remove", "t1"]).unwrap(),
        CliCommand::Remove {
            task_id: "t1".into()
        }
    );
    assert_eq!(
        parse(&["smart-dl", "status", "t1"]).unwrap(),
        CliCommand::Status {
            task_id: "t1".into()
        }
    );
}

#[test]
fn parse_list_logs_config() {
    assert_eq!(parse(&["smart-dl", "list"]).unwrap(), CliCommand::List);
    assert_eq!(
        parse(&["smart-dl", "logs", "t1"]).unwrap(),
        CliCommand::Logs {
            task_id: "t1".into()
        }
    );
    assert_eq!(parse(&["smart-dl", "config"]).unwrap(), CliCommand::Config);
}

#[test]
fn parse_fallback_manual_command() {
    // Q-B9 手动兜底入口：fallback <task_id>
    match parse(&["smart-dl", "fallback", "t9"]).unwrap() {
        CliCommand::Fallback { task_id } => assert_eq!(task_id, "t9"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn json_flag_exposed_on_cli() {
    let args = |a: &[&str]| {
        let v: Vec<String> = a.iter().map(|s| s.to_string()).collect();
        Cli::from_args(&v).unwrap()
    };
    assert!(
        args(&["smart-dl", "--json", "list"]).json,
        "--json 全局标志"
    );
    assert!(!args(&["smart-dl", "list"]).json);
}

#[test]
fn unknown_command_rejected() {
    assert!(matches!(
        parse(&["smart-dl", "explode"]),
        Err(CliError::Unknown(_))
    ));
}

#[test]
fn missing_argument_rejected() {
    assert!(matches!(
        parse(&["smart-dl", "pause"]),
        Err(CliError::MissingArg(_))
    ));
}

// —— 执行层集成：真实 serve + CLI 客户端往返 ——

mod common;

use smart_dl_daemon::client::CliClient;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

#[tokio::test]
async fn cli_add_list_status_roundtrip() {
    // dest 垃圾治理：default_dest_root 缺省 = "."（进程工作目录 = crates/daemon/），
    // dest: None 的 add 会把下载文件留在仓库内（crates/daemon/file）。注入独立
    // tempdir 为白名单根 + 默认落盘目录，TempDir 守卫在测试结束时自动清理。
    let dir = tempfile::tempdir().unwrap();
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![]).with_dest_root(dir.path().to_path_buf()),
    );
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    let client = CliClient::new(&base, None);

    let srv = common::TestServer::start(common::patterned(1024)).await;
    let url = srv.url();

    // add → 成功（默认 dest）
    client
        .run(
            &CliCommand::Add {
                url: url.clone(),
                dest: None,
            },
            false,
        )
        .await
        .unwrap();

    // 重复 add 同 URL → 409 → Err（错误信息含 duplicate）
    let dup = client
        .run(
            &CliCommand::Add {
                url: url.clone(),
                dest: None,
            },
            false,
        )
        .await;
    assert!(dup.is_err(), "重复 add 应报错 409: {dup:?}");
    assert!(dup.unwrap_err().to_string().contains("duplicate"));

    // list → 任务 t1 在列
    client.run(&CliCommand::List, false).await.unwrap();

    // status t1 → 字段齐全
    client
        .run(
            &CliCommand::Status {
                task_id: "t1".into(),
            },
            false,
        )
        .await
        .unwrap();

    // remove → 清除
    client
        .run(
            &CliCommand::Remove {
                task_id: "t1".into(),
            },
            false,
        )
        .await
        .unwrap();
    // 删除后再查 → 404 → Err
    let gone = client
        .run(
            &CliCommand::Status {
                task_id: "t1".into(),
            },
            false,
        )
        .await;
    assert!(gone.is_err(), "删除后 status 应 404: {gone:?}");
}

#[tokio::test]
async fn cli_json_output_success() {
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(DaemonState::new(Arc::new(engine), vec![]));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    let client = CliClient::new(&base, None);

    // 空列表 --json 输出
    client.run(&CliCommand::List, true).await.unwrap();

    // D37 端点补齐：config 命令 → GET /config（v1 有端点，成功返回配置快照）
    client.run(&CliCommand::Config, false).await.unwrap();
    client.run(&CliCommand::Config, true).await.unwrap();
}

// ===== 安全回归（V1 配套）：CLI --token 与 daemon auth_mw 配对 =====

#[tokio::test]
async fn cli_token_roundtrip_against_secured_daemon() {
    // token 配置的 daemon + 携带正确 token 的 CliClient → 全命令可用；
    // 错 token → 401 → CliError。
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_http_token(Some("cli-e2e-token".into())),
    );
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");

    // 无 token → 401
    let anon = CliClient::new(&base, None);
    let r = anon.run(&CliCommand::List, false).await;
    assert!(r.is_err(), "无 token 访问受保护 daemon 应失败: {r:?}");

    // 错 token → 401
    let wrong = CliClient::new(&base, Some("wrong"));
    assert!(wrong.run(&CliCommand::List, false).await.is_err());

    // 正确 token → list/config 正常
    let ok = CliClient::new(&base, Some("cli-e2e-token"));
    ok.run(&CliCommand::List, false).await.unwrap();
    ok.run(&CliCommand::Config, false).await.unwrap();
}
