//! 版本信息：Rust 原生版是独立发行线（tag 前缀 rs-v），
//! 不会把 Python 版 Release 误判成自己的升级包。
//!
//! 出站 HTTPS 交给系统自带的 curl（Windows 回退 PowerShell），
//! 这样依赖树保持纯 Rust，交叉编译时不需要 C 工具链 / OpenSSL / cmake。

use crate::config::APP_VERSION;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;

pub fn state() -> Value {
    json!({
        "ok": true,
        "current": APP_VERSION,
        "runtime": "rust",
        "state": {
            "lastCheck": 0,
            "lastApply": 0,
            "error": ""
        }
    })
}

fn hide(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.as_std_mut().creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

async fn curl_fetch(url: &str) -> Result<String, String> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsSL",
        "--max-time",
        "12",
        "-H",
        "User-Agent: cloud-shuttle-rs",
        "-H",
        "Accept: application/vnd.github+json",
        url,
    ]);
    hide(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| "curl 调用超时".to_string())?
        .map_err(|e| format!("curl 不可用：{}", e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(format!("curl 返回 {}", out.status))
    }
}

#[cfg(windows)]
async fn ps_fetch(url: &str) -> Result<String, String> {
    let script = format!(
        "(Invoke-WebRequest -UseBasicParsing -Headers @{{'User-Agent'='cloud-shuttle-rs'}} -Uri '{}').Content",
        url
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    hide(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| "PowerShell 请求超时".to_string())?
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err("PowerShell 请求失败".into())
    }
}

async fn fetch(url: &str) -> Result<String, String> {
    match curl_fetch(url).await {
        Ok(s) => Ok(s),
        Err(e1) => {
            #[cfg(windows)]
            {
                ps_fetch(url).await.map_err(|e2| format!("{}；{}", e1, e2))
            }
            #[cfg(not(windows))]
            {
                Err(format!("{}（检查更新需要系统已安装 curl）", e1))
            }
        }
    }
}

pub async fn check(repo: &str) -> Value {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
    let mut base = json!({
        "ok": false, "current": APP_VERSION, "latest": APP_VERSION,
        "hasUpdate": false, "notes": "", "runtime": "rust",
        "pythonLatest": "", "error": ""
    });
    let data: Value = match fetch(&url).await {
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => v,
            Err(e) => {
                base["error"] = json!(format!("解析 GitHub 响应失败：{}", e));
                return base;
            }
        },
        Err(e) => {
            base["error"] = json!(e);
            return base;
        }
    };
    let tag = data
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    base["ok"] = json!(true);
    base["pythonLatest"] = json!(tag);
    let notes: String = data
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(600)
        .collect();
    base["notes"] = json!(notes);
    // 只认 rs-v* 标签；Python 版 v3.xx 只是并列参考，不触发升级。
    if tag.starts_with("rs-v") {
        base["latest"] = json!(tag);
        base["hasUpdate"] = json!(tag != APP_VERSION);
        if let Some(assets) = data.get("assets").and_then(|v| v.as_array()) {
            base["assets"] = json!(assets
                .iter()
                .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
                .collect::<Vec<_>>());
        }
    } else {
        base["notes"] = json!("原生版暂无独立 Release；Python 容器版可继续使用其在线更新。");
    }
    base
}

pub fn apply_unsupported() -> Value {
    json!({
        "error": "原生版暂不支持在线替换自身：请到 Releases 下载新的可执行文件覆盖后重启进程（数据目录不受影响）",
        "unsupported": true,
        "current": APP_VERSION
    })
}
