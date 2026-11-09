//! /api 的 action 分发。协议与 Python 版一致：POST {"action": "...", ...} → JSON。
//! 第一版覆盖「页面能完整加载 + 真实下载编排 + 本地文件管理」所需的核心 action；
//! 其余高级功能（分享、回收站、云端解压、验证码采集等）明确返回 unsupported，不假装成功。

use crate::app::AppState;
use crate::config::APP_VERSION;
use crate::{localfs, rclone, update};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// UTC 的 "MM-DD HH:MM"，不引入 chrono，跨平台一致。
fn date_label(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let _ = yoe;
    format!(
        "{:02}-{:02} {:02}:{:02}",
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60
    )
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

pub async fn dispatch(st: AppState, body: Value) -> Value {
    let action = s(&body, "action").to_string();
    let mut out = match action.as_str() {
        "jobs" => jobs(&st),
        "stats" => stats(&st),
        "settings" => settings(&st),
        "savesettings" => savesettings(&st, &body),
        "resetsettings" => resetsettings(&st),
        "version" => version(&st),
        "health" => health(&st),
        "localfiles" => localfs::listing(st.cfg.host_download_root(), s(&body, "path")),
        "localdirs" => localfs::dirs(st.cfg.host_download_root()),
        "localrenamebatch" => localfs::rename_batch(
            st.cfg.host_download_root(),
            s(&body, "path"),
            &body["items"],
        ),
        "history" => history(&st),
        "transfers" => json!({"transfers": []}),
        "cleartransfers" => cleartransfers(&st),
        "providers" => providers(),
        "about" => about(&st, &body).await,
        "remotes" => remotes(&st).await,
        "dirs" => dirs(&st, &body).await,
        "accounts" => accounts(&st).await,
        "accttest" => accttest(&st, &body).await,
        "acctdel" => acctdel(&st, &body).await,
        "acctpurge" => acctpurge(&st).await,
        "acctadd" => acctadd(&st, &body).await,
        "acctrename" => acctrename(&st, &body).await,
        "start" => start(&st, &body).await,
        "stop" => stop(&st, &body),
        "resume" => resume(&st, &body),
        "remove" => remove(&st, &body),
        "clearfinished" => clearfinished(&st),
        "jobdetail" => jobdetail(&st, &body),
        "updatecheck" => update::check(&st.cfg.update_repo).await,
        "updatestate" => update::state(),
        "updateapply" | "updaterollback" => update::apply_unsupported(),
        "bwlimit" | "setbwlimit" => bwlimit(&st, &body),
        "ping" => json!({"ok": true, "uptime": st.started.elapsed().as_secs()}),
        _ => fallback(&action),
    };
    if let Value::Object(ref mut m) = out {
        m.entry("consoleVersion".to_string())
            .or_insert_with(|| json!(APP_VERSION));
        m.entry("runtime".to_string())
            .or_insert_with(|| json!("rust"));
    }
    out
}

fn fallback(action: &str) -> Value {
    // 只读类返回空集合，前端轮询不会报错；写操作明确告知不支持。
    let empty: Value = match action {
        "quota" => json!({"accounts": [], "quota": []}),
        "myshares" | "sharesave" | "shareparse" | "sharelimit" | "sharedel" => {
            json!({"shares": []})
        }
        "trashlist" => json!({"items": [], "listError": ""}),
        "scanpath" => json!({"tree": [], "nodes": []}),
        "unzipinfo" => json!({"files": []}),
        "playinfo" => json!({"playable": false}),
        "speedtest" => json!({"results": []}),
        "verify" => json!({"results": []}),
        "cloudlink" => json!({"error": "原生版暂不支持生成分享直链", "unsupported": true}),
        "cleanup" => {
            json!({"error": "原生版暂不支持一键清理已下载文件，请在本地文件页手动删除", "unsupported": true})
        }
        _ => json!({
            "error": format!("原生版暂不支持操作「{}」，请改用 Python / Docker 版", action),
            "unsupported": true
        }),
    };
    empty
}

fn jobs(st: &AppState) -> Value {
    let proxy = st.proxy_display();
    let disk = st.disk_state();
    let s = st.store.lock().unwrap();
    json!({
        "jobs": s.jobs, "settings": s.settings,
        "state": disk,
        "effectiveProxyUrl": proxy
    })
}

fn stats(st: &AppState) -> Value {
    let jobs = { st.store.lock().unwrap().jobs_vec() };
    let mut speed = 0f64;
    let mut bytes = 0u64;
    let mut transferring: Vec<Value> = Vec::new();
    for j in jobs {
        if s(&j, "state") != "running" {
            continue;
        }
        let b = j.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        bytes += b;
        speed += j.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cur = s(&j, "curfile");
        if !cur.is_empty() {
            transferring.push(json!({"name": cur, "bytes": b, "totalBytes": j.get("total").and_then(|v| v.as_u64()).unwrap_or(0), "speed": j.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.0)}));
        }
    }
    json!({"speed": speed as u64, "bytes": bytes, "transferring": transferring})
}

fn settings(st: &AppState) -> Value {
    let proxy = st.proxy_display();
    let disk = st.disk_state();
    let s = st.store.lock().unwrap();
    json!({
        "settings": s.settings, "state": disk,
        "effectiveProxyUrl": proxy
    })
}

fn savesettings(st: &AppState, body: &Value) -> Value {
    let s = { st.store.lock().unwrap().update_settings(body) };
    json!({"ok": true, "settings": s, "effectiveProxyUrl": st.proxy_display()})
}

fn resetsettings(st: &AppState) -> Value {
    let s = { st.store.lock().unwrap().reset_settings() };
    json!({"ok": true, "settings": s, "effectiveProxyUrl": st.proxy_display()})
}

fn version(st: &AppState) -> Value {
    json!({
        "version": st.version(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "consoleVersion": APP_VERSION,
        "https": false,
        "runtime": "rust",
        "backend": st.cfg.backend.name(),
        "publicHost": st.cfg.public_host
    })
}

fn health(st: &AppState) -> Value {
    let (running, finished, uptime) = {
        let guard = st.store.lock().unwrap();
        let js = guard.jobs_vec();
        (
            js.iter().filter(|j| s(j, "state") == "running").count(),
            js.iter().filter(|j| s(j, "state") == "done").count(),
            st.started.elapsed().as_secs(),
        )
    };
    json!({
        "allocMB": 0, "sysMB": 0, "finishedJobs": finished,
        "runningJobs": running, "uptime": uptime,
        "engine": "rust", "backend": st.cfg.backend.name(),
        "downloadRoot": st.cfg.download_root.to_string_lossy()
    })
}

fn history(st: &AppState) -> Value {
    let h = { st.store.lock().unwrap().history.clone() };
    let now = now();
    let mut buckets: std::collections::BTreeMap<i64, Vec<f64>> = Default::default();
    if let Some(samples) = h.get("samples").and_then(|v| v.as_array()) {
        for smp in samples {
            let ts = smp.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let sp = smp.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            if now - 86400 <= ts && ts <= now {
                buckets.entry(ts / 300).or_default().push(sp);
            }
        }
    }
    let series: Vec<Value> = buckets
        .iter()
        .map(|(k, v)| {
            json!([
                k * 300,
                (v.iter().sum::<f64>() / v.len() as f64 * 10.0).round() / 10.0
            ])
        })
        .collect();
    let mut daily: Vec<Value> = Vec::new();
    if let Some(map) = h.get("daily").and_then(|v| v.as_object()) {
        for (d, b) in map.iter().rev().take(14) {
            daily.push(json!({"date": d, "bytes": b}));
        }
        daily.reverse();
    }
    json!({"series": series, "daily": daily})
}

fn cleartransfers(st: &AppState) -> Value {
    let mut s = st.store.lock().unwrap();
    s.settings["transfersClearedAt"] = json!(now());
    s.save_settings();
    json!({"ok": true})
}

fn providers() -> Value {
    json!({"providers": [
        {"type": "pikpak", "label": "PikPak 网盘", "params": [
            {"name": "refresh_token", "label": "refresh_token", "ph": "可留空：填账号密码，或用书签导入采集"},
            {"name": "username", "label": "账号（邮箱）", "ph": "登录邮箱 / 手机号"},
            {"name": "password", "label": "密码", "secret": true},
            {"name": "device_id", "label": "device_id（可选）"}]},
        {"type": "webdav", "label": "WebDAV（OpenList/Alist/坚果云等）", "params": [
            {"name": "url", "label": "WebDAV 地址", "required": true},
            {"name": "user", "label": "用户名"}, {"name": "pass", "label": "密码", "secret": true},
            {"name": "vendor", "label": "厂商"}]},
        {"type": "sftp", "label": "SFTP / SSH 服务器", "params": [
            {"name": "host", "label": "主机", "required": true}, {"name": "user", "label": "用户名"},
            {"name": "pass", "label": "密码", "secret": true}, {"name": "port", "label": "端口"}]},
        {"type": "local", "label": "本地目录", "params": []}
    ]})
}

async fn remotes(st: &AppState) -> Value {
    match rclone::listremotes(&st.cfg, st.proxy().as_deref()).await {
        Ok(v) => json!({"remotes": v}),
        Err(e) => json!({"remotes": [], "error": e, "listError": e}),
    }
}

async fn dirs(st: &AppState, body: &Value) -> Value {
    let remote = s(body, "remote");
    if remote.is_empty() {
        return json!({"path": "", "dirs": [], "downloadedCount": 0, "listError": "缺少参数：remote"});
    }
    let sub = s(body, "path").trim_matches('/');
    let src = if sub.is_empty() {
        format!("{}:", remote)
    } else {
        format!("{}:{}", remote, sub)
    };
    let proxy = st.proxy();
    let listed = rclone::lsjson(&st.cfg, &src, proxy.as_deref()).await;
    let idx = localfs::downloaded_index(&st.cfg.download_root);
    let mut items: Vec<Value> = Vec::new();
    let mut list_error = String::new();
    match listed {
        Ok(rows) => {
            for e in rows {
                let name = s(&e, "Name").to_string();
                let size = e.get("Size").and_then(|v| v.as_i64()).unwrap_or(0);
                let isdir = e.get("IsDir").and_then(|v| v.as_bool()).unwrap_or(false);
                let ext = Path::new(&name)
                    .extension()
                    .map(|x| format!(".{}", x.to_string_lossy().to_ascii_lowercase()))
                    .unwrap_or_default();
                items.push(json!({
                    "name": name,
                    "isdir": isdir,
                    "size": size,
                    "mtime": s(&e, "ModTime").chars().take(19).collect::<String>(),
                    "downloaded": !isdir && size > 0 && idx.contains(&(name.to_ascii_lowercase(), size as u64)),
                    "archive": !isdir && localfs::is_archive(&name),
                    "mediaKind": "",
                    "playable": !isdir && localfs::is_playable(&ext)
                }));
            }
            items.sort_by(|a, b| {
                let ad = a.get("isdir").and_then(|v| v.as_bool()).unwrap_or(false);
                let bd = b.get("isdir").and_then(|v| v.as_bool()).unwrap_or(false);
                bd.cmp(&ad).then_with(|| {
                    s(a, "name")
                        .to_lowercase()
                        .cmp(&s(b, "name").to_lowercase())
                })
            });
        }
        Err(e) => list_error = format!("列表加载失败：{}", e),
    }
    let downloaded = items
        .iter()
        .filter(|x| {
            x.get("downloaded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    json!({"path": sub, "dirs": items, "downloadedCount": downloaded, "listError": list_error})
}

async fn about(st: &AppState, body: &Value) -> Value {
    let remotes: Vec<String> = body
        .get("remotes")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .take(8)
                .collect()
        })
        .unwrap_or_default();
    let proxy = st.proxy();
    let mut out = Vec::new();
    for r in remotes {
        let d = rclone::about(&st.cfg, &r, proxy.as_deref()).await;
        out.push(json!({
            "remote": r,
            "used": d.get("used").and_then(|v| v.as_i64()).unwrap_or(0),
            "total": d.get("total").and_then(|v| v.as_i64()).unwrap_or(0),
            "free": d.get("free").and_then(|v| v.as_i64()).unwrap_or(0),
            "error": s(&d, "error")
        }));
    }
    json!({"accounts": out})
}

async fn accounts(st: &AppState) -> Value {
    let dump = match rclone::config_dump(&st.cfg, st.proxy().as_deref()).await {
        Ok(v) => v,
        Err(e) => return json!({"accounts": [], "ghosts": [], "error": e}),
    };
    let mut list: Vec<Value> = Vec::new();
    let mut ghosts: Vec<String> = Vec::new();
    if let Some(obj) = dump.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let sec = &obj[k];
            let t = s(sec, "type").to_string();
            if t.is_empty() {
                ghosts.push(k.clone());
                continue;
            }
            let email = ["username", "user", "email", "login"]
                .iter()
                .find_map(|f| sec.get(*f).and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            list.push(json!({"name": k, "type": t, "email": email, "cred": Value::Null}));
        }
    }
    json!({"accounts": list, "ghosts": ghosts})
}

async fn accttest(st: &AppState, body: &Value) -> Value {
    let name = s(body, "name");
    if name.is_empty() {
        return json!({"ok": false, "error": "缺少账号名"});
    }
    match rclone::test_remote(&st.cfg, name, st.proxy().as_deref()).await {
        Ok(()) => json!({"ok": true, "name": name}),
        Err(e) => json!({"ok": false, "name": name, "error": e}),
    }
}

async fn acctdel(st: &AppState, body: &Value) -> Value {
    let name = s(body, "name");
    if name.is_empty() {
        return json!({"error": "缺少账号名"});
    }
    match rclone::config_call(&st.cfg, &["config", "delete", name]).await {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"error": e}),
    }
}

async fn acctpurge(st: &AppState) -> Value {
    let d = accounts(st).await;
    let ghosts: Vec<String> = d
        .get("ghosts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut removed = 0;
    for g in &ghosts {
        if rclone::config_call(&st.cfg, &["config", "delete", g])
            .await
            .is_ok()
        {
            removed += 1;
        }
    }
    json!({"ok": true, "removed": removed, "ghosts": ghosts})
}

async fn acctadd(st: &AppState, body: &Value) -> Value {
    let name = s(body, "name");
    let typ = s(body, "type");
    if name.is_empty() || typ.is_empty() {
        return json!({"error": "缺少参数：name / type"});
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return json!({"error": "账号名只能包含中英文、数字、下划线与连字符"});
    }
    let mut args: Vec<String> = vec!["config".into(), "create".into(), name.into(), typ.into()];
    if let Some(params) = body.get("params").and_then(|v| v.as_object()) {
        for (k, v) in params {
            let val = match v {
                Value::String(x) => x.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            if !val.is_empty() {
                args.push(format!("{}={}", k, val));
            }
        }
    }
    let refs: Vec<&str> = args.iter().map(|x| x.as_str()).collect();
    match rclone::config_call(&st.cfg, &refs).await {
        Ok(_) => json!({"ok": true, "name": name}),
        Err(e) => json!({"error": e}),
    }
}

async fn acctrename(st: &AppState, body: &Value) -> Value {
    let old = s(body, "name");
    let new = s(body, "newName");
    if old.is_empty() || new.is_empty() {
        return json!({"error": "缺少参数：name / newName"});
    }
    let dump = match rclone::config_dump(&st.cfg, None).await {
        Ok(v) => v,
        Err(e) => return json!({"error": e}),
    };
    let sec = match dump.get(old) {
        Some(v) if v.is_object() => v.clone(),
        _ => return json!({"error": "原账号不存在"}),
    };
    let typ = s(&sec, "type").to_string();
    let mut args: Vec<String> = vec!["config".into(), "create".into(), new.into(), typ.clone()];
    if let Some(obj) = sec.as_object() {
        for (k, v) in obj {
            if k == "type" {
                continue;
            }
            let val = match v {
                Value::String(x) => x.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            args.push(format!("{}={}", k, val));
        }
    }
    let refs: Vec<&str> = args.iter().map(|x| x.as_str()).collect();
    if let Err(e) = rclone::config_call(&st.cfg, &refs).await {
        return json!({"error": e});
    }
    let _ = rclone::config_call(&st.cfg, &["config", "delete", old]).await;
    json!({"ok": true, "name": new})
}

fn valid_folder(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 61
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ')
}

fn unique_dest(root: &Path, base: &str) -> std::path::PathBuf {
    let mut p = root.join(base);
    let mut i = 2;
    while p.exists() {
        p = root.join(format!("{}-{}", base, i));
        i += 1;
    }
    p
}

async fn start(st: &AppState, body: &Value) -> Value {
    let remote = s(body, "remote");
    if remote.is_empty() {
        return json!({"error": "缺少参数：remote（请选择账号）"});
    }
    let free = st.disk_free();
    let min = {
        let store = st.store.lock().unwrap();
        store
            .settings
            .get("diskMinGB")
            .and_then(|v| v.as_f64())
            .unwrap_or(20.0)
    };
    if free >= 0.0 && free < min {
        return json!({"error": format!("磁盘空间保护中（剩余 {:.1} GB，阈值 {} GB），请先清理或到设置里调低阈值", free, min)});
    }
    let sub = s(body, "path").trim_matches('/').to_string();
    let folder = s(body, "folder").trim_matches('/').to_string();
    if !folder.is_empty() && !valid_folder(&folder) {
        return json!({"error": "保存目录名不合法（单层：中英文/数字/空格/连字符）"});
    }
    let base_name = if !folder.is_empty() {
        folder.clone()
    } else if !sub.is_empty() {
        sub.rsplit('/').next().unwrap_or(remote).to_string()
    } else {
        remote.to_string()
    };
    let base_name: String = base_name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let host_dest = unique_dest(&st.cfg.download_root, &base_name);
    let _ = std::fs::create_dir_all(&host_dest);
    let dest = match st.cfg.backend {
        crate::config::Backend::Docker => {
            let prefix = st
                .cfg
                .container_download_root
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_string();
            format!(
                "{}/{}",
                prefix,
                host_dest.file_name().unwrap_or_default().to_string_lossy()
            )
        }
        crate::config::Backend::Local => host_dest.to_string_lossy().to_string(),
    };
    let src = if sub.is_empty() {
        format!("{}:", remote)
    } else {
        format!("{}:{}", remote, sub)
    };
    let proxy = st.proxy();

    // 预检源目录，避免建一个空任务
    match rclone::lsjson(&st.cfg, &src, proxy.as_deref()).await {
        Ok(rows) if rows.is_empty() => {
            return json!({"error": format!("源目录为空（{}），没有可下载的文件", src)})
        }
        Err(e) => return json!({"error": format!("预检源目录失败：{}", e)}),
        _ => {}
    }
    let total = rclone::size_json(&st.cfg, &src, proxy.as_deref())
        .await
        .get("bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let id = now_ms().to_string();
    let job = json!({
        "id": id, "jobid": Value::Null,
        "remote": remote, "subpath": sub, "src": src,
        "name": base_name, "mode": s(body, "mode").to_string(),
        "dest": dest, "hostDest": host_dest.to_string_lossy(),
        "state": "queued", "started": date_label(now()),
        "started_ts": now(), "finished_ts": 0,
        "bytes": 0, "total": total, "speed": 0, "eta": Value::Null,
        "error": "", "proxy": proxy.is_some(), "curfile": "",
        "pid": 0, "lastBytes": 0, "lastSampleTs": now()
    });
    {
        let mut store = st.store.lock().unwrap();
        if let Some(arr) = store.jobs.as_array_mut() {
            arr.push(job.clone());
        }
        store.save_jobs();
    }
    tick(&st.clone()).await;
    json!({"job": job, "ok": true})
}

async fn launch(st: AppState, id: String) {
    // 先占住 id，避免调度器与 start 同时为同一个任务拉起两个 rclone 进程。
    {
        let mut pids = st.pids.lock().unwrap();
        if pids.contains_key(&id) {
            return;
        }
        pids.insert(id.clone(), 0);
    }
    // proxy() 内部也要读 store，必须在持有 store 锁之前取，否则 std Mutex 自锁死。
    let proxy = st.proxy();
    let (job, settings) = {
        let store = st.store.lock().unwrap();
        (store.job(&id), store.settings.clone())
    };
    let job = match job {
        Some(j) => j,
        None => {
            st.pids.lock().unwrap().remove(&id);
            return;
        }
    };
    if s(&job, "state") != "queued" && s(&job, "state") != "running" {
        st.pids.lock().unwrap().remove(&id);
        return;
    }
    let src = s(&job, "src").to_string();
    let dest = s(&job, "dest").to_string();
    let mut child =
        match rclone::copy_command(&st.cfg, &src, &dest, &settings, proxy.as_deref()).spawn() {
            Ok(c) => c,
            Err(e) => {
                st.pids.lock().unwrap().remove(&id);
                let mut store = st.store.lock().unwrap();
                if let Some(j) = store.job_mut(&id) {
                    j["state"] = json!("error");
                    j["error"] = json!(format!("无法启动 rclone：{}", e));
                }
                store.save_jobs();
                return;
            }
        };
    let pid = child.id().unwrap_or(0);
    st.pids.lock().unwrap().insert(id.clone(), pid);
    {
        let mut store = st.store.lock().unwrap();
        if let Some(j) = store.job_mut(&id) {
            j["state"] = json!("running");
            j["pid"] = json!(pid);
            j["started_ts"] = json!(now());
            j["error"] = json!("");
        }
        store.save_jobs();
    }
    let st2 = st.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        st2.pids.lock().unwrap().remove(&id2);
        let mut store = st2.store.lock().unwrap();
        if let Some(j) = store.job_mut(&id2) {
            let state = s(j, "state").to_string();
            if state == "running" {
                let ok = status.map(|s| s.success()).unwrap_or(false);
                j["state"] = json!(if ok { "done" } else { "error" });
                j["error"] = json!(if ok {
                    ""
                } else {
                    "rclone 进程异常退出，可点「继续」重试"
                });
                j["speed"] = json!(0);
                j["finished_ts"] = json!(now());
            }
        }
        store.save_jobs();
    });
}

/// 调度 + 进度采样 + 历史记录。由后台任务每 2 秒调用一次。
pub async fn tick(st: &AppState) {
    let (max, running, queued, jobs) = {
        let store = st.store.lock().unwrap();
        let max = store
            .settings
            .get("maxConcurrent")
            .and_then(|v| v.as_i64())
            .unwrap_or(2) as usize;
        let js = store.jobs_vec();
        let running = js.iter().filter(|j| s(j, "state") == "running").count();
        let queued: Vec<String> = js
            .iter()
            .filter(|j| s(j, "state") == "queued")
            .map(|j| {
                j.get("id")
                    .map(|v| match v {
                        Value::Number(n) => n.to_string(),
                        Value::String(x) => x.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default()
            })
            .filter(|x| !x.is_empty())
            .collect();
        (max, running, queued, js)
    };
    for id in queued.into_iter().take(max.saturating_sub(running)) {
        launch(st.clone(), id).await;
    }
    // 进度：按目标目录实际落盘大小估算（rclone 保留源文件大小，直接求和即可）
    let mut changed = false;
    let mut total_speed = 0f64;
    {
        let mut store = st.store.lock().unwrap();
        let ts = now();
        for j in jobs {
            if s(&j, "state") != "running" {
                continue;
            }
            let id = j
                .get("id")
                .map(|v| match v {
                    Value::Number(n) => n.to_string(),
                    Value::String(x) => x.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let p = localfs::job_dest_path(&st.cfg, &j);
            let bytes = localfs::dir_size(&p);
            let last = j.get("lastBytes").and_then(|v| v.as_u64()).unwrap_or(0);
            let last_ts = j.get("lastSampleTs").and_then(|v| v.as_i64()).unwrap_or(ts);
            let dt = (ts - last_ts).max(1) as f64;
            let speed = bytes.saturating_sub(last) as f64 / dt;
            let total = j.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
            if let Some(slot) = store.job_mut(&id) {
                slot["bytes"] = json!(bytes);
                slot["speed"] = json!(speed.round() as u64);
                slot["lastBytes"] = json!(bytes);
                slot["lastSampleTs"] = json!(ts);
                if total > 0 && speed > 0.0 {
                    slot["eta"] =
                        json!(((total.saturating_sub(bytes)) as f64 / speed).round() as i64);
                }
            }
            total_speed += speed;
            changed = true;
        }
        if changed {
            store.save_jobs();
        }
        // 每 5 分钟记一个速度样本，24 小时后自动淘汰
        let bucket = ts / 300;
        let should_record = store
            .history
            .get("lastBucket")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            != bucket;
        if should_record {
            if let Some(samples) = store.history["samples"].as_array_mut() {
                samples.push(json!([
                    ts,
                    (total_speed / 1048576.0 * 100.0).round() / 100.0
                ]));
                let cutoff = ts - 86400;
                samples.retain(|x| x.get(0).and_then(|v| v.as_i64()).unwrap_or(0) >= cutoff);
                if samples.len() > 400 {
                    let drain = samples.len() - 400;
                    samples.drain(0..drain);
                }
            }
            store.history["lastBucket"] = json!(bucket);
            store.save_history();
        }
    }
}

fn stop(st: &AppState, body: &Value) -> Value {
    let id = id_of(body);
    let pid = { st.pids.lock().unwrap().get(&id).copied() };
    if let Some(p) = pid {
        if p > 0 {
            rclone::kill(p);
        }
    }
    let mut store = st.store.lock().unwrap();
    if let Some(j) = store.job_mut(&id) {
        if matches!(s(j, "state"), "running" | "queued") {
            j["state"] = json!("stopped");
            j["error"] = json!("");
            j["speed"] = json!(0);
        }
    }
    store.save_jobs();
    json!({"ok": true})
}

fn id_of(body: &Value) -> String {
    match body.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn resume(st: &AppState, body: &Value) -> Value {
    let id = id_of(body);
    let mut store = st.store.lock().unwrap();
    if let Some(j) = store.job_mut(&id) {
        if matches!(s(j, "state"), "stopped" | "error") {
            j["state"] = json!("queued");
            j["error"] = json!("");
            j["speed"] = json!(0);
            j["lastBytes"] = json!(0);
            j["lastSampleTs"] = json!(now());
        }
    }
    store.save_jobs();
    json!({"ok": true})
}

fn remove(st: &AppState, body: &Value) -> Value {
    let id = id_of(body);
    let pid = { st.pids.lock().unwrap().remove(&id) };
    if let Some(p) = pid {
        if p > 0 {
            rclone::kill(p);
        }
    }
    let mut store = st.store.lock().unwrap();
    if let Some(arr) = store.jobs.as_array_mut() {
        arr.retain(|j| match j.get("id") {
            Some(Value::Number(n)) => n.to_string() != id,
            Some(Value::String(x)) => *x != id,
            _ => true,
        });
    }
    store.save_jobs();
    json!({"ok": true})
}

fn clearfinished(st: &AppState) -> Value {
    let mut store = st.store.lock().unwrap();
    if let Some(arr) = store.jobs.as_array_mut() {
        arr.retain(|j| matches!(s(j, "state"), "running" | "queued"));
    }
    store.save_jobs();
    json!({"ok": true})
}

fn jobdetail(st: &AppState, body: &Value) -> Value {
    let id = id_of(body);
    let job = { st.store.lock().unwrap().job(&id) };
    let job = match job {
        Some(j) => j,
        None => return json!({"error": "任务不存在"}),
    };
    let mut out = localfs::files_under(&localfs::job_dest_path(&st.cfg, &job));
    out["job"] = job;
    out["transferring"] = json!([]);
    out["truncated"] = json!(false);
    out
}

fn bwlimit(st: &AppState, body: &Value) -> Value {
    if body.get("bw").is_some() || body.get("limit").is_some() {
        let v = body
            .get("bw")
            .or_else(|| body.get("limit"))
            .cloned()
            .unwrap_or(Value::Null);
        let mut store = st.store.lock().unwrap();
        if v.is_string() || v.is_number() {
            store.settings["bwLimit"] = v.clone();
        }
        if let Some(on) = body.get("on").and_then(|v| v.as_bool()) {
            store.settings["bwLimitOn"] = json!(on);
        }
        store.save_settings();
    }
    let s = { st.store.lock().unwrap().settings.clone() };
    json!({"bw": s.get("bwLimit").cloned().unwrap_or(json!("")), "on": s.get("bwLimitOn").cloned().unwrap_or(json!(false))})
}
