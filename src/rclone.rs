//! rclone 调用层：Windows / Linux 统一入口。
//! local 后端直接调可执行文件；docker 后端通过 docker exec 复用 NAS 上已有的 rclone-backend。

use crate::config::{Backend, Config};
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

pub fn command(cfg: &Config) -> Command {
    // Windows 分支需要 as_std_mut() 设置 CREATE_NO_WINDOW；Linux 分支不需要 mut
    #[allow(unused_mut)]
    let mut cmd = match cfg.backend {
        Backend::Local => {
            let mut c = Command::new(&cfg.rclone_bin);
            c.arg("--config").arg(&cfg.rclone_config);
            c
        }
        Backend::Docker => {
            let mut c = Command::new("docker");
            c.arg("exec")
                .arg(&cfg.docker_container)
                .arg("rclone")
                .arg("--config")
                .arg(&cfg.container_config);
            c
        }
    };
    // Windows 上 spawn rclone 不弹黑框
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.as_std_mut().creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

pub fn with_proxy(mut cmd: Command, proxy: Option<&str>) -> Command {
    if let Some(p) = proxy {
        if !p.is_empty() {
            cmd.env("HTTP_PROXY", p)
                .env("HTTPS_PROXY", p)
                .env("http_proxy", p)
                .env("https_proxy", p);
        }
    }
    cmd
}

pub async fn run(mut cmd: Command, timeout: Duration) -> Result<String, String> {
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| "rclone 调用超时".to_string())?
        .map_err(|e| format!("无法启动 rclone：{}", e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(stderr
            .lines()
            .last()
            .unwrap_or(&stderr)
            .chars()
            .take(180)
            .collect())
    }
}

fn args_cmd(cfg: &Config, args: &[&str], proxy: Option<&str>) -> Command {
    let mut cmd = command(cfg);
    for a in args {
        cmd.arg(a);
    }
    with_proxy(cmd, proxy)
}

pub async fn version(cfg: &Config) -> String {
    match run(args_cmd(cfg, &["version"], None), Duration::from_secs(8)).await {
        Ok(s) => s.lines().next().unwrap_or("rclone").trim().to_string(),
        Err(_) => String::new(),
    }
}

pub async fn listremotes(cfg: &Config, proxy: Option<&str>) -> Result<Vec<String>, String> {
    let s = run(
        args_cmd(cfg, &["listremotes"], proxy),
        Duration::from_secs(30),
    )
    .await?;
    Ok(s.lines()
        .map(|l| l.trim().trim_end_matches(':').to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

pub async fn lsjson(cfg: &Config, src: &str, proxy: Option<&str>) -> Result<Vec<Value>, String> {
    let s = run(
        args_cmd(cfg, &["lsjson", src], proxy),
        Duration::from_secs(60),
    )
    .await?;
    serde_json::from_str::<Vec<Value>>(&s).map_err(|e| format!("解析 lsjson 失败：{}", e))
}

pub async fn size_json(cfg: &Config, src: &str, proxy: Option<&str>) -> Value {
    match run(
        args_cmd(cfg, &["size", "--json", src], proxy),
        Duration::from_secs(45),
    )
    .await
    {
        Ok(s) => {
            serde_json::from_str::<Value>(&s).unwrap_or_else(|_| serde_json::json!({"bytes": 0}))
        }
        Err(_) => serde_json::json!({"bytes": 0}),
    }
}

pub async fn config_dump(cfg: &Config, proxy: Option<&str>) -> Result<Value, String> {
    let s = run(
        args_cmd(cfg, &["config", "dump"], proxy),
        Duration::from_secs(20),
    )
    .await?;
    serde_json::from_str::<Value>(&s).map_err(|e| format!("解析 config dump 失败：{}", e))
}

pub async fn config_call(cfg: &Config, args: &[&str]) -> Result<String, String> {
    run(args_cmd(cfg, args, None), Duration::from_secs(30)).await
}

/// 账号「测试」：列一层根目录，能列出来就认为配置可用。
pub async fn test_remote(cfg: &Config, name: &str, proxy: Option<&str>) -> Result<(), String> {
    let src = format!("{}:", name);
    let src_ref: &str = &src;
    run(
        args_cmd(cfg, &["lsd", "--max-depth", "1", src_ref], proxy),
        Duration::from_secs(25),
    )
    .await
    .map(|_| ())
}

pub async fn about(cfg: &Config, remote: &str, proxy: Option<&str>) -> Value {
    let src = format!("{}:", remote);
    match run(
        args_cmd(cfg, &["about", "--json", &src], proxy),
        Duration::from_secs(12),
    )
    .await
    {
        Ok(s) => serde_json::from_str::<Value>(&s).unwrap_or_else(|_| serde_json::json!({})),
        Err(e) => serde_json::json!({"error": e}),
    }
}

/// 生成一次 rclone copy 的进程（由 api::launch 负责 spawn 与回收）。
pub fn copy_command(
    cfg: &Config,
    src: &str,
    dest: &str,
    settings: &Value,
    proxy: Option<&str>,
) -> Command {
    let s = |k: &str, d: &str| -> String {
        settings
            .get(k)
            .and_then(|v| v.as_str())
            .map(|x| x.to_string())
            .unwrap_or_else(|| d.to_string())
    };
    let n = |k: &str, d: i64| -> String {
        settings
            .get(k)
            .and_then(|v| v.as_i64())
            .unwrap_or(d)
            .to_string()
    };
    let mut args: Vec<String> = vec![
        "copy".into(),
        src.into(),
        dest.into(),
        "--transfers".into(),
        n("transfers", 6),
        "--checkers".into(),
        n("checkers", 8),
        "--retries".into(),
        n("retries", 10),
        "--stats".into(),
        "0".into(),
        "--log-level".into(),
        s("logLevel", "INFO"),
    ];
    if settings
        .get("bwLimitOn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        args.push("--bwlimit".into());
        args.push(s("bwLimit", "5M"));
    }
    if settings
        .get("advancedMode")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        args.push("--multi-thread-streams".into());
        args.push(n("multiThreadStreams", 8));
        args.push("--multi-thread-cutoff".into());
        args.push(format!("{}M", n("multiThreadCutoffMB", 64)));
        args.push("--order-by".into());
        args.push(s("orderBy", "size,descending"));
        args.push("--timeout".into());
        args.push(s("timeout", "1m"));
        args.push("--contimeout".into());
        args.push(s("idleTimeout", "10m"));
    }
    if settings
        .get("createEmptySrcDirs")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
    {
        args.push("--create-empty-src-dirs".into());
    }
    let mut cmd = command(cfg);
    for a in &args {
        cmd.arg(a);
    }
    with_proxy(cmd, proxy)
}

/// 跨平台终止下载子进程。rclone 本身不再派生子进程，直接杀 PID 即可。
pub fn kill(pid: u32) {
    let _ = kill_inner(pid);
}

#[cfg(windows)]
fn kill_inner(pid: u32) -> std::io::Result<()> {
    use std::process::Stdio;
    let mut c = std::process::Command::new("taskkill");
    c.arg("/PID")
        .arg(pid.to_string())
        .arg("/T")
        .arg("/F")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut c = c;
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000);
    c.status().map(|_| ())
}

#[cfg(not(windows))]
fn kill_inner(pid: u32) -> std::io::Result<()> {
    use std::process::Stdio;
    std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
}
