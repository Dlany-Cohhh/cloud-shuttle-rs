//! 云梭 CloudShuttle · Rust 原生应用进程
//! 一个可执行文件同时服务 Windows 与 Linux：内建 HTTP 控制台 + rclone 下载编排。

mod api;
mod app;
mod config;
mod localfs;
mod rclone;
mod store;
mod update;
mod web;

use app::{log, AppState};
use config::{help_text, Config, APP_VERSION, DEFAULT_UPDATE_REPO};
use std::net::SocketAddr;
use std::time::Duration;

fn self_check(cfg: &Config) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let idx = cfg.static_dir.join("index.html");
    out.push((
        idx.is_file(),
        format!(
            "前端资源：{}",
            if idx.is_file() {
                "已找到"
            } else {
                "缺失（页面无法打开）"
            }
        ),
    ));
    out.push((
        cfg.data_dir.exists(),
        format!("数据目录：{}", cfg.data_dir.display()),
    ));
    out.push((
        cfg.download_root.exists(),
        format!("下载根目录：{}", cfg.download_root.display()),
    ));
    match cfg.backend {
        config::Backend::Local => out.push((
            cfg.rclone_config.exists(),
            format!("rclone 配置：{}", cfg.rclone_config.display()),
        )),
        config::Backend::Docker => out.push((
            true,
            format!(
                "docker 容器：{}（配置 {}）",
                cfg.docker_container,
                cfg.container_config.display()
            ),
        )),
    }
    out.push((
        true,
        format!(
            "rclone 调用方式：{}    配置参数：{}    目标根：{}",
            cfg.backend.name(),
            cfg.rclone_config_arg(),
            cfg.dest_root().display()
        ),
    ));
    out
}

/// 上一次进程遗留的 running 任务没有子进程了，标记为已暂停。
fn recover(st: &AppState) {
    let mut store = st.store.lock().unwrap();
    let mut n = 0;
    if let Some(arr) = store.jobs.as_array_mut() {
        for j in arr.iter_mut() {
            if j.get("state").and_then(|v| v.as_str()) == Some("running") {
                j["state"] = serde_json::json!("stopped");
                j["error"] = serde_json::json!("进程重启后任务已暂停，点「继续」可续传");
                j["speed"] = serde_json::json!(0);
                j["pid"] = serde_json::json!(0);
                n += 1;
            }
        }
    }
    if n > 0 {
        store.save_jobs();
        log(&format!(
            "[recover] {} 个中断任务已标记为暂停（rclone 支持断点续传）",
            n
        ));
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    log("[shutdown] 收到停止信号，正在退出（运行中的 rclone 子任务会保留在任务列表里）");
}

#[tokio::main]
async fn main() {
    let (cfg, cli) = Config::from_args();
    if cli.help {
        println!("{}", help_text());
        return;
    }
    if cli.version {
        println!(
            "cloud-shuttle-rs {} ({}/{})",
            APP_VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return;
    }
    if let Err(e) = cfg.ensure_dirs() {
        eprintln!("无法创建数据目录：{}", e);
        std::process::exit(1);
    }

    let st = AppState::new(cfg.clone());
    let rver = rclone::version(&cfg).await;
    let rclone_ok = !rver.is_empty();
    *st.rclone_version.lock().unwrap() = if rclone_ok {
        rver.clone()
    } else {
        "未检测到 rclone".into()
    };

    log("────────────────────────────────────────────────");
    log(&format!(
        " 云梭 CloudShuttle · Rust 原生应用进程 {}",
        APP_VERSION
    ));
    log(&format!(
        " 平台：{}/{}    rclone：{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        if rclone_ok { &rver } else { "未检测到" }
    ));
    for (ok, line) in self_check(&cfg) {
        log(&format!(" [{}] {}", if ok { "OK" } else { "!!" }, line));
    }
    log(&format!(" 控制台：{}", cfg.url()));
    log(&format!(
        " 数据源：{}    更新源：{}",
        cfg.update_repo, DEFAULT_UPDATE_REPO
    ));
    log(" 停止：Ctrl+C（Linux 下 systemd 发 SIGTERM 亦可）");
    log("────────────────────────────────────────────────");
    if cli.check {
        return;
    }
    if !rclone_ok {
        log("[warn] rclone 不可用：账号 / 云盘列表 / 下载会报错，但本地文件页与设置仍可使用。");
    }

    recover(&st);
    let bg = st.clone();
    tokio::spawn(async move {
        // 先跑一轮，把重启时排队的任务拉起来
        api::tick(&bg).await;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            api::tick(&bg).await;
        }
    });

    let addr: SocketAddr = match format!("{}:{}", cfg.bind, cfg.port).parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("监听地址无效：{}", e);
            std::process::exit(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("端口 {} 绑定失败：{}（可用 --port 换端口）", cfg.port, e);
            std::process::exit(1);
        }
    };
    log(&format!(
        "[ready] 已监听 {}，浏览器打开 {}",
        addr,
        cfg.url()
    ));
    if let Err(e) = axum::serve(listener, web::router(st))
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("服务异常：{}", e);
        std::process::exit(1);
    }
}
