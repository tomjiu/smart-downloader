//! CLI 命令集（D26）：add/pause/resume/remove/list/status/logs/config
//! + fallback <task_id>（Q-B9 手动兜底入口）+ --json 全局输出标志。

/// 解析结果（--json 标志 + 命令）。
#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    pub json: bool,
    pub command: CliCommand,
}

/// 命令。
#[derive(Clone, Debug, PartialEq)]
pub enum CliCommand {
    Add {
        url: String,
        dest: Option<String>,
    },
    Pause {
        task_id: String,
    },
    Resume {
        task_id: String,
    },
    Remove {
        task_id: String,
    },
    List,
    Status {
        task_id: String,
    },
    Logs {
        task_id: String,
    },
    Config,
    /// Q-B9 手动兜底（metadata 超时标志后的人工入口）。
    Fallback {
        task_id: String,
    },
    /// 迅雷原生登录（Task 5-b）：--page 本地 App 同款登录页（默认）/ --browser
    /// 跳转官方授权页 / --qr 终端二维码。本地执行，不需要 daemon 在跑。
    /// `--tier <web|nas>`（P1-1）：身份档位，登录态按档分文件（防互踢）。
    XunleiLogin {
        mode: XunleiLoginMode,
        token_path: Option<String>,
        port: u16,
        tier: Option<String>,
    },
    /// 迅雷任务导入（M9）：xlbt.cfg + 一组 .bt.xltd + .torrent → fastresume。
    #[cfg(feature = "xunlei-import")]
    ImportXunlei {
        torrent: String,
        cfg: String,
        xltds: Vec<String>,
        dest: Option<String>,
    },
    /// 百度分享免登录解析（B3-a）：verify → BDCLND → 分享页 meta →
    /// share/list 文件清单。本地执行，不需要 daemon 在跑；dlink 直链
    /// 转换待 B3-b（需登录态 BDUSS）。
    BaiduResolve {
        url: String,
        /// 提取码（URL 未带 ?pwd= 时显式提供）。
        pwd: Option<String>,
        /// 列子目录（缺省列根目录）。
        dir: Option<String>,
    },
}

/// 迅雷登录模式（Task 5-b）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XunleiLoginMode {
    /// 本地渲染 App 同款登录页（默认）。
    Page,
    /// 系统浏览器跳转官方授权页。
    Browser,
    /// 终端二维码。
    Qr,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("unknown command: {0}")]
    Unknown(String),
    #[error("missing argument for {0}")]
    MissingArg(String),
    #[error("http: {0}")]
    Http(String),
}

impl Cli {
    /// 从完整 argv 解析（含程序名；过滤 --json 全局标志）。
    pub fn from_args(args: &[String]) -> Result<Cli, CliError> {
        let mut json = false;
        let mut rest: Vec<String> = Vec::new();
        for a in args {
            if a == "--json" {
                json = true;
            } else {
                rest.push(a.clone());
            }
        }
        let command = parse_command(&rest[1..])?;
        Ok(Cli { json, command })
    }

    /// 解析为单个命令（测试友好：直接传命令参数）。
    pub fn parse(args: &[String]) -> Result<CliCommand, CliError> {
        Ok(Cli::from_args(args)?.command)
    }
}

fn parse_command(args: &[String]) -> Result<CliCommand, CliError> {
    let cmd = args
        .first()
        .ok_or_else(|| CliError::MissingArg("command".to_string()))?;
    match cmd.as_str() {
        "add" => {
            let url = args
                .get(1)
                .ok_or_else(|| CliError::MissingArg("add <url>".to_string()))?
                .clone();
            let dest = match args.get(2).map(|s| s.as_str()) {
                Some("-o") => Some(
                    args.get(3)
                        .ok_or_else(|| CliError::MissingArg("-o <dir>".to_string()))?
                        .clone(),
                ),
                _ => None,
            };
            Ok(CliCommand::Add { url, dest })
        }
        "pause" | "resume" | "remove" | "status" | "logs" | "fallback" => {
            let task_id = args
                .get(1)
                .ok_or_else(|| CliError::MissingArg(format!("{cmd} <task_id>")))?;
            Ok(match cmd.as_str() {
                "pause" => CliCommand::Pause {
                    task_id: task_id.clone(),
                },
                "resume" => CliCommand::Resume {
                    task_id: task_id.clone(),
                },
                "remove" => CliCommand::Remove {
                    task_id: task_id.clone(),
                },
                "status" => CliCommand::Status {
                    task_id: task_id.clone(),
                },
                "logs" => CliCommand::Logs {
                    task_id: task_id.clone(),
                },
                "fallback" => CliCommand::Fallback {
                    task_id: task_id.clone(),
                },
                _ => unreachable!(),
            })
        }
        #[cfg(feature = "xunlei-import")]
        "import-xunlei" => {
            let torrent = args
                .get(1)
                .ok_or_else(|| CliError::MissingArg("import-xunlei <torrent>".to_string()))?
                .clone();
            let cfg = args
                .get(2)
                .ok_or_else(|| CliError::MissingArg("import-xunlei <torrent> <cfg>".to_string()))?
                .clone();
            // 收集 xltd 路径（至少 1 个），直到遇到 -o
            let mut xltds = Vec::new();
            let mut dest = None;
            let mut i = 3;
            while i < args.len() {
                if args[i] == "-o" {
                    dest = Some(
                        args.get(i + 1)
                            .ok_or_else(|| CliError::MissingArg("-o <dir>".to_string()))?
                            .clone(),
                    );
                    break;
                }
                xltds.push(args[i].clone());
                i += 1;
            }
            if xltds.is_empty() {
                return Err(CliError::MissingArg(
                    "import-xunlei <torrent> <cfg> <xltd> [<xltd2> ...]".to_string(),
                ));
            }
            Ok(CliCommand::ImportXunlei {
                torrent,
                cfg,
                xltds,
                dest,
            })
        }
        #[cfg(not(feature = "xunlei-import"))]
        "import-xunlei" => Err(CliError::Unknown(
            "import-xunlei 需要编译时启用 --features xunlei-import".to_string(),
        )),
        "list" => Ok(CliCommand::List),
        "config" => Ok(CliCommand::Config),
        "baidu-resolve" => {
            let url = args
                .get(1)
                .ok_or_else(|| CliError::MissingArg("baidu-resolve <url>".to_string()))?
                .clone();
            let mut pwd = None;
            let mut dir = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--pwd" => {
                        pwd = Some(
                            args.get(i + 1)
                                .ok_or_else(|| CliError::MissingArg("--pwd <提取码>".to_string()))?
                                .clone(),
                        );
                        i += 1;
                    }
                    "--dir" => {
                        dir = Some(
                            args.get(i + 1)
                                .ok_or_else(|| CliError::MissingArg("--dir <路径>".to_string()))?
                                .clone(),
                        );
                        i += 1;
                    }
                    other => return Err(CliError::Unknown(format!("baidu-resolve {other}"))),
                }
                i += 1;
            }
            Ok(CliCommand::BaiduResolve { url, pwd, dir })
        }
        "xunlei-login" => {
            let mut mode = XunleiLoginMode::Page;
            let mut token_path = None;
            let mut port = 0u16;
            let mut tier = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--browser" => mode = XunleiLoginMode::Browser,
                    "--page" => mode = XunleiLoginMode::Page,
                    "--qr" => mode = XunleiLoginMode::Qr,
                    "--tier" => {
                        tier = Some(
                            args.get(i + 1)
                                .ok_or_else(|| {
                                    CliError::MissingArg("--tier <web|nas>".to_string())
                                })?
                                .clone(),
                        );
                        i += 1;
                    }
                    "--token" => {
                        token_path = Some(
                            args.get(i + 1)
                                .ok_or_else(|| CliError::MissingArg("--token <path>".to_string()))?
                                .clone(),
                        );
                        i += 1;
                    }
                    "--port" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| CliError::MissingArg("--port <n>".to_string()))?;
                        port = v
                            .parse()
                            .map_err(|_| CliError::Unknown(format!("--port 非法端口号: {v}")))?;
                        i += 1;
                    }
                    other => return Err(CliError::Unknown(format!("xunlei-login {other}"))),
                }
                i += 1;
            }
            Ok(CliCommand::XunleiLogin {
                mode,
                token_path,
                port,
                tier,
            })
        }
        other => Err(CliError::Unknown(other.to_string())),
    }
}
