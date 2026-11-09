//! 跨平台运行配置。
//!
//! 所有路径都可用环境变量或命令行参数覆盖；Windows 与 Linux 共用同一套代码，
//! 只在「rclone 的调用方式」与「子进程窗口行为」上有条件编译分支。

use std::env;
use std::path::{Path, PathBuf};

/// 与 GitHub Release 的 tag 保持一致（`rs-v*` 系列），避免 updatecheck 误报升级。
pub const APP_VERSION: &str = "rs-v0.1.0";
pub const DEFAULT_PORT: u16 = 5566;
pub const DEFAULT_PROXY: &str = "http://127.0.0.1:7890";
pub const DEFAULT_UPDATE_REPO: &str = "Dlany-Cohhh/cloud-shuttle-rs";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// 直接调用宿主机上的 rclone 可执行文件（Windows 默认）
    Local,
    /// 通过 docker exec 调用 rclone-backend 容器里的 rclone（NAS 上复用现有实例）
    Docker,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Local => "local",
            Backend::Docker => "docker",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub download_root: PathBuf,
    pub static_dir: PathBuf,
    pub rclone_bin: String,
    pub rclone_config: PathBuf,
    pub backend: Backend,
    pub docker_container: String,
    /// docker 后端时，容器内看到的 rclone.conf 路径
    pub container_config: PathBuf,
    /// docker 后端时，容器内看到的下载根目录（宿主机目录挂载到这里的对应路径）
    pub container_download_root: PathBuf,
    /// 部署时写入的默认代理（设置页 proxyUrl 优先级更高）
    pub base_proxy: String,
    pub public_host: String,
    pub update_repo: String,
}

#[derive(Default, Debug)]
pub struct Cli {
    pub check: bool,
    pub version: bool,
    pub help: bool,
}

fn env_s(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 可选：从 exe 同目录或当前目录读取 .env（已存在的环境变量优先）。
pub fn load_dotenv() {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for p in [exe_dir().join(".env"), cwd.join(".env")] {
        if let Ok(txt) = std::fs::read_to_string(&p) {
            for line in txt.lines() {
                let l = line.trim();
                if l.is_empty() || l.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = l.split_once('=') {
                    let k = k.trim();
                    let v = v.trim().trim_matches('"').trim_matches('\'');
                    if !k.is_empty() && env::var(k).is_err() {
                        env::set_var(k, v);
                    }
                }
            }
            break;
        }
    }
}

pub fn exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn detect_backend(container: &str) -> Backend {
    // Windows 上 Docker Desktop 一般是给 Linux 容器用的，默认走本机 rclone.exe。
    if cfg!(windows) {
        return Backend::Local;
    }
    match std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .output()
    {
        Ok(o) if o.status.success() => {
            let names = String::from_utf8_lossy(&o.stdout);
            if names.lines().any(|l| l.trim() == container) {
                Backend::Docker
            } else {
                Backend::Local
            }
        }
        _ => Backend::Local,
    }
}

impl Config {
    /// 环境变量 + 命令行参数合并。命令行优先级最高。
    pub fn from_args() -> (Config, Cli) {
        load_dotenv();
        let exe = exe_dir();
        let data_dir = env_s("CS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| exe.join("data"));
        let download_root = env_s("CS_DOWNLOAD_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("downloads"));
        let static_dir = env_s("CS_STATIC")
            .or_else(|| env_s("CS_DEV_STATIC"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let beside = exe.join("static");
                if beside.is_dir() {
                    beside
                } else {
                    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    cwd.join("static")
                }
            });
        let docker_container =
            env_s("CS_DOCKER_CONTAINER").unwrap_or_else(|| "rclone-backend".to_string());
        let backend_mode = env_s("CS_RCLONE_BACKEND").unwrap_or_else(|| "auto".to_string());

        let mut c = Config {
            bind: env_s("CS_BIND").unwrap_or_else(|| "127.0.0.1".to_string()),
            port: env_s("CS_PORT")
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(DEFAULT_PORT),
            rclone_config: env_s("CS_RCLONE_CONFIG")
                .or_else(|| env_s("RCLONE_CONFIG"))
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("rclone.conf")),
            data_dir,
            download_root,
            static_dir,
            rclone_bin: env_s("CS_RCLONE_BIN").unwrap_or_else(|| {
                if cfg!(windows) {
                    "rclone.exe"
                } else {
                    "rclone"
                }
                .to_string()
            }),
            backend: Backend::Local,
            docker_container,
            container_config: env_s("CS_CONTAINER_RCLONE_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/config/rclone/rclone.conf")),
            container_download_root: env_s("CS_CONTAINER_DOWNLOAD_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/data")),
            base_proxy: env_s("PROXY_URL").unwrap_or_else(|| DEFAULT_PROXY.to_string()),
            public_host: env_s("CONSOLE_PUBLIC_HOST")
                .unwrap_or_else(|| format!("localhost:{}", DEFAULT_PORT)),
            update_repo: env_s("UPDATE_REPO").unwrap_or_else(|| DEFAULT_UPDATE_REPO.to_string()),
        };

        let rc_explicit = env_s("CS_RCLONE_CONFIG").is_some() || env_s("RCLONE_CONFIG").is_some();
        let dl_explicit = env_s("CS_DOWNLOAD_ROOT").is_some();
        let mut cli_rc = false;
        let mut cli_dl = false;
        let mut cli = Cli::default();
        let mut cli_backend: Option<String> = None;
        let args: Vec<String> = env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            let next = || args.get(i + 1).cloned();
            match a {
                "--port" => {
                    if let Some(v) = next().and_then(|s| s.parse::<u16>().ok()) {
                        c.port = v;
                    }
                    i += 1;
                }
                "--bind" => {
                    if let Some(v) = next() {
                        c.bind = v;
                    }
                    i += 1;
                }
                "--data-dir" => {
                    if let Some(v) = next() {
                        c.data_dir = PathBuf::from(v);
                    }
                    i += 1;
                }
                "--download-root" => {
                    if let Some(v) = next() {
                        c.download_root = PathBuf::from(v);
                        cli_dl = true;
                    }
                    i += 1;
                }
                "--static-dir" => {
                    if let Some(v) = next() {
                        c.static_dir = PathBuf::from(v);
                    }
                    i += 1;
                }
                "--rclone-bin" => {
                    if let Some(v) = next() {
                        c.rclone_bin = v;
                    }
                    i += 1;
                }
                "--rclone-config" => {
                    if let Some(v) = next() {
                        c.rclone_config = PathBuf::from(v);
                        cli_rc = true;
                    }
                    i += 1;
                }
                "--backend" => {
                    if let Some(v) = next() {
                        cli_backend = Some(v);
                    }
                    i += 1;
                }
                "--container" => {
                    if let Some(v) = next() {
                        c.docker_container = v;
                    }
                    i += 1;
                }
                "--proxy" => {
                    if let Some(v) = next() {
                        c.base_proxy = v;
                    }
                    i += 1;
                }
                "--check" => cli.check = true,
                "--version" | "-V" => cli.version = true,
                "--help" | "-h" => cli.help = true,
                _ => {}
            }
            i += 1;
        }

        // 派生路径要跟随最终的 data_dir，否则 --data-dir 会让 rclone.conf 落在旧目录里
        if !cli_rc && !rc_explicit {
            c.rclone_config = c.data_dir.join("rclone.conf");
        }
        if !cli_dl && !dl_explicit {
            c.download_root = c.data_dir.join("downloads");
        }
        // 命令行 > 环境变量 > 自动探测
        let mode = cli_backend.unwrap_or(backend_mode);
        c.backend = match mode.as_str() {
            "docker" => Backend::Docker,
            "local" => Backend::Local,
            _ => detect_backend(&c.docker_container),
        };
        (c, cli)
    }

    /// 容器内可见的配置路径 / 下载根路径；local 后端时与宿主机一致。
    pub fn rclone_config_arg(&self) -> String {
        match self.backend {
            Backend::Local => self.rclone_config.to_string_lossy().to_string(),
            Backend::Docker => self.container_config.to_string_lossy().to_string(),
        }
    }

    pub fn dest_root(&self) -> &Path {
        match self.backend {
            Backend::Local => &self.download_root,
            Backend::Docker => &self.container_download_root,
        }
    }

    /// 把容器内 /data/xxx 的显示路径换回宿主机真实路径（本地文件页用）。
    pub fn host_download_root(&self) -> &Path {
        &self.download_root
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.download_root)?;
        if self.backend == Backend::Local && !self.rclone_config.exists() {
            std::fs::write(&self.rclone_config, b"")?;
        }
        Ok(())
    }

    pub fn url(&self) -> String {
        let host = if self.bind == "0.0.0.0" || self.bind == "::" {
            "127.0.0.1"
        } else {
            &self.bind
        };
        format!("http://{}:{}/", host, self.port)
    }
}

/// 设置页 proxyUrl > 部署环境 PROXY_URL > 默认 mihomo；"off" 表示彻底直连。
pub fn effective_proxy(settings_proxy: &str, base: &str) -> Option<String> {
    let s = settings_proxy.trim();
    if s.eq_ignore_ascii_case("off") {
        return None;
    }
    let chosen = if s.is_empty() { base.trim() } else { s };
    if chosen.is_empty() || chosen.eq_ignore_ascii_case("off") {
        None
    } else {
        Some(chosen.to_string())
    }
}

pub fn help_text() -> &'static str {
    r#"云梭 CloudShuttle · Rust 原生应用进程

用法: cloud-shuttle-rs [选项]

  --port <N>              监听端口（默认 5566，或 CS_PORT）
  --bind <ADDR>           监听地址（默认 127.0.0.1，或 CS_BIND）
  --data-dir <PATH>       数据目录：settings.json / jobs.json / rclone.conf（默认 exe 同目录 data/）
  --download-root <PATH>  下载根目录（默认 <data-dir>/downloads）
  --static-dir <PATH>     前端静态资源目录（默认 exe 同目录 static/）
  --rclone-bin <NAME>     rclone 可执行文件（默认 rclone / rclone.exe）
  --rclone-config <PATH>  rclone.conf 路径（默认 <data-dir>/rclone.conf）
  --backend <auto|local|docker>  rclone 调用方式（默认自动探测）
  --container <NAME>      docker 后端容器名（默认 rclone-backend）
  --proxy <URL|off>       默认代理地址（设置页可覆盖）
  --check                 只做环境自检并退出
  --version               打印版本
  --help                  显示本帮助

环境变量：CS_PORT CS_BIND CS_DATA_DIR CS_DOWNLOAD_ROOT CS_STATIC CS_RCLONE_BIN
CS_RCLONE_CONFIG CS_RCLONE_BACKEND CS_DOCKER_CONTAINER CS_CONTAINER_RCLONE_CONFIG
CS_CONTAINER_DOWNLOAD_ROOT PROXY_URL CONSOLE_PUBLIC_HOST UPDATE_REPO
"#
}
