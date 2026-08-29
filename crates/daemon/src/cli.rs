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
    /// 迅雷任务导入（M9）：xlbt.cfg + 一组 .bt.xltd + .torrent → fastresume。
    #[cfg(feature = "xunlei-import")]
    ImportXunlei {
        torrent: String,
        cfg: String,
        xltds: Vec<String>,
        dest: Option<String>,
    },
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
        other => Err(CliError::Unknown(other.to_string())),
    }
}
