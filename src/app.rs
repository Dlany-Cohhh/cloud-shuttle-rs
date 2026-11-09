//! 进程内共享状态：配置、持久化数据、运行中的 rclone 子进程 PID 表。

use crate::config::{effective_proxy, Config};
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Arc<Mutex<Store>>,
    /// job id -> rclone 进程 PID
    pub pids: Arc<Mutex<HashMap<String, u32>>>,
    pub started: Instant,
    pub rclone_version: Arc<Mutex<String>>,
    /// 磁盘可用空间缓存（GB, 上次刷新时间）；-1 表示无法检测
    pub disk: Arc<Mutex<(f64, Instant)>>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let store = Store::open(&cfg.data_dir);
        let free = crate::localfs::disk_free_gb(&cfg.download_root).unwrap_or(-1.0);
        AppState {
            store: Arc::new(Mutex::new(store)),
            pids: Arc::new(Mutex::new(HashMap::new())),
            started: Instant::now(),
            rclone_version: Arc::new(Mutex::new(String::new())),
            disk: Arc::new(Mutex::new((free, Instant::now()))),
            cfg: Arc::new(cfg),
        }
    }

    /// 设置页 > 环境变量 > 默认；调用方不要在同一线程上先持有 store 锁。
    pub fn proxy(&self) -> Option<String> {
        let sp = {
            let s = self.store.lock().unwrap();
            s.settings
                .get("proxyUrl")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        effective_proxy(&sp, &self.cfg.base_proxy)
    }

    pub fn proxy_display(&self) -> String {
        self.proxy()
            .unwrap_or_else(|| "直连（未启用代理）".to_string())
    }

    pub fn version(&self) -> String {
        self.rclone_version.lock().unwrap().clone()
    }

    /// 每 60 秒最多探测一次磁盘，避免前端 2 秒轮询把 df 打爆。
    pub fn disk_free(&self) -> f64 {
        let mut d = self.disk.lock().unwrap();
        if d.1.elapsed() > Duration::from_secs(60) {
            d.0 = crate::localfs::disk_free_gb(&self.cfg.download_root).unwrap_or(-1.0);
            d.1 = Instant::now();
        }
        d.0
    }

    /// state 字段；无法检测时 diskFree 用 null，前端显示 "--"。
    pub fn disk_state(&self) -> Value {
        let free = self.disk_free();
        let min = {
            let s = self.store.lock().unwrap();
            s.settings
                .get("diskMinGB")
                .and_then(|v| v.as_f64())
                .unwrap_or(20.0)
        };
        if free < 0.0 {
            json!({"diskLow": false, "diskFree": Value::Null})
        } else {
            json!({"diskLow": free < min, "diskFree": (free * 10.0).round() / 10.0})
        }
    }
}

pub fn log(msg: &str) {
    println!("{}", msg);
}
