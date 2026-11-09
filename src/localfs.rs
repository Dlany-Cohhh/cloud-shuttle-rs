//! 本地文件页：安全列目录、批量重命名、下载目录索引。
//! 安全规则与 Python 版一致：拒绝 .. / 绝对路径 / 软链接越界，整批预检 + 两阶段改名 + 失败回滚。

use crate::config::Config;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LISTING: usize = 2000;
const RENAME_MAX_ITEMS: usize = 500;
const TMP_PREFIX: &str = ".__wb_rename_tmp__";

/// 下载根目录所在磁盘的可用空间（GB）。Windows 用 Win32 API，Unix 用 df，均不引入额外 crate。
pub fn disk_free_gb(root: &Path) -> Option<f64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(
                dir: *const u16,
                free_available: *mut u64,
                total: *mut u64,
                total_free: *mut u64,
            ) -> i32;
        }
        let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        wide.push(0);
        let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
        let ok =
            unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) };
        if ok != 0 {
            Some(free as f64 / 1073741824.0)
        } else {
            None
        }
    }
    #[cfg(not(windows))]
    {
        let out = std::process::Command::new("df")
            .arg("-Pk")
            .arg(root)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let kb: f64 = text
            .lines()
            .last()?
            .split_whitespace()
            .nth(3)?
            .parse()
            .ok()?;
        Some(kb / 1048576.0)
    }
}

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn mtime(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ok_entry_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 200
        && n != "."
        && n != ".."
        && !n.starts_with('.')
        && !n.starts_with(TMP_PREFIX)
        && !n.contains('/')
        && !n.contains('\\')
        && !n.contains('\0')
        && !n.chars().any(|c| c.is_control())
}

/// 把前端传来的相对路径解析成下载根目录下的绝对路径，并拒绝越界。
pub fn safe_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_matches('/');
    if rel.contains('\\') || rel.contains('\0') {
        return Err("路径不合法".into());
    }
    for seg in rel.split('/') {
        if seg == ".." {
            return Err("路径不合法：不允许 ..".into());
        }
    }
    let mut p = root.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty() && *s != ".") {
        p.push(seg);
    }
    let canon_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon_p = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
    if !canon_p.starts_with(&canon_root) {
        return Err("路径越界：目标不在下载根目录内".into());
    }
    Ok(p)
}

fn kind(ext: &str) -> &'static str {
    match ext {
        ".mp4" | ".mkv" | ".avi" | ".mov" | ".webm" | ".m4v" | ".ts" | ".flv" => "video",
        ".mp3" | ".flac" | ".wav" | ".aac" | ".m4a" | ".ogg" => "audio",
        ".jpg" | ".jpeg" | ".png" | ".gif" | ".webp" | ".bmp" => "image",
        ".zip" | ".rar" | ".7z" | ".tar" | ".gz" | ".tar.gz" => "archive",
        ".srt" | ".ass" | ".ssa" | ".vtt" => "subtitle",
        _ => "other",
    }
}

pub fn is_playable(ext: &str) -> bool {
    matches!(
        ext,
        ".mp4"
            | ".webm"
            | ".m4v"
            | ".mp3"
            | ".wav"
            | ".ogg"
            | ".m4a"
            | ".jpg"
            | ".jpeg"
            | ".png"
            | ".gif"
            | ".webp"
            | ".pdf"
    )
}

pub fn is_archive(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    [".zip", ".rar", ".7z", ".tar", ".gz", ".tar.gz"]
        .iter()
        .any(|e| low.ends_with(e))
}

fn playable(ext: &str) -> bool {
    is_playable(ext)
}
fn archive(name: &str) -> bool {
    is_archive(name)
}

/// 与 Python `localfiles_listing` 完全相同的返回结构。
pub fn listing(root: &Path, rel: &str) -> Value {
    let p = match safe_path(root, rel) {
        Ok(p) => p,
        Err(e) => return json!({"error": e}),
    };
    if !p.is_dir() {
        return json!({"error": "目录不存在或已被移动"});
    }
    let segs: Vec<&str> = rel
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    let mut names: Vec<String> = match fs::read_dir(&p) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect(),
        Err(e) => return json!({"error": format!("读取目录失败：{}", e)}),
    };
    names.sort_by_key(|s| s.to_lowercase());
    let mut dirs: Vec<Value> = Vec::new();
    let mut files: Vec<Value> = Vec::new();
    let mut truncated = false;
    for name in names {
        if name.starts_with('.') {
            continue;
        }
        let fp = p.join(&name);
        let md = match fs::symlink_metadata(&fp) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // 软链接一律跳过，避免把改名变成越界写入
        if md.file_type().is_symlink() {
            continue;
        }
        if dirs.len() + files.len() >= MAX_LISTING {
            truncated = true;
            break;
        }
        if md.is_dir() {
            let count = fs::read_dir(&fp).map(|rd| rd.count()).unwrap_or(0);
            dirs.push(json!({"name": name, "count": count, "mtime": mtime(&md)}));
        } else if md.is_file() {
            let ext = Path::new(&name)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase()))
                .unwrap_or_default();
            files.push(json!({
                "name": name, "size": md.len(), "mtime": mtime(&md), "ext": ext,
                "kind": kind(&ext), "playable": playable(&ext), "archive": archive(&name)
            }));
        }
    }
    let path = segs.join("/");
    let parent = if segs.len() > 1 {
        segs[..segs.len() - 1].join("/")
    } else {
        String::new()
    };
    json!({"path": path, "parent": parent, "dirs": dirs, "files": files, "truncated": truncated})
}

/// 保存目录下拉框用的单层目录列表。
pub fn dirs(root: &Path) -> Value {
    let mut out: Vec<String> = match fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !n.starts_with('.') && n != "__speedtest.tmp")
            .collect(),
        Err(e) => return json!({"dirs": [], "error": e.to_string()}),
    };
    out.sort_by_key(|s| s.to_lowercase());
    json!({"dirs": out})
}

/// 整批预检 + 两阶段临时名 + 失败回滚，返回 {"ok": n, "failed": [...]}。
pub fn rename_batch(root: &Path, rel: &str, items: &Value) -> Value {
    let folder = match safe_path(root, rel) {
        Ok(p) => p,
        Err(e) => return json!({"error": e}),
    };
    if !folder.is_dir() {
        return json!({"error": "目录不存在或已被移动"});
    }
    let arr = match items.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return json!({"error": "缺少参数：items（要重命名的文件列表）"}),
    };
    if arr.len() > RENAME_MAX_ITEMS {
        return json!({"error": format!("一次最多重命名 {} 项，请分批处理", RENAME_MAX_ITEMS)});
    }
    let mut clean: Vec<(String, String)> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for it in arr {
        let a = it
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let b = it
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !ok_entry_name(&a) || !ok_entry_name(&b) {
            failed.push(json!({"name": if a.is_empty() { "?" } else { &a }, "error": "名称不合法（不能含 /、隐藏名或临时名）"}));
            continue;
        }
        if a != b {
            clean.push((a, b));
        }
    }
    if clean.is_empty() {
        return json!({"ok": 0, "failed": failed});
    }
    let mut seen = HashSet::new();
    for (_, b) in &clean {
        if !seen.insert(b.clone()) {
            return json!({"error": format!("新名字重复：{}", b), "conflicts": [b]});
        }
    }
    let existing: HashSet<String> = match fs::read_dir(&folder) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect(),
        Err(e) => return json!({"error": format!("读取当前目录失败：{}", e)}),
    };
    let srcs: HashSet<&String> = clean.iter().map(|(a, _)| a).collect();
    let blocked: Vec<&String> = clean
        .iter()
        .map(|(_, b)| b)
        .filter(|b| existing.contains(*b) && !srcs.contains(*b))
        .collect();
    if !blocked.is_empty() {
        return json!({"error": format!("目标名称已存在：{}", blocked[0]), "conflicts": blocked});
    }
    let missing: Vec<&String> = clean
        .iter()
        .map(|(a, _)| a)
        .filter(|a| !existing.contains(*a))
        .collect();
    if !missing.is_empty() {
        return json!({"error": format!("源文件已不存在：{}", missing[0]), "conflicts": missing});
    }
    let root_canon = fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone());
    for (a, b) in &clean {
        for n in [a, b] {
            let fp = folder.join(n);
            match fs::symlink_metadata(&fp) {
                Ok(m) if m.file_type().is_symlink() => {
                    return json!({"error": format!("出于安全考虑不支持改名软链接：{}", n)});
                }
                // 目标名本来就还不存在，这是正常情况
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return json!({"error": format!("无法检查条目 {}：{}", n, e)}),
                Ok(_) => {}
            }
            let parent = fp
                .parent()
                .and_then(|p| fs::canonicalize(p).ok())
                .unwrap_or_default();
            if parent != root_canon {
                return json!({"error": format!("路径越界：{}", n)});
            }
        }
    }
    let stamp = format!("{}_{}", now_ts(), std::process::id());
    let mut tmps: Vec<String> = Vec::new();
    let mut used = existing.clone();
    for i in 0..clean.len() {
        let mut t = format!("{}{}_{}", TMP_PREFIX, i, stamp);
        while used.contains(&t) {
            t.push('_');
        }
        used.insert(t.clone());
        tmps.push(t);
    }
    // 阶段一：全部源改到临时名；任一失败立即回滚
    let mut staged: Vec<(String, String)> = Vec::new();
    for ((a, _), t) in clean.iter().zip(tmps.iter()) {
        if let Err(e) = fs::rename(folder.join(a), folder.join(t)) {
            for (a0, t0) in &staged {
                let _ = fs::rename(folder.join(t0), folder.join(a0));
            }
            return json!({"error": format!("准备改名失败，已回滚：{}", e)});
        }
        staged.push((a.clone(), t.clone()));
    }
    // 阶段二：临时名改成目标名；失败时把剩余项恢复到原名
    let mut ok = 0usize;
    let mut undo: Vec<(String, String)> = Vec::new(); // (original, tmp)
    for ((a, b), t) in clean.iter().zip(tmps.iter()) {
        match fs::rename(folder.join(t), folder.join(b)) {
            Ok(()) => ok += 1,
            Err(_) => undo.push((a.clone(), t.clone())),
        }
    }
    if !undo.is_empty() {
        // 已经成功改名且恰好是别人要恢复的原名时，不覆盖；直接把失败项留在临时名之外
        for (a, t) in &undo {
            if !folder.join(a).exists() {
                let _ = fs::rename(folder.join(t), folder.join(a));
            }
        }
    }
    json!({"ok": ok, "failed": failed})
}

/// 已下载文件索引：(小写文件名, 大小)，用于云盘页「已下载」标记。
pub fn downloaded_index(root: &Path) -> HashSet<(String, u64)> {
    let mut out = HashSet::new();
    fn walk(p: &Path, out: &mut HashSet<(String, u64)>, depth: u32) {
        if depth > 4 || out.len() > 20000 {
            return;
        }
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.filter_map(|e| e.ok()) {
                let fp = e.path();
                let md = match fs::symlink_metadata(&fp) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if md.is_dir() {
                    walk(&fp, out, depth + 1);
                } else if md.is_file() {
                    out.insert((
                        e.file_name().to_string_lossy().to_ascii_lowercase(),
                        md.len(),
                    ));
                }
            }
        }
    }
    walk(root, &mut out, 0);
    out
}

/// 目录总大小（用于任务进度与 jobdetail，限制遍历量避免卡住 API）。
pub fn dir_size(p: &Path) -> u64 {
    fn walk(p: &Path, total: &mut u64, budget: &mut u32) {
        if *budget == 0 {
            return;
        }
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.filter_map(|e| e.ok()) {
                *budget -= 1;
                if *budget == 0 {
                    return;
                }
                let fp = e.path();
                match fs::symlink_metadata(&fp) {
                    Ok(m) if m.is_dir() => walk(&fp, total, budget),
                    Ok(m) if m.is_file() => *total += m.len(),
                    _ => {}
                }
            }
        }
    }
    let mut total = 0;
    let mut budget = 20000u32;
    walk(p, &mut total, &mut budget);
    total
}

/// 任务详情里的落盘文件清单。
pub fn files_under(p: &Path) -> Value {
    let mut files: Vec<Value> = Vec::new();
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.filter_map(|e| e.ok()).take(500) {
            let fp = e.path();
            if let Ok(md) = fs::symlink_metadata(&fp) {
                if md.is_file() {
                    total += md.len();
                    files.push(json!({
                        "name": e.file_name().to_string_lossy(),
                        "size": md.len(), "mtime": mtime(&md)
                    }));
                }
            }
        }
    }
    json!({"files": files, "diskTotal": total, "diskCount": files.len(), "exists": p.exists()})
}

/// 任务里记录的 hostDest 或 dest → 宿主机真实路径。
pub fn job_dest_path(cfg: &Config, job: &Value) -> PathBuf {
    if let Some(p) = job.get("hostDest").and_then(|v| v.as_str()) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let dest = job.get("dest").and_then(|v| v.as_str()).unwrap_or("");
    if !dest.is_empty()
        && Path::new(dest).is_absolute()
        && Path::new(dest).starts_with(&cfg.download_root)
    {
        return PathBuf::from(dest);
    }
    let leaf = Path::new(dest).file_name().unwrap_or_default();
    cfg.download_root.join(leaf)
}
