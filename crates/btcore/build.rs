// crates/btcore/build.rs — bindgen 从 ffi/lt.h 生成绑定 + 链接 lt_kernel 静态库。
// 契约流（D14/F0）：lt.h（手写，契约定死）→ lt_kernel.cpp 实现 → bindgen 生成 Rust 声明。
// 链接：
//   LT_KERNEL_LIB_DIR — lt_kernel.lib 所在目录（默认 ../../ffi/build，Release 产物在
//                       ffi/build/Release，构建脚本按存在性探测）。
//   LT_VCPKG_LIB_DIR  — vcpkg 安装的 .lib 目录（默认 ../../ffi/vcpkg_installed/x64-windows/lib），
//                       逐个以 dylib=<stem> 链接（libtorrent 为动态库，lt_kernel 引用其
//                       dllimport 符号；boost/openssl 等传递依赖一并带上）。

use std::env;
use std::path::{Path, PathBuf};

/// 预检测 libclang 是否可用（bindgen 0.71 缺库时 panic，须先行探测）。
/// 探测顺序：LIBCLANG_PATH 环境变量 → 常见系统路径（含 llvm-* 版本目录）。
fn libclang_available() -> bool {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = env::var("LIBCLANG_PATH") {
        candidates.push(PathBuf::from(p));
    }
    for base in ["/usr/lib", "/usr/local/lib"] {
        candidates.push(PathBuf::from(base).join("x86_64-linux-gnu"));
        candidates.push(PathBuf::from(base));
        for ver in 10..=21 {
            candidates.push(PathBuf::from(format!("{base}/llvm-{ver}/lib")));
        }
    }
    for dir in candidates {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("libclang") && name.contains(".so") {
                    return true;
                }
            }
        }
        // 直接命中文件（LIBCLANG_PATH 可能指向文件本身）
        if dir.is_file() {
            return true;
        }
    }
    false
}

/// 剥离 bindgen 产物中的编译期布局断言块（`const _: () = [...]` 或
/// `const _: () = { ... };` 含 "Size of / Alignment of / Offset of field" 标记的整段）。
/// 这些断言依赖生成平台的类型布局，跨平台 check 时会 const 求值越界。
fn strip_layout_assertions(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let rest_src = src;
    let mut i = 0;
    while i < rest_src.len() {
        if rest_src[i..].starts_with("const _: ()") {
            let rest = &rest_src[i..];
            // 求值表达式起点
            let eq = match rest.find('=') {
                Some(e) => e,
                None => {
                    out.push_str(rest);
                    break;
                }
            };
            let after_eq = &rest[eq + 1..];
            let trimmed = after_eq.trim_start();
            let (end, drop_block) = if trimmed.starts_with('{') {
                // 块表达式：按括号深度找配对 '}'，再吃掉紧随的 ';'
                let offset_in_rest = after_eq.len() - trimmed.len();
                let block_start = eq + 1 + offset_in_rest;
                let mut depth = 0i64;
                let mut found = None;
                for (k, c) in rest[block_start..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                found = Some(block_start + k);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                match found {
                    Some(brace_end) => {
                        let semi = rest[brace_end..].find(';').map(|s| brace_end + s);
                        match semi {
                            Some(s) => (
                                s,
                                rest[block_start..=brace_end].contains("Size of ")
                                    || rest[block_start..=brace_end].contains("Alignment of ")
                                    || rest[block_start..=brace_end].contains("Offset of field:"),
                            ),
                            None => (rest.len() - 1, false),
                        }
                    }
                    None => (rest.len() - 1, false),
                }
            } else {
                // 表达式形式：到第一个 ';'
                match rest.find(';') {
                    Some(s) => (
                        s,
                        rest[..=s].contains("Size of ")
                            || rest[..=s].contains("Alignment of ")
                            || rest[..=s].contains("Offset of field:"),
                    ),
                    None => {
                        out.push_str(rest);
                        break;
                    }
                }
            };
            if drop_block {
                i += end + 1;
                // 吃掉块后换行
                while i < rest_src.len()
                    && (rest_src.as_bytes()[i] == b'\n' || rest_src.as_bytes()[i] == b'\r')
                {
                    i += 1;
                }
                continue;
            }
            out.push_str(&rest[..=end]);
            i += end + 1;
            continue;
        }
        let ch = rest_src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let repo = PathBuf::from(&manifest).join("..").join("..");

    let lt_h = repo.join("ffi").join("lt.h");
    // 绑定策略（跨平台确定性优先）：默认一律使用仓库内已提交的 bindings.rs
    //（净化副本写入 OUT_DIR，见下）。理由：
    //  ① 仓库内绑定是唯一被全量测试验证过的形状（Windows 生成、Linux 净化后
    //     实测 33+162 全绿）；
    //  ② 现场 bindgen 生成的 Linux 绑定与提交版存在常量类型差异（如 LT_ALERT_*
    //     宏 u32/i32），会使按提交绑定形状编写的 ffi.rs 编译失败
    //     （CI bt job 首跑实证：runner 带 libclang → 全量 E0308）；
    //  ③ bindgen 0.71 在缺 libclang 时 panic 而非返回 Err，隐式探测路径脆弱。
    // 需要重新生成绑定时显式开启（需 libclang；产物回存 crates/btcore/bindings.rs）：
    //   SMART_DL_REGEN_BINDINGS=1 cargo build -p smart-dl-btcore
    let fallback_bindings = PathBuf::from(&manifest).join("bindings.rs");
    let regen_requested = env::var("SMART_DL_REGEN_BINDINGS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let libclang_ok = libclang_available();
    let use_fallback = fallback_bindings.exists() && !(regen_requested && libclang_ok);
    if regen_requested && !libclang_ok {
        println!(
            "cargo:warning=SMART_DL_REGEN_BINDINGS=1 但 libclang 不可用，回退使用已提交的 bindings.rs"
        );
    }
    // 声明自定义 cfg 名（cargo 1.80+ unexpected_cfgs 检查要求先声明再使用；
    // 与分支无关 —— ffi.rs 在两种路径下都引用该 cfg）
    println!("cargo::rustc-check-cfg=cfg(lt_bindings_fallback)");
    if use_fallback {
        // 净化回退产物：Windows/MSVC 生成的 bindings.rs 含平台相关的布局断言
        //（"Size of/Alignment of/Offset of field"，如 _Mbstatet），在 Linux 上
        // const 求值会越界 panic。剥离这些编译期检查（不影响运行语义），
        // 写入 OUT_DIR 并用 rustc-cfg 让 ffi.rs 切换 include。
        let raw =
            std::fs::read_to_string(&fallback_bindings).expect("read fallback bindings.rs failed");
        let sanitized = strip_layout_assertions(&raw);
        let out_dir = env::var("OUT_DIR").unwrap();
        let out_path = Path::new(&out_dir).join("bindings_fallback.rs");
        std::fs::write(&out_path, sanitized).expect("write bindings_fallback.rs failed");
        println!("cargo:rustc-cfg=lt_bindings_fallback");
    } else {
        match bindgen::Builder::default()
            .header(lt_h.to_str().unwrap())
            .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
            .generate_comments(false)
            .generate()
        {
            Ok(bindings) => {
                bindings
                    .write_to_file(&fallback_bindings)
                    .expect("write bindings.rs failed");
            }
            Err(e) => {
                if fallback_bindings.exists() {
                    println!("cargo:warning=bindgen 失败（{e}），保留已提交的 bindings.rs");
                } else {
                    panic!("bindgen failed on ffi/lt.h: {e}，且无回退 bindings.rs");
                }
            }
        }
    }
    println!("cargo:rerun-if-changed={}", lt_h.display());

    // lt_kernel.lib：默认 ffi/build，优先 Release/debug 探测
    let lib_dir = env::var("LT_KERNEL_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let cand = repo.join("ffi").join("build");
            for sub in ["Release", "Debug", ""] {
                let p = cand.join(sub);
                if p.join("lt_kernel.lib").exists() {
                    return p;
                }
            }
            cand
        });
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=lt_kernel");

    // vcpkg 动态库导入库：逐个链接（与 ffi cmake 的 LibtorrentRasterbar 传递依赖一致）
    let vcpkg_lib = env::var("LT_VCPKG_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            repo.join("ffi")
                .join("vcpkg_installed")
                .join("x64-windows")
                .join("lib")
        });
    println!("cargo:rustc-link-search=native={}", vcpkg_lib.display());
    if let Ok(entries) = std::fs::read_dir(&vcpkg_lib) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "lib").unwrap_or(false))
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        for stem in names {
            println!("cargo:rustc-link-lib=dylib={}", stem);
        }
    }
    println!("cargo:rerun-if-changed={}", vcpkg_lib.display());
}
