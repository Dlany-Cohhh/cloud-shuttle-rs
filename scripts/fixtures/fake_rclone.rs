//! 测试夹具：一个「假 rclone」，用来在没有真实网盘 / 网络的环境里
//! 端到端验证 Rust 控制台的任务编排（预检 → 排队 → 启动 → 进度 → 完成）。
//!
//! 编译：rustc --edition 2021 -O scripts/fixtures/fake_rclone.rs -o <临时目录>/rclone[.exe]
//! 它只认识 Python 版 / Rust 版实际会调用的那些子命令，其它一律安静返回成功。

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    // 跳过开头的 --config <path>（docker 后端时位置相同）
    let mut i = 0usize;
    while i < args.len() {
        if args[i] == "--config" {
            i += 2;
            continue;
        }
        break;
    }
    let cmd = args.get(i).map(|s| s.as_str()).unwrap_or("");
    match cmd {
        "version" => println!("rclone v1.70.0 (fake fixture)"),
        "listremotes" => println!("local:"),
        "config" => match args.get(i + 1).map(|s| s.as_str()).unwrap_or("") {
            "dump" => println!(
                r#"{{"local":{{"type":"local"}},"ghost-old":{{"captcha_token":"fake"}}}}"#
            ),
            _ => {}
        },
        "lsjson" => println!(
            r#"[{{"Name":"demo.mp4","Size":1048576,"IsDir":false,"ModTime":"2026-01-01T00:00:00Z"}}]"#
        ),
        "size" => println!(r#"{{"bytes":1048576,"count":1}}"#),
        "lsd" => println!("           0 2026/01/01 00:00:00                 0 demo"),
        "about" => println!(
            r#"{{"used":1048576,"total":107374182400,"free":107373133824}}"#
        ),
        "copy" => {
            let dest = args.get(i + 2).cloned().unwrap_or_default();
            if !dest.is_empty() {
                let _ = fs::create_dir_all(&dest);
                let p = Path::new(&dest).join("demo.mp4");
                if let Ok(mut f) = fs::File::create(p) {
                    let chunk = vec![0u8; 131072];
                    for _ in 0..12 {
                        let _ = f.write_all(&chunk);
                        let _ = f.flush();
                        thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        }
        _ => {}
    }
}
