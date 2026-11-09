//! settings.json / jobs.json / history.json 的读写与规范化。
//! 字段与 Python 版保持兼容，前端无需改动即可消费。

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub fn defaults() -> Value {
    json!({
        "maxConcurrent": 2,
        "autoStart": "",
        "autoPause": "",
        "diskMinGB": 20,
        "transfers": 6,
        "checkers": 8,
        "proxy": false,
        "downloadProxy": true,
        "advancedMode": false,
        "multiThreadStreams": 8,
        "multiThreadCutoffMB": 64,
        "orderBy": "size,descending",
        "bwLimitOn": false,
        "bwLimit": "5M",
        "retries": 10,
        "timeout": "1m",
        "idleTimeout": "10m",
        "createEmptySrcDirs": true,
        "logLevel": "INFO",
        "logFile": "",
        "transfersClearedAt": 0,
        "shareAutoCancel": false,
        "nasHomeUrl": "http://127.0.0.1:8080/",
        "proxyUrl": ""
    })
}

const ORDER_BY: &[&str] = &[
    "",
    "size,descending",
    "size,ascending",
    "name,ascending",
    "modtime,descending",
    "modtime,ascending",
];
const LOG_LEVELS: &[&str] = &["INFO", "DEBUG", "NOTICE", "WARN", "ERROR", "OFF"];

fn num(d: &mut Map<String, Value>, key: &str, lo: f64, hi: f64, int: bool) {
    let default = defaults().get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let v = d.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
    let mut v = v.max(lo).min(hi);
    if int {
        v = v.round();
        d.insert(key.to_string(), json!(v as i64));
    } else {
        d.insert(key.to_string(), json!(v));
    }
}

fn str_field(d: &mut Map<String, Value>, key: &str, max: usize, default: &str) {
    let v = d.get(key).and_then(|v| v.as_str()).unwrap_or(default);
    let v: String = v.chars().take(max).collect();
    d.insert(key.to_string(), json!(v));
}

fn bool_field(d: &mut Map<String, Value>, key: &str, default: bool) {
    let v = match d.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(s)) => {
            matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        }
        _ => default,
    };
    d.insert(key.to_string(), json!(v));
}

/// 与 Python 版 `_norm_settings` 等价的白名单 + 钳制。
pub fn norm_settings(raw: Option<&Value>) -> Value {
    let mut d = defaults();
    if let Some(Value::Object(src)) = raw {
        let base = d.as_object().unwrap().clone();
        for (k, _) in base {
            if let Some(v) = src.get(&k) {
                d[k] = v.clone();
            }
        }
    }
    let m = d.as_object_mut().unwrap();
    num(m, "maxConcurrent", 1.0, 6.0, true);
    num(m, "diskMinGB", 0.0, 10000.0, false);
    num(m, "transfers", 1.0, 12.0, true);
    num(m, "checkers", 1.0, 16.0, true);
    num(m, "multiThreadStreams", 1.0, 32.0, true);
    num(m, "multiThreadCutoffMB", 1.0, 1024.0, true);
    num(m, "retries", 0.0, 100.0, true);
    num(m, "transfersClearedAt", 0.0, 4102444800.0, false);
    bool_field(m, "proxy", false);
    bool_field(m, "downloadProxy", true);
    bool_field(m, "advancedMode", false);
    bool_field(m, "shareAutoCancel", false);
    bool_field(m, "bwLimitOn", false);
    bool_field(m, "createEmptySrcDirs", true);
    str_field(m, "autoStart", 5, "");
    str_field(m, "autoPause", 5, "");
    str_field(m, "orderBy", 40, "size,descending");
    if !ORDER_BY.contains(&m["orderBy"].as_str().unwrap_or("")) {
        m.insert("orderBy".into(), json!("size,descending"));
    }
    let lvl = m
        .get("logLevel")
        .and_then(|v| v.as_str())
        .unwrap_or("INFO")
        .to_ascii_uppercase();
    m.insert(
        "logLevel".into(),
        json!(if LOG_LEVELS.contains(&lvl.as_str()) {
            lvl
        } else {
            "INFO".into()
        }),
    );
    for k in ["timeout", "idleTimeout"] {
        let v = m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ok = v.len() > 1
            && v[..v.len() - 1].chars().all(|c| c.is_ascii_digit())
            && matches!(v.chars().last(), Some('s' | 'm' | 'h'))
            || (v.ends_with("ms") && v[..v.len() - 2].chars().all(|c| c.is_ascii_digit()));
        if !ok {
            m.insert(k.into(), json!(if k == "timeout" { "1m" } else { "10m" }));
        }
    }
    let bw = m
        .get("bwLimit")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let bw_ok = !bw.is_empty()
        && {
            let mut chars = bw.chars();
            let first = chars.next().unwrap();
            first.is_ascii_digit()
        }
        && bw.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
    if !bw_ok {
        m.insert("bwLimit".into(), json!("5M"));
    }
    str_field(m, "logFile", 200, "");
    let home = m
        .get("nasHomeUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let home_ok = (home.starts_with("http://") || home.starts_with("https://"))
        && !home.contains(char::is_whitespace)
        && !home.contains(['<', '>', '"', '\'']);
    if !home_ok {
        m.insert("nasHomeUrl".into(), json!("http://127.0.0.1:8080/"));
    }
    let p = m
        .get("proxyUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let p = p.trim();
    let p_ok = p.is_empty()
        || p.eq_ignore_ascii_case("off")
        || (p.starts_with("http://") || p.starts_with("https://") || p.starts_with("socks5://"))
            && !p.contains(char::is_whitespace);
    if !p_ok {
        m.insert("proxyUrl".into(), json!(""));
    }
    d
}

pub fn load_json(path: &Path, fallback: Value) -> Value {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(fallback)
}

pub fn save_json(path: &Path, v: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(v).unwrap_or_else(|_| b"{}".to_vec());
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &data)?;
    // Windows 的 rename 不能覆盖已存在文件，先删旧文件；失败再退回直接写。
    #[cfg(windows)]
    let _ = fs::remove_file(path);
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::write(path, &data)?;
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pub settings_path: PathBuf,
    pub jobs_path: PathBuf,
    pub history_path: PathBuf,
    pub settings: Value,
    pub jobs: Value,
    pub history: Value,
}

impl Store {
    pub fn open(dir: &Path) -> Self {
        let settings_path = dir.join("settings.json");
        let jobs_path = dir.join("jobs.json");
        let history_path = dir.join("history.json");
        let settings = norm_settings(Some(&load_json(&settings_path, json!({}))));
        let jobs = match load_json(&jobs_path, json!([])) {
            Value::Array(_) => load_json(&jobs_path, json!([])),
            _ => json!([]),
        };
        let mut history = load_json(&history_path, json!({"samples": [], "daily": {}}));
        if !history.is_object() {
            history = json!({"samples": [], "daily": {}});
        }
        if !history
            .get("samples")
            .map(|v| v.is_array())
            .unwrap_or(false)
        {
            history["samples"] = json!([]);
        }
        if !history.get("daily").map(|v| v.is_object()).unwrap_or(false) {
            history["daily"] = json!({});
        }
        Store {
            settings_path,
            jobs_path,
            history_path,
            settings,
            jobs,
            history,
        }
    }

    pub fn save_settings(&self) {
        let _ = save_json(&self.settings_path, &self.settings);
    }

    pub fn save_jobs(&self) {
        let _ = save_json(&self.jobs_path, &self.jobs);
    }

    pub fn save_history(&self) {
        let _ = save_json(&self.history_path, &self.history);
    }

    pub fn update_settings(&mut self, patch: &Value) -> Value {
        let merged = norm_settings(Some(&{
            let mut m = self.settings.as_object().cloned().unwrap_or_default();
            if let Value::Object(p) = patch {
                let allowed: Vec<String> =
                    defaults().as_object().unwrap().keys().cloned().collect();
                for (k, v) in p {
                    if allowed.contains(k) {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Object(m)
        }));
        self.settings = merged.clone();
        self.save_settings();
        merged
    }

    pub fn reset_settings(&mut self) -> Value {
        let cleared = self
            .settings
            .get("transfersClearedAt")
            .cloned()
            .unwrap_or(json!(0));
        let mut s = defaults();
        s["transfersClearedAt"] = cleared;
        let s = norm_settings(Some(&s));
        self.settings = s.clone();
        self.save_settings();
        s
    }

    pub fn jobs_vec(&self) -> Vec<Value> {
        self.jobs.as_array().cloned().unwrap_or_default()
    }

    pub fn job_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.jobs.as_array_mut()?.iter_mut().find(|j| {
            j.get("id")
                .map(|v| match v {
                    Value::Number(n) => n.to_string() == id,
                    Value::String(s) => s == id,
                    _ => false,
                })
                .unwrap_or(false)
        })
    }

    pub fn job(&self, id: &str) -> Option<Value> {
        self.jobs
            .as_array()?
            .iter()
            .find(|j| {
                j.get("id")
                    .map(|v| match v {
                        Value::Number(n) => n.to_string() == id,
                        Value::String(s) => s == id,
                        _ => false,
                    })
                    .unwrap_or(false)
            })
            .cloned()
    }
}
