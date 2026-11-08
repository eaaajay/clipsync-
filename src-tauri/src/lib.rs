use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

const SYNC_FILE: &str = "/clipsync/clip-sync.json";
const POLL_INTERVAL_MS: u64 = 1500;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub webdav_url: String,
    pub username: String,
    pub password: String,
    pub device_id: String,
    /// 呼出窗口的全局快捷键（Tauri accelerator 语法，留空表示关闭）
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
}

fn default_hotkey() -> String {
    "CmdOrCtrl+Shift+V".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            webdav_url: String::new(),
            username: String::new(),
            password: String::new(),
            device_id: new_device_id(),
            hotkey: default_hotkey(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteClip {
    device_id: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncStatus {
    running: bool,
    last_error: Option<String>,
    last_sync_at: Option<u64>,
    /// 待发送条数
    pending: usize,
}

/// 收藏的删除墓碑，用于多端同步时不把已删除的收藏再合并回来
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tombstone {
    id: String,
    time: u64,
}

/// 收藏集合（本地文件与 WebDAV 远端使用同一格式）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FavDoc {
    #[serde(default)]
    favorites: Vec<Entry>,
    #[serde(default)]
    deleted: Vec<Tombstone>,
}

const TOMBSTONE_RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;
const FAV_SYNC_INTERVAL_MS: u64 = 6000;

impl FavDoc {
    fn prune_tombstones(&mut self) {
        let cutoff = now_ms().saturating_sub(TOMBSTONE_RETENTION_MS);
        self.deleted.retain(|t| t.time >= cutoff);
    }

    fn is_deleted(&self, id: &str) -> bool {
        self.deleted.iter().any(|t| t.id == id)
    }

    fn remove_favorite(&mut self, id: &str) -> bool {
        if let Some(pos) = self.favorites.iter().position(|e| e.id == id) {
            self.favorites.remove(pos);
            if !self.is_deleted(id) {
                self.deleted.push(Tombstone {
                    id: id.to_string(),
                    time: now_ms(),
                });
            }
            return true;
        }
        false
    }

    /// 合并远端：删除墓碑优先，其余取并集
    fn merge(&self, remote: &FavDoc) -> FavDoc {
        let mut out = self.clone();
        out.prune_tombstones();

        // 1) 合并墓碑
        for t in &remote.deleted {
            if !out.is_deleted(&t.id) {
                out.deleted.push(t.clone());
            }
        }
        // 2) 应用墓碑：删掉本地对应收藏
        let dead: std::collections::HashSet<String> =
            out.deleted.iter().map(|t| t.id.clone()).collect();
        out.favorites.retain(|e| !dead.contains(&e.id));

        // 3) 并入远端收藏（已存在则取较新的使用时间）
        for e in &remote.favorites {
            if dead.contains(&e.id) {
                continue;
            }
            if let Some(existing) = out.favorites.iter_mut().find(|x| x.id == e.id) {
                existing.used_at = existing.used_at.max(e.used_at);
                existing.thumb = existing.thumb.clone().or_else(|| e.thumb.clone());
            } else {
                out.favorites.push(e.clone());
            }
        }

        // 按时间倒序，稳定展示
        out.favorites.sort_by(|a, b| b.time.cmp(&a.time));
        out.favorites.truncate(MAX_FAVORITES);
        out
    }
}

/// 一条同步记录。dir: "up" = 本机 -> 云端, "down" = 云端 -> 本机
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    time: u64,
    dir: String,
    /// "text" | "image"
    #[serde(default = "default_kind")]
    kind: String,
    preview: String,
    /// 文本内容（供复制）；图片记录里存的是图片哈希
    content: String,
    len: usize,
    /// 图片缩略图（data URL）
    #[serde(default)]
    thumb: Option<String>,
    /// 最近一次被使用（粘贴/复制）的时间，0 表示没用过
    #[serde(default)]
    used_at: u64,
}

fn default_ext() -> String {
    "png".to_string()
}

fn default_kind() -> String {
    "text".to_string()
}

const MAX_HISTORY: usize = 500;
const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000; // 最近记录只保留 7 天
const PREVIEW_CHARS: usize = 200;
const CONTENT_CHARS: usize = 20000;
const MAX_FAVORITES: usize = 1000;

fn make_preview(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let mut out: String = trimmed.chars().take(PREVIEW_CHARS).collect();
    if trimmed.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

fn cap_content(s: &str) -> String {
    if s.chars().count() > CONTENT_CHARS {
        s.chars().take(CONTENT_CHARS).collect()
    } else {
        s.to_string()
    }
}

#[derive(Clone)]
struct SyncState {
    running: Arc<AtomicBool>,
    last_local_sent: Arc<Mutex<Option<String>>>,
    last_applied_remote: Arc<Mutex<Option<String>>>,
    last_error: Arc<Mutex<Option<String>>>,
    last_sync_at: Arc<Mutex<Option<u64>>>,
    history: Arc<Mutex<std::collections::VecDeque<Entry>>>,
    /// 收藏：永久保留，持久化到磁盘并与 WebDAV 同步
    favorites: Arc<Mutex<FavDoc>>,
    last_fav_sync: Arc<Mutex<u64>>,
    /// 最近一次上传/应用的图片哈希
    last_local_image: Arc<Mutex<Option<String>>>,
    last_applied_image: Arc<Mutex<Option<String>>>,
    /// 应用远端图片后短暂抑制上传，避免回环
    img_suppress_until: Arc<Mutex<u64>>,
    /// 已知图片清单（本地 + 云端合并结果）
    local_images: Arc<Mutex<ImageIndex>>,
    /// 是否已建立「启动基线」（首次读到剪贴板时不当作新内容上传）
    baseline_done: Arc<AtomicBool>,
    /// 上次看到的剪贴板 changeCount（macOS）
    last_clip_count: Arc<Mutex<i64>>,
    /// 待发送队列（断网时保留，恢复后补传）
    outbox: Arc<Mutex<Vec<OutboxItem>>>,
    seq: Arc<std::sync::atomic::AtomicU64>,
    /// Android Activity 存活标记；退后台不删除，只有 onDestroy 才删除。
    #[cfg(target_os = "android")]
    activity_marker: Arc<Mutex<Option<std::path::PathBuf>>>,
    #[cfg(target_os = "android")]
    native_status_cache: Arc<Mutex<Option<String>>>,
}

impl SyncState {
    fn next_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        format!("{}-{}", now_ms(), n)
    }

    fn push_history(&self, app: &AppHandle, dir: &str, content: &str) {
        self.push_history_kind(app, dir, content, "text");
    }

    fn push_history_kind(&self, app: &AppHandle, dir: &str, content: &str, kind: &str) {
        let now = now_ms();
        let entry = Entry {
            id: self.next_id(),
            time: now,
            dir: dir.to_string(),
            kind: kind.to_string(),
            preview: make_preview(content),
            content: if kind == "image" { content.to_string() } else { cap_content(content) },
            len: if kind == "image" { 0 } else { content.chars().count() },
            thumb: None,
            used_at: 0,
        };
        {
            let mut h = self.history.lock().unwrap();
            let duplicated = h
                .front()
                .map(|f| {
                    f.dir == entry.dir
                        && f.kind == entry.kind
                        && f.preview == entry.preview
                        && now.saturating_sub(f.time) < 5_000
                })
                .unwrap_or(false);
            if duplicated {
                return;
            }
            h.push_front(entry.clone());
            // 超过 7 天的记录丢弃
            let cutoff = now.saturating_sub(RETENTION_MS);
            while h.back().map(|e| e.time.max(e.used_at) < cutoff).unwrap_or(false) {
                h.pop_back();
            }
            while h.len() > MAX_HISTORY {
                h.pop_back();
            }
        }
        {
            let h = self.history.lock().unwrap();
            save_history(app, &h);
        }
        update_tray_latest(app, &entry);
        let _ = app.emit("sync-history", ());
    }

    fn push_image_history(
        &self,
        app: &AppHandle,
        dir: &str,
        hash: &str,
        label: &str,
        thumb: Option<String>,
    ) {
        let now = now_ms();
        let entry = Entry {
            id: self.next_id(),
            time: now,
            dir: dir.to_string(),
            kind: "image".to_string(),
            preview: label.to_string(),
            content: hash.to_string(),
            len: 0,
            thumb,
            used_at: 0,
        };
        {
            let mut h = self.history.lock().unwrap();
            let duplicated = h
                .front()
                .map(|f| {
                    f.dir == entry.dir
                        && f.kind == entry.kind
                        && f.preview == entry.preview
                        && now.saturating_sub(f.time) < 5_000
                })
                .unwrap_or(false);
            if duplicated {
                return;
            }
            h.push_front(entry.clone());
            let cutoff = now.saturating_sub(RETENTION_MS);
            while h.back().map(|e| e.time.max(e.used_at) < cutoff).unwrap_or(false) {
                h.pop_back();
            }
            while h.len() > MAX_HISTORY {
                h.pop_back();
            }
        }
        {
            let h = self.history.lock().unwrap();
            save_history(app, &h);
        }
        update_tray_latest(app, &entry);
        let _ = app.emit("sync-history", ());
    }

    /// 从历史中清掉过期记录
    fn prune_history(&self) {
        let cutoff = now_ms().saturating_sub(RETENTION_MS);
        let mut h = self.history.lock().unwrap();
        while h.back().map(|e| e.time.max(e.used_at) < cutoff).unwrap_or(false) {
            h.pop_back();
        }
    }
}

// ---------- 收藏持久化 ----------
fn favorites_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法获取配置目录: {}", e))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {}", e))?;
    Ok(dir.join("favorites.json"))
}

fn load_favorites(app: &AppHandle) -> FavDoc {
    let Ok(path) = favorites_path(app) else {
        return FavDoc::default();
    };
    if !path.exists() {
        return FavDoc::default();
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return FavDoc::default();
    };
    // 兼容旧格式（纯数组）
    if let Ok(doc) = serde_json::from_str::<FavDoc>(&text) {
        return doc;
    }
    if let Ok(items) = serde_json::from_str::<Vec<Entry>>(&text) {
        return FavDoc {
            favorites: items,
            deleted: Vec::new(),
        };
    }
    FavDoc::default()
}

fn save_favorites(app: &AppHandle, doc: &FavDoc) -> Result<(), String> {
    let path = favorites_path(app)?;
    let data =
        serde_json::to_string_pretty(doc).map_err(|e| format!("序列化收藏失败: {}", e))?;
    std::fs::write(&path, data).map_err(|e| format!("写入收藏失败: {}", e))
}

// ---------- 最近记录持久化 ----------
fn history_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法获取配置目录: {}", e))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {}", e))?;
    Ok(dir.join("history.json"))
}

fn load_history(app: &AppHandle) -> std::collections::VecDeque<Entry> {
    let mut out = std::collections::VecDeque::new();
    let Ok(path) = history_path(app) else {
        return out;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return out;
    };
    let Ok(items) = serde_json::from_str::<Vec<Entry>>(&text) else {
        return out;
    };
    let cutoff = now_ms().saturating_sub(RETENTION_MS);
    for e in items.into_iter().filter(|e| e.time.max(e.used_at) >= cutoff) {
        out.push_back(e);
    }
    while out.len() > MAX_HISTORY {
        out.pop_back();
    }
    out
}

fn save_history(app: &AppHandle, items: &std::collections::VecDeque<Entry>) {
    let Ok(path) = history_path(app) else {
        return;
    };
    let vec: Vec<&Entry> = items.iter().collect();
    if let Ok(data) = serde_json::to_string(&vec) {
        let _ = std::fs::write(&path, data);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_device_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("dev-{:x}", nanos)
}

// ---------- Android 相册保存 ----------
#[cfg(target_os = "android")]
struct AndroidGallery<R: tauri::Runtime>(tauri::plugin::PluginHandle<R>);

#[cfg(desktop)]
fn hotkey_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_global_shortcut::Builder::new().build()
}

#[cfg(mobile)]
fn hotkey_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("global-shortcut").build()
}

fn gallery_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    #[cfg(target_os = "android")]
    {
        tauri::plugin::Builder::new("clipsync-gallery")
            .setup(|app, api| {
                let handle =
                    api.register_android_plugin("com.wyj.clipsync", "ClipImagePlugin")?;
                app.manage(AndroidGallery(handle));
                Ok(())
            })
            .build()
    }
    #[cfg(not(target_os = "android"))]
    {
        tauri::plugin::Builder::new("clipsync-gallery").build()
    }
}

/// 取走无障碍服务捕获的后台剪贴板文本（仅 Android）
#[cfg(target_os = "android")]
fn take_background_clipboard(app: &AppHandle) -> Option<String> {
    #[derive(serde::Serialize)]
    struct Empty {}
    #[derive(serde::Deserialize)]
    struct Res {
        #[serde(default)]
        empty: bool,
        #[serde(default)]
        text: Option<String>,
    }
    let st = app.try_state::<AndroidGallery<tauri::Wry>>()?;
    let res: Res = st
        .0
        .run_mobile_plugin("takeBackgroundClipboard", Empty {})
        .ok()?;
    if res.empty {
        None
    } else {
        res.text
    }
}

#[cfg(not(target_os = "android"))]
fn take_background_clipboard(_app: &AppHandle) -> Option<String> {
    None
}

/// 非阻塞读取通知验证码。插件异常或回调丢失时只暂停 OTP，不拖住文字/收藏同步线程。
#[cfg(target_os = "android")]
fn take_notification_otp(app: &AppHandle) -> Option<String> {
    static RESULTS: std::sync::OnceLock<Mutex<std::collections::VecDeque<String>>> =
        std::sync::OnceLock::new();
    static CALL_ACTIVE: AtomicBool = AtomicBool::new(false);

    #[derive(serde::Serialize)]
    struct Empty {}
    #[derive(serde::Deserialize)]
    struct Res {
        #[serde(default)]
        empty: bool,
        #[serde(default)]
        text: Option<String>,
    }

    let results = RESULTS.get_or_init(|| Mutex::new(std::collections::VecDeque::new()));
    if let Some(code) = results.lock().unwrap().pop_front() {
        return Some(code);
    }
    if CALL_ACTIVE.swap(true, Ordering::SeqCst) {
        return None;
    }
    let Some(st) = app.try_state::<AndroidGallery<tauri::Wry>>() else {
        CALL_ACTIVE.store(false, Ordering::SeqCst);
        return None;
    };
    let plugin = st.0.clone();
    tauri::async_runtime::spawn(async move {
        struct CallGuard(&'static AtomicBool);
        impl Drop for CallGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        // 无论超时、回调丢失还是生命周期 panic 展开，都允许下一轮重试。
        let _guard = CallGuard(&CALL_ACTIVE);
        if let Ok(Ok(res)) = tokio::time::timeout(
            Duration::from_secs(2),
            plugin.run_mobile_plugin_async::<Res>("takeOtp", Empty {}),
        )
        .await
        {
            if !res.empty {
                if let Some(code) = res.text {
                    if let Some(queue) = RESULTS.get() {
                        queue.lock().unwrap().push_back(code);
                    }
                }
            }
        }
    });
    None
}

#[cfg(not(target_os = "android"))]
fn take_notification_otp(_app: &AppHandle) -> Option<String> {
    None
}

/// 读取本机剪贴板文本：前台直接读；Android 后台读不到时回退到无障碍服务缓存
fn read_local_clipboard(app: &AppHandle) -> Option<String> {
    if let Ok(t) = app.clipboard().read_text() {
        if !t.is_empty() {
            return Some(t);
        }
    }
    take_background_clipboard(app)
}

#[cfg(target_os = "android")]
fn save_png_to_gallery(app: &AppHandle, png: &[u8]) -> Result<String, String> {
    #[derive(serde::Serialize)]
    struct Args {
        base64: String,
        name: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Res {
        #[allow(dead_code)]
        uri: String,
        name: String,
    }

    let st = app
        .try_state::<AndroidGallery<tauri::Wry>>()
        .ok_or("相册插件未初始化")?;
    let args = Args {
        base64: base64_encode(png),
        name: None,
    };
    let res: Res = st
        .0
        .run_mobile_plugin("saveToGallery", args)
        .map_err(|e| e.to_string())?;
    Ok(res.name)
}

/// 把图片落到当前设备：
/// - Android：系统剪贴板不支持图片，改为保存到相册（Pictures/ClipSync）
/// - 桌面端：写入系统剪贴板
fn apply_image(app: &AppHandle, bytes: &[u8]) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        let name = save_png_to_gallery(app, bytes)?;
        Ok(format!("已保存到相册：{}", name))
    }
    #[cfg(not(target_os = "android"))]
    {
        let dynimg =
            image::load_from_memory(bytes).map_err(|e| format!("解码图片失败: {}", e))?;
        let rgba = dynimg.to_rgba8();
        let (w, h) = rgba.dimensions();
        let owned = tauri::image::Image::new_owned(rgba.into_raw(), w, h);
        app.clipboard()
            .write_image(&owned)
            .map_err(|e| format!("写入剪贴板失败: {}", e))?;
        Ok("图片已复制到剪贴板".to_string())
    }
}

// ---------- 发送队列（断网重试）----------
const MAX_OUTBOX: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutboxItem {
    /// "text" | "image"
    kind: String,
    /// 文本内容；图片为内容哈希
    content: String,
    /// 图片扩展名
    #[serde(default)]
    ext: String,
    created: u64,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    next_try: u64,
}

fn outbox_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法获取配置目录: {}", e))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {}", e))?;
    Ok(dir.join("outbox.json"))
}

fn load_outbox(app: &AppHandle) -> Vec<OutboxItem> {
    let Ok(p) = outbox_path(app) else {
        return Vec::new();
    };
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<OutboxItem>>(&t).ok())
        .unwrap_or_default()
}

fn save_outbox(app: &AppHandle, items: &[OutboxItem]) {
    if let Ok(p) = outbox_path(app) {
        if let Ok(data) = serde_json::to_string(items) {
            let _ = std::fs::write(p, data);
        }
    }
}

/// 待发送图片的暂存目录（避免丢内容）
fn outbox_payload_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    let base = app.path().app_data_dir().ok()?;
    let dir = base.join("outbox");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

fn outbox_payload_path(app: &AppHandle, content: &str, ext: &str) -> Option<std::path::PathBuf> {
    Some(outbox_payload_dir(app)?.join(format!("{}.{}", content, ext)))
}

#[allow(dead_code)]
fn outbox_len(state: &SyncState) -> usize {
    state.outbox.lock().unwrap().len()
}

/// 入队（同内容去重；超上限丢最旧的）
fn enqueue(app: &AppHandle, state: &SyncState, mut item: OutboxItem) {
    item.created = now_ms();
    item.next_try = 0;
    item.attempts = 0;
    let mut q = state.outbox.lock().unwrap();
    q.retain(|i| !(i.kind == item.kind && i.content == item.content));
    q.push(item);
    while q.len() > MAX_OUTBOX {
        q.remove(0);
    }
    let snapshot = q.clone();
    drop(q);
    save_outbox(app, &snapshot);
}

// ---------- 敏感信息存储（macOS Keychain / Android Keystore）----------
const SECRET_SERVICE: &str = "com.wyj.clipsync";

/// 进程内缓存，避免每个同步周期都去调用系统钥匙串
static PASSWORD_CACHE: Mutex<Option<String>> = Mutex::new(None);

#[cfg(target_os = "macos")]
fn secret_store(app: &AppHandle, value: &str) -> Result<(), String> {
    let _ = app;
    let out = std::process::Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            SECRET_SERVICE,
            "-a",
            "webdav",
            "-w",
            value,
        ])
        .output()
        .map_err(|e| format!("写入钥匙串失败: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(target_os = "macos")]
fn secret_read(app: &AppHandle) -> String {
    let _ = app;
    let out = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            SECRET_SERVICE,
            "-a",
            "webdav",
            "-w",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

#[cfg(target_os = "android")]
fn secret_store(app: &AppHandle, value: &str) -> Result<(), String> {
    #[derive(serde::Serialize)]
    struct Args {
        key: String,
        value: String,
    }
    let st = app
        .try_state::<AndroidGallery<tauri::Wry>>()
        .ok_or("插件未初始化")?;
    st.0
        .run_mobile_plugin::<serde_json::Value>(
            "setSecret",
            Args {
                key: "webdav".into(),
                value: value.into(),
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
fn secret_read(app: &AppHandle) -> String {
    #[derive(serde::Serialize)]
    struct Args {
        key: String,
    }
    #[derive(serde::Deserialize)]
    struct Res {
        #[serde(default)]
        value: String,
    }
    let Some(st) = app.try_state::<AndroidGallery<tauri::Wry>>() else {
        return String::new();
    };
    match st.0.run_mobile_plugin::<Res>(
        "getSecret",
        Args {
            key: "webdav".into(),
        },
    ) {
        Ok(r) => r.value,
        Err(_) => String::new(),
    }
}

fn password_set(app: &AppHandle, password: &str) -> Result<(), String> {
    secret_store(app, password)?;
    *PASSWORD_CACHE.lock().unwrap() = Some(password.to_string());
    Ok(())
}

fn password_get(app: &AppHandle) -> String {
    {
        let cache = PASSWORD_CACHE.lock().unwrap();
        if let Some(p) = cache.as_ref() {
            return p.clone();
        }
    }
    let p = secret_read(app);
    *PASSWORD_CACHE.lock().unwrap() = Some(p.clone());
    p
}

// ---------- 配置持久化 ----------
fn config_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法获取配置目录: {}", e))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {}", e))?;
    Ok(dir.join("config.json"))
}

fn load_config(app: &AppHandle) -> Result<Config, String> {
    let path = config_path(app)?;
    let mut cfg = if path.exists() {
        let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&s).map_err(|e| format!("配置解析失败: {}", e))?
    } else {
        Config::default()
    };

    if !cfg.password.is_empty() {
        // 旧版本明文密码 -> 迁移到系统安全存储，并从配置文件中清除
        let legacy = cfg.password.clone();
        let _ = password_set(app, &legacy);
        let mut cleaned = cfg.clone();
        cleaned.password.clear();
        let _ = save_config_inner(app, &cleaned);
        cfg.password = legacy;
    } else {
        cfg.password = password_get(app);
    }
    Ok(cfg)
}

fn save_config_inner(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let path = config_path(app)?;
    let s = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, s).map_err(|e| e.to_string())
}

// ---------- WebDAV ----------
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn basic_auth_header(user: &str, pass: &str) -> String {
    let cred = format!("{}:{}", user, pass);
    format!("Basic {}", base64_encode(cred.as_bytes()))
}

fn remote_url(cfg: &Config) -> String {
    format!("{}{}", cfg.webdav_url.trim_end_matches('/'), SYNC_FILE)
}

fn push_remote(cfg: &Config, rc: &RemoteClip) -> Result<(), String> {
    let body = serde_json::to_string(rc).map_err(|e| e.to_string())?;
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    let resp = ureq::put(&remote_url(cfg))
        .set("Authorization", &auth)
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| format!("上传失败: {}", e))?;
    if (200..300).contains(&resp.status()) {
        Ok(())
    } else {
        Err(format!("上传失败: HTTP {}", resp.status()))
    }
}

fn pull_remote(cfg: &Config) -> Result<Option<RemoteClip>, String> {
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    let resp = match ureq::get(&remote_url(cfg))
        .set("Authorization", &auth)
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(ureq::Error::Status(code, _)) => return Err(format!("下载失败: HTTP {}", code)),
        Err(e) => return Err(format!("下载失败: {}", e)),
    };
    let body = resp
        .into_string()
        .map_err(|e| format!("读取响应失败: {}", e))?;
    let rc: RemoteClip =
        serde_json::from_str(&body).map_err(|e| format!("解析远端数据失败: {}", e))?;
    Ok(Some(rc))
}

// ---------- 收藏同步（WebDAV）----------
const FAV_FILE: &str = "/clipsync/favorites.json";

fn fav_remote_url(cfg: &Config) -> String {
    format!("{}{}", cfg.webdav_url.trim_end_matches('/'), FAV_FILE)
}

fn dav_get_text(cfg: &Config, url: &str) -> Result<Option<String>, String> {
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    match ureq::get(url).set("Authorization", &auth).call() {
        Ok(resp) => resp
            .into_string()
            .map(Some)
            .map_err(|e| format!("读取响应失败: {}", e)),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => Err(format!("下载失败: HTTP {}", code)),
        Err(e) => Err(format!("下载失败: {}", e)),
    }
}

fn dav_put_text(cfg: &Config, url: &str, body: &str) -> Result<(), String> {
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    let resp = ureq::put(url)
        .set("Authorization", &auth)
        .set("Content-Type", "application/json")
        .send_string(body)
        .map_err(|e| format!("上传失败: {}", e))?;
    if (200..300).contains(&resp.status()) {
        Ok(())
    } else {
        Err(format!("上传失败: HTTP {}", resp.status()))
    }
}

/// 与远端合并收藏：删除墓碑优先，其余取并集；有变化则回写
fn sync_favorites(app: &AppHandle, state: &SyncState, cfg: &Config) -> Result<(), String> {
    let local = state.favorites.lock().unwrap().clone();
    let url = fav_remote_url(cfg);

    let remote_text = dav_get_text(cfg, &url)?;
    let remote: FavDoc = remote_text
        .as_deref()
        .and_then(|t| serde_json::from_str::<FavDoc>(t).ok())
        .unwrap_or_default();

    let mut merged = local.merge(&remote);
    merged.prune_tombstones();

    let merged_json =
        serde_json::to_string_pretty(&merged).map_err(|e| format!("序列化收藏失败: {}", e))?;
    let remote_json = serde_json::to_string_pretty(&remote).unwrap_or_default();

    let local_changed = merged_json != serde_json::to_string_pretty(&local).unwrap_or_default();

    if local_changed {
        *state.favorites.lock().unwrap() = merged.clone();
        save_favorites(app, &merged)?;
        let _ = app.emit("sync-history", ());
    }

    // 远端落后则回写
    if merged_json != remote_json {
        if merged.favorites.is_empty() && merged.deleted.is_empty() {
            // 两端都为空，不创建空文件
        } else {
            dav_put_text(cfg, &url, &merged_json)?;
        }
    }

    Ok(())
}

// ---------- 图片同步（WebDAV，多图历史）----------
const IMG_DIR: &str = "/clipsync/images";
const THUMB_PX: u32 = 96;
const MAX_IMAGES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageItem {
    hash: String,
    /// 文件扩展名：png / jpg
    #[serde(default = "default_ext")]
    ext: String,
    device_id: String,
    width: u32,
    height: u32,
    size: u64,
    time: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageIndex {
    #[serde(default)]
    items: Vec<ImageItem>,
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{:016x}", h)
}

fn img_item_url(cfg: &Config, hash: &str) -> String {
    img_item_url_ext(cfg, hash, "png")
}

fn img_item_url_ext(cfg: &Config, hash: &str, ext: &str) -> String {
    format!(
        "{}{}/{}.{}",
        cfg.webdav_url.trim_end_matches('/'),
        IMG_DIR,
        hash,
        ext
    )
}

/// 按内容哈希在索引里找条目，返回 (ext, 是否存在)
fn index_ext_of(state: &SyncState, hash: &str) -> String {
    state
        .local_images
        .lock()
        .unwrap()
        .items
        .iter()
        .find(|i| i.hash == hash)
        .map(|i| i.ext.clone())
        .unwrap_or_else(|| "png".to_string())
}

fn img_index_url(cfg: &Config) -> String {
    format!(
        "{}{}/index.json",
        cfg.webdav_url.trim_end_matches('/'),
        IMG_DIR
    )
}

fn dav_delete(cfg: &Config, url: &str) -> Result<(), String> {
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    match ureq::request("DELETE", url)
        .set("Authorization", &auth)
        .call()
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(format!("删除失败: HTTP {}", code)),
        Err(e) => Err(format!("删除失败: {}", e)),
    }
}

fn dav_put_bytes(cfg: &Config, url: &str, body: &[u8], ctype: &str) -> Result<(), String> {
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    let resp = ureq::put(url)
        .set("Authorization", &auth)
        .set("Content-Type", ctype)
        .send_bytes(body)
        .map_err(|e| format!("上传失败: {}", e))?;
    if (200..300).contains(&resp.status()) {
        Ok(())
    } else {
        Err(format!("上传失败: HTTP {}", resp.status()))
    }
}

fn dav_get_bytes(cfg: &Config, url: &str) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    match ureq::get(url).set("Authorization", &auth).call() {
        Ok(resp) => {
            let mut buf = Vec::new();
            resp.into_reader()
                .read_to_end(&mut buf)
                .map_err(|e| format!("读取图片失败: {}", e))?;
            Ok(Some(buf))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => Err(format!("下载失败: HTTP {}", code)),
        Err(e) => Err(format!("下载失败: {}", e)),
    }
}

/// 确保远端目录存在（WebDAV 不会自动建目录，PUT 进不存在的目录会 403）
fn dav_mkcol(cfg: &Config, url: &str) -> Result<(), String> {
    let auth = basic_auth_header(&cfg.username, &cfg.password);
    match ureq::request("MKCOL", url)
        .set("Authorization", &auth)
        .call()
    {
        Ok(_) => Ok(()),
        // 已存在：不同服务端返回 405 / 301 / 409
        Err(ureq::Error::Status(405, _))
        | Err(ureq::Error::Status(301, _))
        | Err(ureq::Error::Status(409, _)) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(format!("创建目录失败: HTTP {}", code)),
        Err(e) => Err(format!("创建目录失败: {}", e)),
    }
}

fn ensure_remote_dirs(cfg: &Config) {
    let base = cfg.webdav_url.trim_end_matches('/');
    let _ = dav_mkcol(cfg, &format!("{}/clipsync", base));
    let _ = dav_mkcol(cfg, &format!("{}/clipsync/images", base));
}

fn images_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    let base = app
        .path()
        .app_data_dir()
        .or_else(|_| app.path().app_config_dir())
        .ok()?;
    let dir = base.join("images");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

fn cache_png_path(app: &AppHandle, hash: &str) -> Option<std::path::PathBuf> {
    Some(images_dir(app)?.join(format!("{}.png", hash)))
}

fn images_index_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    let base = app
        .path()
        .app_config_dir()
        .ok()?;
    let _ = std::fs::create_dir_all(&base);
    Some(base.join("images-index.json"))
}

fn load_local_images(app: &AppHandle) -> ImageIndex {
    images_index_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<ImageIndex>(&t).ok())
        .unwrap_or_default()
}

fn save_local_images(app: &AppHandle, idx: &ImageIndex) {
    if let Some(p) = images_index_path(app) {
        if let Ok(data) = serde_json::to_string(idx) {
            let _ = std::fs::write(p, data);
        }
    }
}

const MAX_IMG_SIDE: u32 = 2560;

/// 上传前处理：长边缩到 2560；无透明通道用 JPEG(q85)，有透明保留 PNG
fn encode_for_upload(raw: &[u8], w: u32, h: u32) -> Result<(Vec<u8>, String), String> {
    use image::{DynamicImage, ImageEncoder, RgbaImage};

    if w == 0 || h == 0 {
        return Err("图片尺寸无效".to_string());
    }
    let img = RgbaImage::from_raw(w, h, raw.to_vec()).ok_or_else(|| "图片数据不完整".to_string())?;
    let dynimg = DynamicImage::ImageRgba8(img);

    let scale = (MAX_IMG_SIDE as f32 / w.max(h) as f32).min(1.0);
    let resized = if scale < 1.0 {
        let nw = ((w as f32 * scale) as u32).max(1);
        let nh = ((h as f32 * scale) as u32).max(1);
        dynimg.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        dynimg
    };

    let rgba = resized.to_rgba8();
    let has_alpha = rgba.pixels().any(|p| p[3] < 255);

    let mut out: Vec<u8> = Vec::new();
    if has_alpha {
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("PNG 编码失败: {}", e))?;
        Ok((out, "png".to_string()))
    } else {
        let rgb = resized.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| format!("JPEG 编码失败: {}", e))?;
        Ok((out, "jpg".to_string()))
    }
}

fn decode_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    image::load_from_memory(data)
        .ok()
        .map(|img| (img.width(), img.height()))
}

fn cache_image_path(app: &AppHandle, hash: &str, ext: &str) -> Option<std::path::PathBuf> {
    Some(images_dir(app)?.join(format!("{}.{}", hash, ext)))
}

fn make_thumb_data_url(png: &[u8]) -> Option<String> {
    use image::ImageEncoder;
    let img = image::load_from_memory(png).ok()?;
    let thumb = img.thumbnail(THUMB_PX, THUMB_PX);
    let rgba = thumb.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out: Vec<u8> = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(format!("data:image/png;base64,{}", base64_encode(&out)))
}

/// 清理超过 7 天或超出上限的图片（同时删除本地缓存文件）
fn prune_images(app: &AppHandle, state: &SyncState, cfg: &Config) -> bool {
    let cutoff = now_ms().saturating_sub(RETENTION_MS);
    let mut idx = state.local_images.lock().unwrap().clone();
    let before = idx.items.len();

    // 收集要删除的条目（超过 7 天，或超出数量上限的最旧部分）
    let mut removed: Vec<ImageItem> = Vec::new();
    idx.items.retain(|i| {
        if i.time < cutoff {
            removed.push(i.clone());
            false
        } else {
            true
        }
    });
    idx.items.sort_by(|a, b| b.time.cmp(&a.time));
    if idx.items.len() > MAX_IMAGES {
        for it in idx.items.split_off(MAX_IMAGES) {
            removed.push(it);
        }
    }

    if before == idx.items.len() {
        return false;
    }

    *state.local_images.lock().unwrap() = idx.clone();
    save_local_images(app, &idx);

    // 本地缓存 + 云端文件一起清理，避免 NAS 无限增长
    for it in &removed {
        if let Some(p) = cache_png_path(app, &it.hash) {
            let _ = std::fs::remove_file(p);
        }
        let _ = dav_delete(cfg, &img_item_url_ext(cfg, &it.hash, &it.ext));
    }

    true
}

fn sync_images(
    app: &AppHandle,
    state: &SyncState,
    cfg: &Config,
    clip_changed: bool,
) -> Result<(), String> {
    let mut index_changed = false;

    // A) 本机剪贴板新图片 -> 上传（剪贴板没变化就不去解码图片）
    let suppress = *state.img_suppress_until.lock().unwrap() > now_ms();
    if clip_changed && !suppress {
        if let Ok(img) = app.clipboard().read_image() {
            let w = img.width();
            let h = img.height();
            let raw = img.rgba().to_vec();
            let (data, ext) = encode_for_upload(&raw, w, h)?;
            let hash = fnv1a(&data);
            let known = state
                .local_images
                .lock()
                .unwrap()
                .items
                .iter()
                .any(|i| i.hash == hash);

            if !known && w > 0 && h > 0 {
                // 先把图片暂存到本地，再入队；真正上传成功后才写索引
                if let Some(p) = outbox_payload_path(app, &hash, &ext) {
                    let _ = std::fs::write(p, &data);
                }
                enqueue(
                    app,
                    state,
                    OutboxItem {
                        kind: "image".to_string(),
                        content: hash.clone(),
                        ext: ext.clone(),
                        created: now_ms(),
                        attempts: 0,
                        next_try: 0,
                    },
                );

                *state.last_local_image.lock().unwrap() = Some(hash.clone());
                *state.last_applied_image.lock().unwrap() = Some(hash);
                *state.img_suppress_until.lock().unwrap() = now_ms() + 4000;
                *state.last_sync_at.lock().unwrap() = Some(now_ms());
                let _ = app.emit("sync-status", status_snapshot(state));
                index_changed = true;
            }
        }
    }

    // B) 合并远端清单，下载其它设备的新图
    if let Some(text) = dav_get_text(cfg, &img_index_url(cfg))? {
        if let Ok(remote) = serde_json::from_str::<ImageIndex>(&text) {
            let mut local = state.local_images.lock().unwrap().clone();
            let mut incoming: Vec<ImageItem> = Vec::new();

            for it in &remote.items {
                if it.hash.is_empty() {
                    continue;
                }
                if !local.items.iter().any(|x| x.hash == it.hash) {
                    local.items.push(it.clone());
                    index_changed = true;
                    if it.device_id != cfg.device_id {
                        incoming.push(it.clone());
                    }
                }
            }

            if !incoming.is_empty() {
                *state.local_images.lock().unwrap() = local.clone();
                save_local_images(app, &local);
                incoming.sort_by(|a, b| b.time.cmp(&a.time));

                for (pos, it) in incoming.iter().enumerate() {
                    let Some(bytes) =
                        dav_get_bytes(cfg, &img_item_url_ext(cfg, &it.hash, &it.ext))?
                    else {
                        continue;
                    };
                    if let Some(p) = cache_image_path(app, &it.hash, &it.ext) {
                        let _ = std::fs::write(p, &bytes);
                    }
                    let thumb = make_thumb_data_url(&bytes);

                    // 只把最新的一张落到设备（Mac 写剪贴板 / Android 存相册），其余仅入历史
                    if pos == 0 && apply_image(app, &bytes).is_ok() {
                        *state.last_applied_image.lock().unwrap() = Some(it.hash.clone());
                        *state.last_local_image.lock().unwrap() = Some(it.hash.clone());
                        *state.img_suppress_until.lock().unwrap() = now_ms() + 4000;
                        *state.last_sync_at.lock().unwrap() = Some(now_ms());
                        let _ = app.emit("sync-status", status_snapshot(state));
                    }

                    state.push_image_history(
                        app,
                        "down",
                        &it.hash,
                        &format!("[图片 {}×{} · {} KB]", it.width, it.height, it.size / 1024),
                        thumb,
                    );
                }
            }
        }
    }

    // C) 清理过期图片
    if prune_images(app, state, cfg) {
        index_changed = true;
    }

    // D) 回写清单
    if index_changed {
        let idx = state.local_images.lock().unwrap().clone();
        ensure_remote_dirs(cfg);
        let body = serde_json::to_string(&idx).unwrap_or_default();
        let _ = dav_put_text(cfg, &img_index_url(cfg), &body);
    }

    Ok(())
}

// ---------- 队列发送 ----------
fn send_queued_image(
    app: &AppHandle,
    state: &SyncState,
    cfg: &Config,
    item: &OutboxItem,
) -> Result<(), String> {
    let path = outbox_payload_path(app, &item.content, &item.ext)
        .ok_or_else(|| "待发送图片路径无效".to_string())?;
    let data = std::fs::read(&path).map_err(|e| format!("读取待发送图片失败: {}", e))?;

    let ctype = if item.ext == "jpg" {
        "image/jpeg"
    } else {
        "image/png"
    };
    ensure_remote_dirs(cfg);
    dav_put_bytes(cfg, &img_item_url_ext(cfg, &item.content, &item.ext), &data, ctype)?;

    let (w, h) = decode_dimensions(&data).unwrap_or((0, 0));
    {
        let mut idx = state.local_images.lock().unwrap();
        if !idx.items.iter().any(|i| i.hash == item.content) {
            idx.items.push(ImageItem {
                hash: item.content.clone(),
                ext: item.ext.clone(),
                device_id: cfg.device_id.clone(),
                width: w,
                height: h,
                size: data.len() as u64,
                time: item.created,
            });
        }
        save_local_images(app, &idx);
        let body = serde_json::to_string(&*idx).unwrap_or_default();
        drop(idx);
        let _ = dav_put_text(cfg, &img_index_url(cfg), &body);
    }

    if let Some(p) = cache_image_path(app, &item.content, &item.ext) {
        let _ = std::fs::write(p, &data);
    }
    let thumb = make_thumb_data_url(&data);
    state.push_image_history(
        app,
        "up",
        &item.content,
        &format!("[图片 {}×{} · {} KB]", w, h, data.len() / 1024),
        thumb,
    );
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// 逐条发送队列；失败按指数退避，保留到下次
fn flush_outbox(app: &AppHandle, state: &SyncState, cfg: &Config) -> Result<(), String> {
    loop {
        let item = {
            let q = state.outbox.lock().unwrap();
            q.iter()
                .find(|i| i.next_try <= now_ms())
                .cloned()
        };
        let Some(item) = item else { break };

        let result = if item.kind == "image" {
            send_queued_image(app, state, cfg, &item)
        } else {
            let rc = RemoteClip {
                device_id: cfg.device_id.clone(),
                content: item.content.clone(),
            };
            push_remote(cfg, &rc)
        };

        match result {
            Ok(()) => {
                {
                    let mut q = state.outbox.lock().unwrap();
                    q.retain(|i| !(i.kind == item.kind && i.content == item.content));
                    let snapshot = q.clone();
                    drop(q);
                    save_outbox(app, &snapshot);
                }
                if item.kind == "text" {
                    *state.last_local_sent.lock().unwrap() = Some(item.content.clone());
                }
                if item.kind != "image" {
                    state.push_history(app, "up", &item.content);
                }
                *state.last_sync_at.lock().unwrap() = Some(now_ms());
                let _ = app.emit("sync-status", status_snapshot(state));
            }
            Err(e) => {
                {
                    let mut q = state.outbox.lock().unwrap();
                    if let Some(it) = q
                        .iter_mut()
                        .find(|i| i.kind == item.kind && i.content == item.content)
                    {
                        it.attempts += 1;
                        let backoff = 2u64.pow(it.attempts.min(5)) * 1000;
                        it.next_try = now_ms() + backoff;
                    }
                    let snapshot = q.clone();
                    drop(q);
                    save_outbox(app, &snapshot);
                }
                let _ = app.emit("sync-status", status_snapshot(state));
                return Err(e);
            }
        }
    }
    Ok(())
}

// ---------- 同步 ----------
fn status_snapshot(state: &SyncState) -> SyncStatus {
    SyncStatus {
        running: state.running.load(Ordering::SeqCst),
        last_error: state.last_error.lock().unwrap().clone(),
        last_sync_at: *state.last_sync_at.lock().unwrap(),
        pending: state.outbox.lock().unwrap().len(),
    }
}

/// 用原子文件替换驱动 Android 状态通知；相同状态不写盘，避免额外耗电。
#[cfg(target_os = "android")]
fn publish_android_status(state: &SyncState) {
    let marker = state.activity_marker.lock().unwrap().clone();
    let Some(marker) = marker else { return };
    let status = status_snapshot(state);
    let body = serde_json::json!({
        "running": status.running,
        "error": status.last_error.unwrap_or_default(),
        "pending": status.pending,
        "activityAlive": marker.exists(),
    })
    .to_string();
    {
        let mut cache = state.native_status_cache.lock().unwrap();
        if cache.as_deref() == Some(body.as_str()) {
            return;
        }
        let path = marker.with_file_name(".sync_status.json");
        let temp = marker.with_file_name(".sync_status.json.tmp");
        if std::fs::write(&temp, &body).is_err() || std::fs::rename(&temp, &path).is_err() {
            return;
        }
        *cache = Some(body);
    }
}

/// 仅由界面命令调用；后台线程不能进入 mobile plugin dispatch。
#[cfg(target_os = "android")]
fn refresh_android_status_service(app: &AppHandle, state: &SyncState) {
    publish_android_status(state);
    if let Some(plugin) = app.try_state::<AndroidGallery<tauri::Wry>>() {
        let _ = plugin.0.run_mobile_plugin::<serde_json::Value>(
            "refreshStatusService",
            serde_json::json!({}),
        );
    }
}

fn sync_once(app: &AppHandle, state: &SyncState) -> Result<(), String> {
    let cfg = load_config(app)?;
    if cfg.webdav_url.trim().is_empty() {
        return Ok(());
    }

    // 先拉一次远端，用于判断「本机内容是否其实已经同步过」
    // 拉取失败（断网等）不阻断上传：本地内容照样入队，等网络恢复后补传
    let remote = match pull_remote(&cfg) {
        Ok(r) => r,
        Err(e) => {
            *state.last_error.lock().unwrap() = Some(e);
            None
        }
    };

    // 剪贴板是否变化（macOS 用 changeCount 判断，避免每轮重复解码图片）
    let clip_changed = match clipboard_change_count() {
        Some(c) => {
            let mut last = state.last_clip_count.lock().unwrap();
            let changed = *last != c;
            *last = c;
            changed
        }
        None => true,
    };
    let first_pass = !state.baseline_done.load(Ordering::SeqCst);

    // 1) 本机剪贴板有变化 -> 上传到 WebDAV
    if clip_changed || first_pass {
    if let Some(local) = read_local_clipboard(app) {
        // 启动后第一次读到剪贴板：作为基线，不当作新内容上传
        let first_read = !state.baseline_done.swap(true, Ordering::SeqCst);
        if first_read {
            *state.last_local_sent.lock().unwrap() = Some(local);
        } else {
            let need_upload = {
                let last_sent = state.last_local_sent.lock().unwrap();
                last_sent.as_deref() != Some(local.as_str())
            };

            if need_upload {
                // 远端已是同样的内容（重启后内存去重状态丢失）-> 视为已同步
                let already_on_remote = remote
                    .as_ref()
                    .map(|r| r.content == local)
                    .unwrap_or(false);

                if already_on_remote {
                    *state.last_local_sent.lock().unwrap() = Some(local);
                } else {
                    // 先入队，由 flush 负责真正发送（失败会自动退避重试）
                    *state.last_local_sent.lock().unwrap() = Some(local.clone());
                    enqueue(
                        app,
                        state,
                        OutboxItem {
                            kind: "text".to_string(),
                            content: local,
                            ext: String::new(),
                            created: now_ms(),
                            attempts: 0,
                            next_try: 0,
                        },
                    );
                    let _ = app.emit("sync-status", status_snapshot(state));
                }
            }
        }
    }
    }

    // 2) 远端来自其它设备 -> 写入本机剪贴板
    if let Some(rc) = remote {
        if rc.device_id != cfg.device_id {
            let needs_apply = {
                let last_applied = state.last_applied_remote.lock().unwrap();
                last_applied.as_deref() != Some(rc.content.as_str())
            };
            // 本机剪贴板已经是这段内容 -> 只同步状态，不重复记录
            let already_local = read_local_clipboard(app)
                .map(|t| t == rc.content)
                .unwrap_or(false);

            if needs_apply {
                if already_local {
                    *state.last_applied_remote.lock().unwrap() = Some(rc.content.clone());
                    *state.last_local_sent.lock().unwrap() = Some(rc.content);
                } else if app.clipboard().write_text(rc.content.clone()).is_ok() {
                    *state.last_applied_remote.lock().unwrap() = Some(rc.content.clone());
                    *state.last_local_sent.lock().unwrap() = Some(rc.content.clone());
                    *state.last_sync_at.lock().unwrap() = Some(now_ms());
                    state.push_history(app, "down", &rc.content);
                    let _ = app.emit("sync-status", status_snapshot(state));
                }
            }
        }
    }

    // 验证码在本轮最后入队，确保任何待发送/新剪贴板文本都先上传，OTP 保持为最终远端内容
    while let Some(code) = take_notification_otp(app) {
        enqueue(
            app,
            state,
            OutboxItem {
                kind: "otp".to_string(),
                content: code,
                ext: String::new(),
                created: now_ms(),
                attempts: 0,
                next_try: 0,
            },
        );
    }
    let _ = app.emit("sync-status", status_snapshot(state));

    // 3) 收藏同步（独立节流，失败不影响剪贴板同步）
    let due = {
        let mut last = state.last_fav_sync.lock().unwrap();
        let now = now_ms();
        if now.saturating_sub(*last) >= FAV_SYNC_INTERVAL_MS {
            *last = now;
            true
        } else {
            false
        }
    };
    if due {
        let _ = sync_favorites(app, state, &cfg);
    }

    // 4) 图片同步（失败不影响文字同步）
    let _ = sync_images(app, state, &cfg, clip_changed);

    // 5) 发送队列：断网时保留，恢复后按退避补传
    if let Err(e) = flush_outbox(app, state, &cfg) {
        *state.last_error.lock().unwrap() = Some(e);
    }

    Ok(())
}

fn spawn_sync_loop(app: AppHandle, state: SyncState) {
    thread::spawn(move || loop {
        if !state.running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(300));
            continue;
        }
        #[cfg(target_os = "android")]
        {
            let activity_alive = state
                .activity_marker
                .lock()
                .unwrap()
                .as_ref()
                .map(|path| path.exists())
                .unwrap_or(false);
            if !activity_alive {
                publish_android_status(&state);
                thread::sleep(Duration::from_millis(300));
                continue;
            }
        }
        // 防御 Activity 检查与插件派发之间的生命周期竞态，单次异常不能杀死同步线程。
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sync_once(&app, &state)
        })) {
            Ok(result) => result,
            Err(_) => Err("系统界面已重建，同步将在下一轮自动恢复".to_string()),
        };
        match outcome {
            Ok(()) => {
                let had_error = {
                    let mut err = state.last_error.lock().unwrap();
                    let had = err.is_some();
                    if had {
                        *err = None;
                    }
                    had
                };
                if had_error {
                    let _ = app.emit("sync-status", status_snapshot(&state));
                }
            }
            Err(e) => {
                *state.last_error.lock().unwrap() = Some(e.clone());
                let _ = app.emit("sync-status", status_snapshot(&state));
            }
        }
        #[cfg(target_os = "android")]
        publish_android_status(&state);
        // 自适应间隔：最近有同步就快一点，长时间空闲则放慢，减少对 NAS 的请求
        let idle_ms = {
            let last = *state.last_sync_at.lock().unwrap();
            match last {
                Some(t) => now_ms().saturating_sub(t),
                None => u64::MAX,
            }
        };
        let interval = if idle_ms > 120_000 {
            6000
        } else if idle_ms > 30_000 {
            3000
        } else {
            POLL_INTERVAL_MS
        };
        thread::sleep(Duration::from_millis(interval));
    });
}

// ---------- Tauri 命令 ----------
#[tauri::command]
fn get_config(app: AppHandle) -> Result<Config, String> {
    load_config(&app)
}

#[tauri::command]
fn save_config(
    app: AppHandle,
    webdav_url: String,
    username: String,
    password: String,
    hotkey: Option<String>,
) -> Result<(), String> {
    let mut cfg = load_config(&app)?;
    cfg.webdav_url = webdav_url.trim().to_string();
    cfg.username = username.trim().to_string();
    if !password.is_empty() {
        password_set(&app, &password)?;
    }
    cfg.password = String::new();
    if let Some(hk) = hotkey {
        cfg.hotkey = hk.trim().to_string();
    }
    save_config_inner(&app, &cfg)?;

    #[cfg(desktop)]
    {
        apply_hotkey(&app, &cfg.hotkey)?;
    }
    Ok(())
}

#[tauri::command]
fn start_sync(app: AppHandle, state: State<'_, SyncState>) -> Result<(), String> {
    let cfg = load_config(&app)?;
    if cfg.webdav_url.trim().is_empty() {
        return Err("请先填写 WebDAV 地址并保存".into());
    }
    state.running.store(true, Ordering::SeqCst);
    #[cfg(target_os = "android")]
    refresh_android_status_service(&app, &state);
    Ok(())
}

#[tauri::command]
fn stop_sync(_app: AppHandle, state: State<'_, SyncState>) -> Result<(), String> {
    state.running.store(false, Ordering::SeqCst);
    #[cfg(target_os = "android")]
    refresh_android_status_service(&_app, &state);
    Ok(())
}

#[tauri::command]
fn get_status(state: State<'_, SyncState>) -> SyncStatus {
    status_snapshot(state.inner())
}

#[tauri::command]
fn get_history(state: State<'_, SyncState>) -> Vec<Entry> {
    state.prune_history();
    state.history.lock().unwrap().iter().cloned().collect()
}

#[tauri::command]
fn get_favorites(state: State<'_, SyncState>) -> Vec<Entry> {
    state.favorites.lock().unwrap().favorites.clone()
}

/// 切换收藏状态，返回切换后是否为「已收藏」
#[tauri::command]
fn toggle_favorite(
    app: AppHandle,
    state: State<'_, SyncState>,
    id: String,
) -> Result<bool, String> {
    // 已收藏 -> 取消（记墓碑，避免其它端再合并回来）
    {
        let mut doc = state.favorites.lock().unwrap();
        if doc.remove_favorite(&id) {
            let snapshot = doc.clone();
            drop(doc);
            save_favorites(&app, &snapshot)?;
            // 立刻把变更推到云端，其它端尽快看到
            *state.last_fav_sync.lock().unwrap() = 0;
            let _ = app.emit("sync-history", ());
            return Ok(false);
        }
    }

    // 未收藏 -> 从最近记录里取一条加入
    let entry = {
        let h = state.history.lock().unwrap();
        match h.iter().find(|e| e.id == id) {
            Some(e) => e.clone(),
            None => return Err("记录已过期，无法收藏".into()),
        }
    };

    let snapshot = {
        let mut doc = state.favorites.lock().unwrap();
        doc.deleted.retain(|t| t.id != id);
        if !doc.favorites.iter().any(|e| e.id == id) {
            doc.favorites.push(entry);
        }
        doc.favorites.sort_by(|a, b| b.time.cmp(&a.time));
        doc.favorites.truncate(MAX_FAVORITES);
        doc.clone()
    };
    save_favorites(&app, &snapshot)?;
    *state.last_fav_sync.lock().unwrap() = 0;
    let _ = app.emit("sync-history", ());
    Ok(true)
}

/// 把历史里的某张图片重新复制到剪贴板（本地缓存优先，缺失则从云端下载）
#[tauri::command]
fn copy_image(
    app: AppHandle,
    state: State<'_, SyncState>,
    hash: String,
) -> Result<String, String> {
    let cfg = load_config(&app)?;
    if cfg.webdav_url.trim().is_empty() {
        return Err("未配置 WebDAV".into());
    }

    let ext = index_ext_of(&state, &hash);
    let bytes = if let Some(path) = cache_image_path(&app, &hash, &ext) {
        match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                let ext = index_ext_of(&state, &hash);
                dav_get_bytes(&cfg, &img_item_url_ext(&cfg, &hash, &ext))?
                    .ok_or_else(|| "云端也找不到这张图片".to_string())?
            }
        }
    } else {
        let ext = index_ext_of(&state, &hash);
        dav_get_bytes(&cfg, &img_item_url_ext(&cfg, &hash, &ext))?
            .ok_or_else(|| "云端也找不到这张图片".to_string())?
    };

    let msg = apply_image(&app, &bytes)?;

    *state.last_applied_image.lock().unwrap() = Some(hash.clone());
    *state.last_local_image.lock().unwrap() = Some(hash);
    *state.img_suppress_until.lock().unwrap() = now_ms() + 4000;
    Ok(msg)
}

/// 标记记录「刚被使用」：挪到列表最前（同时同步到收藏列表与磁盘）
#[tauri::command]
fn touch_entry(app: AppHandle, state: State<'_, SyncState>, id: String) -> Result<(), String> {
    let now = now_ms();

    {
        let mut h = state.history.lock().unwrap();
        if let Some(pos) = h.iter().position(|e| e.id == id) {
            if pos != 0 {
                let mut entry = h.remove(pos).unwrap();
                entry.used_at = now;
                h.push_front(entry);
                save_history(&app, &h);
            } else if let Some(front) = h.front_mut() {
                front.used_at = now;
                let snapshot = h.clone();
                save_history(&app, &snapshot);
            }
        }
    }

    {
        let mut doc = state.favorites.lock().unwrap();
        if let Some(pos) = doc.favorites.iter().position(|e| e.id == id) {
            let mut entry = doc.favorites.remove(pos);
            entry.used_at = now;
            doc.favorites.insert(0, entry);
            let snapshot = doc.clone();
            drop(doc);
            save_favorites(&app, &snapshot)?;
        }
    }

    let _ = app.emit("sync-history", ());
    Ok(())
}

/// 把指定记录粘贴到「上一个应用」的光标处（仅 macOS）
#[tauri::command]
fn paste_entry(app: AppHandle, state: State<'_, SyncState>, id: String) -> Result<(), String> {
    paste_entry_impl(app, state, id)
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

#[cfg(target_os = "macos")]
fn paste_log(msg: &str) {
    use std::io::Write;
    if let Some(home) = std::env::var_os("HOME") {
        let dir = std::path::PathBuf::from(home).join("Library/Logs/ClipSync");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("paste.log"))
        {
            let _ = writeln!(f, "[{}] {}", now_ms(), msg);
        }
    }
}

/// macOS 剪贴板变更计数；未变化时同值，可跳过昂贵的图片解码
#[cfg(target_os = "macos")]
fn clipboard_change_count() -> Option<i64> {
    use objc2_app_kit::NSPasteboard;
    let pb = NSPasteboard::generalPasteboard();
    Some(pb.changeCount() as i64)
}

#[cfg(not(target_os = "macos"))]
fn clipboard_change_count() -> Option<i64> {
    None
}

#[cfg(target_os = "macos")]
static LAST_EXTERNAL_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// 记下当前最前面的「别的 App」，供粘贴时把焦点还回去
#[cfg(target_os = "macos")]
fn remember_frontmost_app() {
    use objc2_app_kit::NSWorkspace;
    let ws = NSWorkspace::sharedWorkspace();
    if let Some(app) = ws.frontmostApplication() {
        let pid = app.processIdentifier();
        if pid > 0 && pid != std::process::id() as i32 {
            LAST_EXTERNAL_PID.store(pid, Ordering::SeqCst);
        }
    }
}

/// 激活之前记录的 App
#[cfg(target_os = "macos")]
fn activate_remembered_app() -> bool {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    let pid = LAST_EXTERNAL_PID.load(Ordering::SeqCst);
    if pid <= 0 {
        return false;
    }
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        return app.activateWithOptions(NSApplicationActivationOptions::empty());
    }
    false
}

#[cfg(target_os = "macos")]
fn is_accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

#[cfg(target_os = "macos")]
fn open_macos_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

/// 直接发送 ⌘V（CGEvent，只需「辅助功能」权限）
#[cfg(target_os = "macos")]
fn send_paste_shortcut() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    const KEY_V: u16 = 9; // kVK_ANSI_V

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "无法创建事件源".to_string())?;

    let down = CGEvent::new_keyboard_event(source.clone(), KEY_V, true)
        .map_err(|_| "创建按键事件失败".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    std::thread::sleep(std::time::Duration::from_millis(40));

    let up = CGEvent::new_keyboard_event(source, KEY_V, false)
        .map_err(|_| "创建按键事件失败".to_string())?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);

    Ok(())
}

#[cfg(target_os = "macos")]
fn paste_entry_impl(app: AppHandle, state: State<'_, SyncState>, id: String) -> Result<(), String> {
    // 1) 找到要粘贴的记录（最近列表或收藏列表）
    let entry = {
        let h = state.history.lock().unwrap();
        h.iter().find(|e| e.id == id).cloned()
    }
    .or_else(|| {
        let f = state.favorites.lock().unwrap();
        f.favorites.iter().find(|e| e.id == id).cloned()
    })
    .ok_or_else(|| "记录不存在".to_string())?;

    // 2) 写入剪贴板
    if entry.kind == "image" {
        let cfg = load_config(&app)?;
        let hash = entry.content.clone();
        let ext0 = index_ext_of(&state, &hash);
        let mut bytes = cache_image_path(&app, &hash, &ext0)
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        if bytes.is_empty() {
            let ext = index_ext_of(&state, &hash);
            bytes = dav_get_bytes(&cfg, &img_item_url_ext(&cfg, &hash, &ext))?
                .ok_or_else(|| "云端找不到这张图片".to_string())?;
        }
        let dynimg = image::load_from_memory(&bytes).map_err(|e| format!("解码失败: {}", e))?;
        let rgba = dynimg.to_rgba8();
        let (w, h) = rgba.dimensions();
        let owned = tauri::image::Image::new_owned(rgba.into_raw(), w, h);
        app.clipboard()
            .write_image(&owned)
            .map_err(|e| format!("写入剪贴板失败: {}", e))?;
        *state.last_local_image.lock().unwrap() = Some(hash.clone());
        *state.last_applied_image.lock().unwrap() = Some(hash);
        *state.img_suppress_until.lock().unwrap() = now_ms() + 4000;
    } else {
        let text = if entry.content.is_empty() {
            entry.preview.clone()
        } else {
            entry.content.clone()
        };
        app.clipboard()
            .write_text(text.clone())
            .map_err(|e| format!("写入剪贴板失败: {}", e))?;
        *state.last_local_sent.lock().unwrap() = Some(text);
    }

    paste_log(&format!(
        "准备粘贴 kind={} trusted={}",
        entry.kind,
        is_accessibility_trusted()
    ));

    // 3) 隐藏窗口并把焦点还给之前那个 App
    let _ = app.hide();
    std::thread::sleep(std::time::Duration::from_millis(120));
    let activated = activate_remembered_app();
    paste_log(&format!("已激活上一个 App: {}", activated));
    std::thread::sleep(std::time::Duration::from_millis(320));

    // 4) 发送 ⌘V
    let trusted = is_accessibility_trusted();
    paste_log(&format!("隐藏后 trusted={}", trusted));
    if !trusted {
        // 首次使用：引导用户授权（内容已进剪贴板，可直接 ⌘V）
        open_macos_accessibility_settings();
        return Err("首次使用需要授权：请在「辅助功能」里勾选 ClipSync 再试（内容已复制，也可直接 ⌘V）".into());
    }

    match send_paste_shortcut() {
        Ok(()) => {
            paste_log("CGEvent 已发送 ⌘V");
            return Ok(());
        }
        Err(e) => paste_log(&format!("CGEvent 发送失败: {}", e)),
    }

    // 兜底：退回 AppleScript
    let script = r#"tell application "System Events" to keystroke "v" using command down"#;
    match std::process::Command::new("osascript").args(["-e", script]).output() {
        Ok(o) if o.status.success() => Ok(()),
        _ => Err("已复制到剪贴板，请手动按 ⌘V".into()),
    }
}

#[cfg(not(target_os = "macos"))]
fn paste_entry_impl(
    _app: AppHandle,
    _state: State<'_, SyncState>,
    _id: String,
) -> Result<(), String> {
    Err("仅 macOS 支持回车快速粘贴".into())
}

/// 取图片原图（data URL），本地缓存优先，缺失则从云端下载
#[tauri::command]
fn image_data_url(
    app: AppHandle,
    state: State<'_, SyncState>,
    hash: String,
) -> Result<String, String> {
    let ext = index_ext_of(state.inner(), &hash);

    let mut bytes = cache_image_path(&app, &hash, &ext)
        .and_then(|p| std::fs::read(p).ok())
        .unwrap_or_default();

    if bytes.is_empty() {
        let cfg = load_config(&app)?;
        bytes = dav_get_bytes(&cfg, &img_item_url_ext(&cfg, &hash, &ext))?
            .ok_or_else(|| "找不到这张图片".to_string())?;
        if let Some(p) = cache_image_path(&app, &hash, &ext) {
            let _ = std::fs::write(p, &bytes);
        }
    }

    let mime = if ext == "jpg" { "image/jpeg" } else { "image/png" };
    Ok(format!("data:{};base64,{}", mime, base64_encode(&bytes)))
}

/// 用系统浏览器打开链接
#[tauri::command]
fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    let u = url.trim().to_string();
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("仅支持 http/https 链接".into());
    }

    #[cfg(target_os = "macos")]
    {
        let _ = &app;
        std::process::Command::new("open")
            .arg(&u)
            .spawn()
            .map_err(|e| format!("打开失败: {}", e))?;
        return Ok(());
    }

    #[cfg(target_os = "android")]
    {
        #[derive(serde::Serialize)]
        struct Args {
            url: String,
        }
        let st = app
            .try_state::<AndroidGallery<tauri::Wry>>()
            .ok_or("插件未初始化")?;
        st.0
            .run_mobile_plugin::<serde_json::Value>("openUrl", Args { url: u })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        let _ = (&app, u);
        Err("当前平台不支持".into())
    }
}

/// 无障碍后台同步是否已启用（Android）
#[tauri::command]
fn accessibility_enabled(app: AppHandle) -> bool {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Serialize)]
        struct Empty {}
        #[derive(serde::Deserialize)]
        struct Res {
            #[serde(default)]
            enabled: bool,
        }
        if let Some(st) = app.try_state::<AndroidGallery<tauri::Wry>>() {
            if let Ok(r) = st
                .0
                .run_mobile_plugin::<Res>("isAccessibilityEnabled", Empty {})
            {
                return r.enabled;
            }
        }
        false
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        false
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtpStatus {
    enabled: bool,
    permission_granted: bool,
    connected: bool,
}

/// 开关短信验证码同步；默认关闭，设置仅保存在本机
#[tauri::command]
fn set_otp_enabled(
    app: AppHandle,
    state: State<'_, SyncState>,
    enabled: bool,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Serialize)]
        struct Args {
            enabled: bool,
        }
        let st = app
            .try_state::<AndroidGallery<tauri::Wry>>()
            .ok_or("插件未初始化")?;
        let result = st
            .0
            .run_mobile_plugin::<serde_json::Value>("setOtpEnabled", Args { enabled })
            .map(|_| ())
            .map_err(|e| e.to_string());
        if !enabled && result.is_ok() {
            // 已离线入队的验证码也一并撤销，避免关闭后恢复网络又上传。
            let mut q = state.outbox.lock().unwrap();
            q.retain(|item| item.kind != "otp");
            let snapshot = q.clone();
            drop(q);
            save_outbox(&app, &snapshot);
            let _ = app.emit("sync-status", status_snapshot(&state));
        }
        result
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, state, enabled);
        Err("仅 Android 支持短信验证码同步".into())
    }
}

#[tauri::command]
fn otp_status(app: AppHandle) -> OtpStatus {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Serialize)]
        struct Empty {}
        if let Some(st) = app.try_state::<AndroidGallery<tauri::Wry>>() {
            if let Ok(status) = st
                .0
                .run_mobile_plugin::<OtpStatus>("getOtpStatus", Empty {})
            {
                return status;
            }
        }
        OtpStatus::default()
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        OtpStatus::default()
    }
}

/// 打开系统通知使用权设置（Android）
#[tauri::command]
fn open_notification_settings(app: AppHandle) {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Serialize)]
        struct Empty {}
        if let Some(st) = app.try_state::<AndroidGallery<tauri::Wry>>() {
            let _ = st
                .0
                .run_mobile_plugin::<serde_json::Value>("openNotificationSettings", Empty {});
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
    }
}

/// 打开系统无障碍设置页（Android）
#[tauri::command]
fn open_accessibility_settings(app: AppHandle) {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Serialize)]
        struct Empty {}
        if let Some(st) = app.try_state::<AndroidGallery<tauri::Wry>>() {
            let _ = st
                .0
                .run_mobile_plugin::<serde_json::Value>("openAccessibilitySettings", Empty {});
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
    }
}

/// 隐藏窗口（Esc）
#[tauri::command]
fn hide_window(app: AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.hide();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

/// 上传一段 PNG 到云端，并写入本地索引/缓存/历史
#[allow(dead_code)]
fn upload_image_bytes(
    app: &AppHandle,
    state: &SyncState,
    cfg: &Config,
    png: &[u8],
    width: u32,
    height: u32,
) -> Result<String, String> {
    ensure_remote_dirs(cfg);
    let hash = fnv1a(png);
    dav_put_bytes(cfg, &img_item_url(cfg, &hash), png, "image/png")?;

    let item = ImageItem {
        hash: hash.clone(),
        ext: "png".to_string(),
        device_id: cfg.device_id.clone(),
        width,
        height,
        size: png.len() as u64,
        time: now_ms(),
    };
    {
        let mut idx = state.local_images.lock().unwrap();
        if !idx.items.iter().any(|i| i.hash == hash) {
            idx.items.push(item);
        }
        save_local_images(app, &idx);
    }
    if let Some(p) = cache_png_path(app, &hash) {
        let _ = std::fs::write(p, png);
    }

    let thumb = make_thumb_data_url(png);
    state.push_image_history(
        app,
        "up",
        &hash,
        &format!("[图片 {}×{} · {} KB]", width, height, png.len() / 1024),
        thumb,
    );

    // 立刻把清单推到云端，Mac 端尽快看到
    let idx = state.local_images.lock().unwrap().clone();
    let body = serde_json::to_string(&idx).unwrap_or_default();
    let _ = dav_put_text(cfg, &img_index_url(cfg), &body);

    Ok(hash)
}

/// 取走手机「分享」进来的图片并上传（Android）
#[tauri::command]
fn import_shared_image(app: AppHandle, state: State<'_, SyncState>) -> Result<String, String> {
    import_shared_image_impl(app, state)
}

#[cfg(target_os = "android")]
fn import_shared_image_impl(
    app: AppHandle,
    state: State<'_, SyncState>,
) -> Result<String, String> {
    use base64::Engine as _;

    #[derive(serde::Serialize)]
    struct Empty {}
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SharedRes {
        #[serde(default)]
        empty: bool,
        #[serde(default)]
        base64: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        width: u32,
        #[serde(default)]
        height: u32,
    }

    let st = app
        .try_state::<AndroidGallery<tauri::Wry>>()
        .ok_or("相册插件未初始化")?;
    let res: SharedRes = st
        .0
        .run_mobile_plugin("takeSharedImage", Empty {})
        .map_err(|e| e.to_string())?;

    if res.empty {
        return Ok(String::new());
    }
    let cfg = load_config(&app)?;
    if cfg.webdav_url.trim().is_empty() {
        return Err("请先在设置里配置 WebDAV".into());
    }

    let mut uploaded = 0usize;
    let mut current = Some(res);

    // 一次分享可能有多张图，逐张取走并上传（最多 9 张）
    while let Some(item) = current.take() {
        let Some(b64) = item.base64 else { break };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("图片解码失败: {}", e))?;

        let (w, h) = (item.width.max(1), item.height.max(1));
        let hash = upload_image_bytes(&app, state.inner(), &cfg, &bytes, w, h)?;
        *state.last_local_image.lock().unwrap() = Some(hash.clone());
        *state.last_applied_image.lock().unwrap() = Some(hash);
        *state.img_suppress_until.lock().unwrap() = now_ms() + 4000;
        uploaded += 1;

        if uploaded >= 9 {
            break;
        }
        // 继续取下一张（没有则 empty=true）
        match st
            .0
            .run_mobile_plugin::<SharedRes>("takeSharedImage", Empty {})
        {
            Ok(next) if !next.empty => current = Some(next),
            _ => break,
        }
    }

    if uploaded == 0 {
        return Ok(String::new());
    }
    Ok(format!("已分享 {} 张图片到云端", uploaded))
}

#[cfg(not(target_os = "android"))]
fn import_shared_image_impl(
    _app: AppHandle,
    _state: State<'_, SyncState>,
) -> Result<String, String> {
    Ok(String::new())
}

/// 一键清理图片：
/// - Android：删除相册 Pictures/ClipSync 下本 App 保存的图片
/// - 桌面端：清理本地图片缓存目录
#[tauri::command]
fn clear_image_storage(app: AppHandle) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Deserialize)]
        struct Res {
            deleted: u32,
        }
        let st = app
            .try_state::<AndroidGallery<tauri::Wry>>()
            .ok_or("相册插件未初始化")?;
        let res: Res = st
            .0
            .run_mobile_plugin("clearGallery", ())
            .map_err(|e| e.to_string())?;
        Ok(format!("已清理相册中的 {} 张图片", res.deleted))
    }
    #[cfg(not(target_os = "android"))]
    {
        let mut n = 0usize;
        if let Some(dir) = images_dir(&app) {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    let is_png = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("png"))
                        .unwrap_or(false);
                    if is_png && std::fs::remove_file(&p).is_ok() {
                        n += 1;
                    }
                }
            }
        }
        Ok(format!("已清理 {} 个本地图片缓存", n))
    }
}

#[tauri::command]
fn clear_history(app: AppHandle, state: State<'_, SyncState>) {
    {
        let mut h = state.history.lock().unwrap();
        h.clear();
        save_history(&app, &h);
    }
    let _ = app.emit("sync-history", ());
}

// ---------- macOS 托盘 ----------
#[cfg(target_os = "macos")]
fn show_main_window(app: &AppHandle) {
    // 弹出前先记住当前最前面的 App，供「回车粘贴」把焦点还回去
    remember_frontmost_app();
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 托盘句柄：用于动态刷新「最新同步记录」
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct TrayRefs {
    latest_item: tauri::menu::MenuItem<tauri::Wry>,
    hint_item: tauri::menu::MenuItem<tauri::Wry>,
    tray: tauri::tray::TrayIcon<tauri::Wry>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Default)]
struct TrayState(std::sync::Arc<Mutex<Option<TrayRefs>>>);

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Default)]
struct TrayState;

/// 截断为单行短文本
fn shorten_line(s: &str, max: usize) -> String {
    let line: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect();
    let t = line.trim();
    let mut out: String = t.chars().take(max).collect();
    if t.chars().count() > max {
        out.push('…');
    }
    if out.is_empty() {
        out.push_str("(空)");
    }
    out
}

/// 应用快捷键：先注销旧的，再注册新的
#[cfg(desktop)]
fn apply_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let mut msg = String::from("快捷键已关闭");
    if !hotkey.trim().is_empty() {
        gs.on_shortcut(hotkey, |app, _shortcut, event| {
            if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                show_main_window(app);
            }
        })
        .map_err(|e| format!("快捷键注册失败: {}", e))?;
        msg = format!("快捷键已设为 {}", hotkey);
    }

    // 同步托盘里的提示文字
    #[cfg(target_os = "macos")]
    {
        if let Some(st) = app.try_state::<TrayState>() {
            let refs = st.0.lock().unwrap().clone();
            if let Some(refs) = refs {
                let text = if hotkey.trim().is_empty() {
                    "未设置呼出快捷键".to_string()
                } else {
                    format!("{} 快速呼出", pretty_hotkey(hotkey))
                };
                let _ = refs.hint_item.set_text(text);
            }
        }
    }
    let _ = msg;
    Ok(())
}

/// CmdOrCtrl+Shift+V -> ⌘⇧V
#[cfg(target_os = "macos")]
fn pretty_hotkey(h: &str) -> String {
    h.replace("CmdOrCtrl", "⌘")
        .replace("Command", "⌘")
        .replace("Shift", "⇧")
        .replace("Alt", "⌥")
        .replace("Ctrl", "⌃")
        .replace('+', "")
}

#[cfg(target_os = "macos")]
fn update_tray_latest(app: &AppHandle, e: &Entry) {
    let Some(st) = app.try_state::<TrayState>() else {
        return;
    };
    let refs = st.0.lock().unwrap().clone();
    let Some(refs) = refs else {
        return;
    };

    let arrow = if e.dir == "up" { "↑" } else { "↓" };
    let short = shorten_line(&e.preview, 10);
    let clock = {
        // 本地时间 HH:MM
        let secs = (e.time / 1000) as i64;
        let dt = time_from_epoch(secs);
        dt
    };

    let _ = refs
        .latest_item
        .set_text(format!("最新 {} {} · {}", arrow, short, clock));
    // 菜单栏图标右侧直接显示最新记录
    let _ = refs.tray.set_title(Some(format!("{} {}", arrow, short)));
    let _ = refs
        .tray
        .set_tooltip(Some(format!("最新同步 {} {} · {}", arrow, short, clock)));
}

#[cfg(not(target_os = "macos"))]
fn update_tray_latest(_app: &AppHandle, _e: &Entry) {}

/// epoch 秒 -> "HH:MM"（本地时区）
#[cfg(target_os = "macos")]
fn time_from_epoch(secs: i64) -> String {
    // 用 chrono 会多一个依赖，这里用 libc localtime 的简化实现：交给系统
    use std::process::Command;
    let out = Command::new("date")
        .args(["-r", &secs.to_string(), "+%H:%M"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

#[cfg(target_os = "macos")]
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let latest = MenuItem::with_id(app, "latest", "最新：暂无记录", true, None::<&str>)?;
    let hint = MenuItem::with_id(app, "hint", "⌘⇧V 快速呼出", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "打开设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = tauri::menu::PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&latest, &hint, &sep, &open, &quit])?;

    let tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("剪贴板同步")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    // 保存句柄，供同步时刷新
    if let Some(st) = app.try_state::<TrayState>() {
        *st.0.lock().unwrap() = Some(TrayRefs {
            latest_item: latest,
            hint_item: hint,
            tray,
        });
    }
    Ok(())
}

// ---------- 入口 ----------
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let sync_state = SyncState {
        running: Arc::new(AtomicBool::new(false)),
        last_local_sent: Arc::new(Mutex::new(None)),
        last_applied_remote: Arc::new(Mutex::new(None)),
        last_error: Arc::new(Mutex::new(None)),
        last_sync_at: Arc::new(Mutex::new(None)),
        history: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        favorites: Arc::new(Mutex::new(FavDoc::default())),
        last_fav_sync: Arc::new(Mutex::new(0)),
        last_local_image: Arc::new(Mutex::new(None)),
        last_applied_image: Arc::new(Mutex::new(None)),
        img_suppress_until: Arc::new(Mutex::new(0)),
        local_images: Arc::new(Mutex::new(ImageIndex::default())),
        baseline_done: Arc::new(AtomicBool::new(false)),
        last_clip_count: Arc::new(Mutex::new(0)),
        outbox: Arc::new(Mutex::new(Vec::new())),
        seq: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        #[cfg(target_os = "android")]
        activity_marker: Arc::new(Mutex::new(None)),
        #[cfg(target_os = "android")]
        native_status_cache: Arc::new(Mutex::new(None)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(gallery_plugin())
        .plugin(hotkey_plugin())
        .manage(sync_state.clone())
        .manage(TrayState::default())
        .on_window_event(|app, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                #[cfg(target_os = "macos")]
                {
                    api.prevent_close();
                    let _ = app.get_webview_window("main").and_then(|w| w.hide().ok());
                }
            }
        })
        .setup(move |app| {
            // 在 Activity 可用时缓存纯文件路径；之后不能在无 Activity 时调用 Tauri path 插件。
            #[cfg(target_os = "android")]
            {
                let marker = app
                    .path()
                    .app_data_dir()
                    .ok()
                    .map(|dir| dir.join(".main_activity_alive"));
                *app.state::<SyncState>().activity_marker.lock().unwrap() = marker;
            }

            // 载入本地数据：收藏（永久）与最近记录（7 天）
            {
                let st = app.state::<SyncState>();
                *st.favorites.lock().unwrap() = load_favorites(app.handle());
                *st.history.lock().unwrap() = load_history(app.handle());
                *st.local_images.lock().unwrap() = load_local_images(app.handle());
                *st.outbox.lock().unwrap() = load_outbox(app.handle());
                st.prune_history();

                // 已有配置则启动即自动开始同步
                let mut has_config = false;
                if let Ok(cfg) = load_config(app.handle()) {
                    if !cfg.webdav_url.trim().is_empty() {
                        st.running.store(true, Ordering::SeqCst);
                        has_config = true;
                    }
                }

                // 启动时把「当前剪贴板」当作基线：它是上次遗留的旧内容，
                // 不应被当成用户新复制的内容再上传（否则会覆盖别端的更新）。
                if has_config {
                    *st.last_clip_count.lock().unwrap() =
                        clipboard_change_count().unwrap_or(0);
                    if let Ok(current) = app.clipboard().read_text() {
                        if !current.is_empty() {
                            *st.last_local_sent.lock().unwrap() = Some(current);
                        }
                    }
                }
            }

            #[cfg(target_os = "android")]
            publish_android_status(&sync_state);

            #[cfg(target_os = "macos")]
            {
                let _ = build_tray(app.handle());
                // 启动时先用最近一条记录填充菜单栏
                let last = app
                    .state::<SyncState>()
                    .history
                    .lock()
                    .unwrap()
                    .front()
                    .cloned();
                if let Some(entry) = last {
                    update_tray_latest(app.handle(), &entry);
                }
            }

            // 按配置注册全局快捷键
            #[cfg(desktop)]
            {
                let hk = load_config(app.handle())
                    .map(|c| c.hotkey)
                    .unwrap_or_else(|_| default_hotkey());
                let _ = apply_hotkey(app.handle(), &hk);
            }

            spawn_sync_loop(app.handle().clone(), sync_state.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            start_sync,
            stop_sync,
            get_status,
            get_history,
            get_favorites,
            toggle_favorite,
            copy_image,
            clear_image_storage,
            paste_entry,
            hide_window,
            import_shared_image,
            touch_entry,
            open_url,
            image_data_url,
            accessibility_enabled,
            open_accessibility_settings,
            set_otp_enabled,
            otp_status,
            open_notification_settings,
            clear_history
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
