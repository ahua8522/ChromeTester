use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

const CFT_API: &str = "https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json";

// Chromium快照数据源（提供M113之前的历史版本）
const MILESTONES_API: &str = "https://chromiumdash.appspot.com/fetch_milestones?only_branched=true";
const SNAPSHOT_LIST_API: &str = "https://www.googleapis.com/storage/v1/b/chromium-browser-snapshots/o";
const SNAPSHOT_DL_BASE: &str = "https://commondatastorage.googleapis.com/chromium-browser-snapshots";

// npmmirror 国内镜像（淘宝，无需梯子直连）
const NPM_CFT_BASE: &str = "https://registry.npmmirror.com/-/binary/chrome-for-testing/";
const NPM_SNAP_BASE: &str = "https://registry.npmmirror.com/-/binary/chromium-browser-snapshots/";

// 内置版本快照（首次启动即时展示、无需联网；可通过“刷新”更新缓存）
const CHROME_VERSIONS_SNAPSHOT: &str = include_str!("../snapshot/chrome-versions.json");
const CHROMIUM_MILESTONES_SNAPSHOT: &str = include_str!("../snapshot/chromium-milestones.json");

// ---------- 数据结构 ----------

#[derive(Serialize, Clone)]
pub struct VersionInfo {
    version: String,
    download_url: String,
}

#[derive(Serialize, Clone)]
pub struct MilestoneInfo {
    milestone: u32,
    position: u64,
    /// 快照目录地址（实际下载时会定位到最近的可用构建）
    snapshot_url: String,
}

#[derive(Serialize, Clone)]
pub struct InstalledVersion {
    version: String,
    path: String,
}

#[derive(Serialize, Clone)]
pub struct ProfileInfo {
    name: String,
    path: String,
    /// 自定义启动参数（如 --proxy-server=..., --lang=en-US）
    args: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct DownloadProgress {
    version: String,
    status: String, // downloading | extracting | completed | error
    percent: u32,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    current: String,
    latest: String,
    has_update: bool,
    url: String,
    notes: String,
}

// ---------- 路径工具 ----------

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

// ---------- 配置文件（存于默认应用数据目录，不随安装路径变化） ----------

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join("config.json"))
}

fn load_config(app: &AppHandle) -> serde_json::Value {
    config_path(app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn save_config(app: &AppHandle, cfg: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(app)?, text).map_err(|e| e.to_string())
}

/// 下载源："google"（官方，需梯子）| "npmmirror"（国内镜像）
fn download_source(app: &AppHandle) -> String {
    let cfg = load_config(app);
    match cfg["download_source"].as_str() {
        Some("npmmirror") => "npmmirror".to_string(),
        _ => "google".to_string(),
    }
}

/// 用户自定义的安装目录（未配置时返回None）
fn custom_install_dir(app: &AppHandle) -> Option<PathBuf> {
    let cfg = load_config(app);
    let p = cfg["install_dir"].as_str()?.trim().to_string();
    if p.is_empty() {
        return None;
    }
    Some(PathBuf::from(p))
}

fn chrome_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = match custom_install_dir(app) {
        Some(d) => d,
        None => data_dir(app)?.join("chrome"),
    };
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn profiles_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = data_dir(app)?.join("profiles");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

// ---------- 多安装路径（记录历史路径，“已安装”全部扫描） ----------

/// 所有需要扫描的安装目录：默认目录 + 当前自定义目录 + 历史用过的目录（去重、仅保留存在的）
fn all_install_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, dirs: &mut Vec<PathBuf>| {
        if p.is_dir() && !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    // 当前生效目录（自定义或默认）优先
    if let Ok(d) = chrome_dir(app) {
        push(d, &mut dirs);
    }
    // 默认目录
    if let Ok(base) = data_dir(app) {
        push(base.join("chrome"), &mut dirs);
    }
    // 历史目录
    let cfg = load_config(app);
    if let Some(arr) = cfg["install_dirs"].as_array() {
        for v in arr {
            if let Some(s) = v.as_str() {
                push(PathBuf::from(s), &mut dirs);
            }
        }
    }
    dirs
}

/// 在所有安装目录中定位某个版本的目录
fn find_version_dir(app: &AppHandle, version: &str) -> Option<PathBuf> {
    for d in all_install_dirs(app) {
        let p = d.join(version);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// 把目录记入历史（install_dirs），用于切换路径后仍能扫到旧版本
fn record_install_dir(cfg: &mut serde_json::Value, path: &str) {
    let arr = cfg
        .as_object_mut()
        .and_then(|o| o.entry("install_dirs").or_insert(serde_json::json!([])).as_array_mut());
    if let Some(arr) = arr {
        if !arr.iter().any(|v| v.as_str() == Some(path)) {
            arr.push(serde_json::json!(path));
        }
    }
}

/// 当前平台标识（与Chrome for Testing API对应）
fn current_platform() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "win64"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "mac-arm64"
    } else if cfg!(target_os = "macos") {
        "mac-x64"
    } else {
        "linux64"
    }
}

/// 版本号解析为数字数组，用于排序比较
/// chromium-M65 这类目录名解析为[0]，排在数字版本之后
fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// 校验名称合法性：仅允许字母、数字、中文、下划线、连字符
/// 防止路径穿越及生成非法Windows文件名
fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if name.len() > 64 {
        return Err("名称过长".into());
    }
    let ok = name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err("名称仅支持字母、数字、下划线、连字符".into());
    }
    Ok(())
}

/// 校验版本目录名：数字点分版本号（如114.0.5735.90）或chromium-M{里程碑}
fn validate_version(version: &str) -> Result<(), String> {
    let ok = !version.is_empty()
        && !version.contains("..")
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok {
        return Err("非法版本号".into());
    }
    Ok(())
}

/// Chromium快照桶的平台目录名
fn snapshot_platform() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "Win_x64"
    } else if cfg!(target_os = "windows") {
        "Win"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "Mac_Arm"
    } else if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "Linux_x64"
    }
}

// ---------- HTTP客户端（支持Windows系统代理） ----------

/// 读取Windows注册表中的系统代理设置（Internet选项）
/// reqwest只认HTTP_PROXY等环境变量，不会读系统代理，需手动桥接
#[cfg(windows)]
fn system_proxy() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    if server.is_empty() {
        return None;
    }
    // 可能是 "host:port" 或 "http=host:port;https=host:port" 格式
    let proxy = if server.contains('=') {
        server
            .split(';')
            .find_map(|part| part.strip_prefix("https=").or(part.strip_prefix("http=")))?
            .to_string()
    } else {
        server
    };
    Some(format!("http://{proxy}"))
}

#[cfg(not(windows))]
fn system_proxy() -> Option<String> {
    None
}

/// 全局HTTP客户端：自动应用系统代理，设置连接/请求超时
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60));
        // 环境变量代理（reqwest默认支持）优先；否则尝试Windows系统代理
        let has_env_proxy = std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("http_proxy"))
            .is_ok();
        if !has_env_proxy {
            if let Some(proxy) = system_proxy() {
                if let Ok(p) = reqwest::Proxy::all(&proxy) {
                    builder = builder.proxy(p);
                }
            }
        }
        builder.build().expect("创建HTTP客户端失败")
    })
}

/// 下载大文件专用客户端：不限制总超时（只限连接超时）
fn download_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(15));
        let has_env_proxy = std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("http_proxy"))
            .is_ok();
        if !has_env_proxy {
            if let Some(proxy) = system_proxy() {
                if let Ok(p) = reqwest::Proxy::all(&proxy) {
                    builder = builder.proxy(p);
                }
            }
        }
        builder.build().expect("创建HTTP客户端失败")
    })
}

// ---------- Commands ----------

/// 构造某个版本的下载地址（根据当前下载源）
fn cft_download_url(source: &str, version: &str, platform: &str) -> String {
    if source == "npmmirror" {
        format!("{NPM_CFT_BASE}{version}/{platform}/chrome-{platform}.zip")
    } else {
        format!(
            "https://storage.googleapis.com/chrome-for-testing-public/{version}/{platform}/chrome-{platform}.zip"
        )
    }
}

/// 从本地缓存或内置快照读取版本字符串列表（不联网）
fn load_version_strings(app: &AppHandle) -> Vec<String> {
    let text = data_dir(app)
        .ok()
        .and_then(|d| fs::read_to_string(d.join("versions-cache.json")).ok())
        .unwrap_or_else(|| CHROME_VERSIONS_SNAPSHOT.to_string());
    serde_json::from_str::<Vec<String>>(&text)
        .or_else(|_| serde_json::from_str::<Vec<String>>(CHROME_VERSIONS_SNAPSHOT))
        .unwrap_or_default()
}

/// 由版本字符串构造完整的可用版本列表
fn build_version_list(app: &AppHandle, versions: Vec<String>) -> Vec<VersionInfo> {
    let source = download_source(app);
    let platform = current_platform();
    let mut list: Vec<VersionInfo> = versions
        .into_iter()
        .filter(|v| validate_version(v).is_ok())
        .map(|v| {
            let download_url = cft_download_url(&source, &v, platform);
            VersionInfo {
                version: v,
                download_url,
            }
        })
        .collect();
    list.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    list
}

/// 获取可用版本（本地缓存/内置快照，不联网，启动即时返回）
#[tauri::command]
fn get_available_versions(app: AppHandle) -> Result<Vec<VersionInfo>, String> {
    Ok(build_version_list(&app, load_version_strings(&app)))
}

/// 刷新：从网络拉取最新版本列表并更新缓存
#[tauri::command]
async fn refresh_available_versions(app: AppHandle) -> Result<Vec<VersionInfo>, String> {
    let source = download_source(&app);
    let platform = current_platform();
    let versions = fetch_version_strings(&source, platform).await?;
    if versions.is_empty() {
        return Err("未获取到版本列表".into());
    }
    if let Ok(d) = data_dir(&app) {
        let _ = fs::write(
            d.join("versions-cache.json"),
            serde_json::to_string(&versions).unwrap_or_default(),
        );
    }
    Ok(build_version_list(&app, versions))
}

/// 从网络获取版本字符串列表（根据下载源）
async fn fetch_version_strings(source: &str, platform: &str) -> Result<Vec<String>, String> {
    let empty = vec![];
    if source == "npmmirror" {
        let data: serde_json::Value = http_client()
            .get(NPM_CFT_BASE)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(data
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter(|e| e["type"].as_str() == Some("dir"))
            .filter_map(|e| {
                let v = e["name"].as_str()?.trim_end_matches('/').to_string();
                if validate_version(&v).is_ok() {
                    Some(v)
                } else {
                    None
                }
            })
            .collect())
    } else {
        let data: serde_json::Value = http_client()
            .get(CFT_API)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(data["versions"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|v| {
                let version = v["version"].as_str()?.to_string();
                let has = v["downloads"]["chrome"]
                    .as_array()?
                    .iter()
                    .any(|d| d["platform"].as_str() == Some(platform));
                if has {
                    Some(version)
                } else {
                    None
                }
            })
            .collect())
    }
}

#[tauri::command]
fn get_installed_versions(app: AppHandle) -> Result<Vec<InstalledVersion>, String> {
    let mut installed: Vec<InstalledVersion> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    // 扫描所有安装目录（当前 + 历史），同名版本以先扫到的为准
    for dir in all_install_dirs(&app) {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let version = entry.file_name().to_string_lossy().to_string();
            if seen.contains(&version) {
                continue;
            }
            seen.push(version.clone());
            installed.push(InstalledVersion {
                version,
                path: entry.path().to_string_lossy().to_string(),
            });
        }
    }

    installed.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    Ok(installed)
}

#[tauri::command]
async fn download_version(
    app: AppHandle,
    version: String,
    download_url: String,
) -> Result<(), String> {
    validate_version(&version)?;
    // 仅允许从官方源或 npmmirror 镜像下载
    let allowed = download_url
        .starts_with("https://storage.googleapis.com/chrome-for-testing-public/")
        || download_url.starts_with("https://registry.npmmirror.com/-/binary/chrome-for-testing/");
    if !allowed {
        return Err("非法下载地址".into());
    }

    let target_dir = chrome_dir(&app)?.join(&version);
    if target_dir.exists() {
        return Err("版本已存在".into());
    }

    download_and_extract(&app, &version, &download_url, &target_dir).await
}

/// 下载zip并解压到目标目录，全程通过download-progress事件推送进度
async fn download_and_extract(
    app: &AppHandle,
    label: &str,
    download_url: &str,
    target_dir: &PathBuf,
) -> Result<(), String> {
    let emit = |status: &str, percent: u32, error: Option<String>| {
        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                version: label.to_string(),
                status: status.into(),
                percent,
                error,
            },
        );
    };

    // 1. 流式下载zip
    emit("downloading", 0, None);
    let resp = download_client()
        .get(download_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let e = format!("下载失败: HTTP {}", resp.status());
        emit("error", 0, Some(e.clone()));
        return Err(e);
    }
    let total = resp.content_length().unwrap_or(0);

    let zip_path = data_dir(app)?.join(format!("chrome-{label}.zip"));
    let download_result: Result<(), String> = async {
        let mut file = fs::File::create(&zip_path).map_err(|e| e.to_string())?;
        let mut downloaded: u64 = 0;
        let mut last_percent: u32 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            downloaded += chunk.len() as u64;
            if total > 0 {
                let percent = (downloaded * 100 / total) as u32;
                if percent != last_percent {
                    last_percent = percent;
                    emit("downloading", percent, None);
                }
            }
        }
        Ok(())
    }
    .await;

    if let Err(e) = download_result {
        let _ = fs::remove_file(&zip_path);
        emit("error", 0, Some(e.clone()));
        return Err(e);
    }

    // 2. 解压（阻塞操作放到blocking线程，避免卡住异步运行时）
    emit("extracting", 100, None);
    let extract_result = {
        let zip_path = zip_path.clone();
        let target_dir = target_dir.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let file = fs::File::open(&zip_path).map_err(|e| e.to_string())?;
            let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
            fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
            archive.extract(&target_dir).map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    };

    // 3. 清理zip；解压失败则回滚半成品目录
    let _ = fs::remove_file(&zip_path);
    if let Err(e) = extract_result {
        let _ = fs::remove_dir_all(target_dir);
        emit("error", 0, Some(e.clone()));
        return Err(e);
    }

    emit("completed", 100, None);
    Ok(())
}

// ---------- Chromium历史版本（快照源） ----------

#[tauri::command]
fn get_chromium_milestones(app: AppHandle) -> Result<Vec<MilestoneInfo>, String> {
    Ok(build_milestone_list(&app, load_milestone_pairs(&app)))
}

/// 刷新：从 chromiumdash 拉取最新里程碑并更新缓存（需可访问 Google）
#[tauri::command]
async fn refresh_chromium_milestones(app: AppHandle) -> Result<Vec<MilestoneInfo>, String> {
    let pairs = fetch_milestone_pairs().await?;
    if pairs.is_empty() {
        return Err("未获取到里程碑列表".into());
    }
    if let Ok(d) = data_dir(&app) {
        let _ = fs::write(
            d.join("milestones-cache.json"),
            serde_json::to_string(&pairs).unwrap_or_default(),
        );
    }
    Ok(build_milestone_list(&app, pairs))
}

/// 从本地缓存或内置快照读取里程碑对（不联网）
fn load_milestone_pairs(app: &AppHandle) -> Vec<(u32, u64)> {
    let text = data_dir(app)
        .ok()
        .and_then(|d| fs::read_to_string(d.join("milestones-cache.json")).ok())
        .unwrap_or_else(|| CHROMIUM_MILESTONES_SNAPSHOT.to_string());
    serde_json::from_str::<Vec<(u32, u64)>>(&text)
        .or_else(|_| serde_json::from_str::<Vec<(u32, u64)>>(CHROMIUM_MILESTONES_SNAPSHOT))
        .unwrap_or_default()
}

/// 由里程碑对构造列表（含当前下载源的快照地址）
fn build_milestone_list(app: &AppHandle, pairs: Vec<(u32, u64)>) -> Vec<MilestoneInfo> {
    let platform = snapshot_platform();
    let npm = download_source(app) == "npmmirror";
    let mut list: Vec<MilestoneInfo> = pairs
        .iter()
        .map(|&(milestone, position)| {
            let snapshot_url = if npm {
                format!("{NPM_SNAP_BASE}{platform}/{position}/")
            } else {
                format!("{SNAPSHOT_DL_BASE}/{platform}/{position}/")
            };
            MilestoneInfo {
                milestone,
                position,
                snapshot_url,
            }
        })
        .collect();
    list.sort_by(|a, b| b.milestone.cmp(&a.milestone));
    list
}

/// 从 chromiumdash 获取 (里程碑, 分支点position) 对（仅M113之前）
async fn fetch_milestone_pairs() -> Result<Vec<(u32, u64)>, String> {
    let resp = http_client()
        .get(MILESTONES_API)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    let empty = vec![];
    Ok(data
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|m| {
            let milestone = m["milestone"].as_u64()? as u32;
            let position = m["chromium_main_branch_position"].as_u64()?;
            // M113及之后由Chrome for Testing覆盖，此处只提供更早的里程碑
            if milestone >= 113 || position == 0 {
                return None;
            }
            Some((milestone, position))
        })
        .collect())
}

/// 列出快照桶中指定数字前缀下的所有position
async fn list_snapshot_positions(
    client: &reqwest::Client,
    platform: &str,
    digits: &str,
) -> Result<Vec<u64>, String> {
    let url = format!(
        "{SNAPSHOT_LIST_API}?delimiter=/&prefix={platform}/{digits}&fields=prefixes&maxResults=500"
    );
    let data: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let empty = vec![];
    Ok(data["prefixes"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|p| {
            p.as_str()?
                .trim_end_matches('/')
                .rsplit('/')
                .next()?
                .parse()
                .ok()
        })
        .collect())
}

/// 根据分支点position定位最近的可用快照，返回完整下载地址
async fn resolve_snapshot(app: &AppHandle, platform: &str, target: u64) -> Result<String, String> {
    if download_source(app) == "npmmirror" {
        resolve_snapshot_npm(platform, target).await
    } else {
        resolve_snapshot_google(platform, target).await
    }
}

/// Google 官方：用 storage API 定位快照
async fn resolve_snapshot_google(platform: &str, target: u64) -> Result<String, String> {
    let client = http_client();
    let pos_str = target.to_string();

    // 逐步放宽数字前缀（先搜±百以内，再±千、±万），优先取 >= 分支点的最近快照
    let mut chosen: Option<u64> = None;
    for cut in 2..=4 {
        if pos_str.len() <= cut {
            break;
        }
        let digits = &pos_str[..pos_str.len() - cut];
        let mut positions = list_snapshot_positions(client, platform, digits).await?;
        positions.sort_unstable();
        // 取小于分支点的最大快照：分支前最后的该里程碑主干构建（否则会拿到下一个里程碑的早期构建）
        if let Some(&p) = positions.iter().rev().find(|&&p| p < target) {
            chosen = Some(p);
            break;
        }
        // 该前缀内都不小于分支点：退而取最接近的
        if let Some(&p) = positions.iter().min_by_key(|&&p| (p as i128 - target as i128).abs()) {
            chosen = Some(p);
            break;
        }
    }
    let pos = chosen.ok_or("未在快照库中找到可用构建")?;

    // 查该position下的浏览器zip对象名（不同年代命名不同）
    let url = format!("{SNAPSHOT_LIST_API}?prefix={platform}/{pos}/&fields=items(name)");
    let data: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let empty = vec![];
    let names: Vec<&str> = data["items"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|i| i["name"].as_str())
        .collect();

    let zip_name = [
        "chrome-win.zip",
        "chrome-win32.zip",
        "chrome-mac.zip",
        "chrome-linux.zip",
    ]
    .iter()
    .find_map(|z| names.iter().find(|n| n.ends_with(z)))
    .ok_or("快照中未找到浏览器压缩包")?;

    Ok(format!("{SNAPSHOT_DL_BASE}/{zip_name}"))
}

/// npmmirror 镜像：从目录列表定位快照
async fn resolve_snapshot_npm(platform: &str, target: u64) -> Result<String, String> {
    let client = http_client();
    let empty = vec![];

    // 列出该平台下所有position（一次请求返回全部）
    let list_url = format!("{NPM_SNAP_BASE}{platform}/");
    let data: serde_json::Value = client
        .get(&list_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let mut positions: Vec<u64> = data
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|e| e["name"].as_str()?.trim_end_matches('/').parse().ok())
        .collect();
    positions.sort_unstable();
    let pos = positions
        .iter()
        .rev()
        .find(|&&p| p < target)
        .copied()
        .or_else(|| {
            positions
                .iter()
                .min_by_key(|&&p| (p as i128 - target as i128).abs())
                .copied()
        })
        .ok_or("镜像快照库中未找到可用构建")?;

    // 查该position下的zip文件名
    let files_url = format!("{NPM_SNAP_BASE}{platform}/{pos}/");
    let fdata: serde_json::Value = client
        .get(&files_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let names: Vec<&str> = fdata
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|i| i["name"].as_str())
        .collect();

    let zip = [
        "chrome-win.zip",
        "chrome-win32.zip",
        "chrome-mac.zip",
        "chrome-linux.zip",
    ]
    .iter()
    .find_map(|z| names.iter().find(|n| n.ends_with(z)))
    .ok_or("镜像快照中未找到浏览器压缩包")?;

    Ok(format!("{NPM_SNAP_BASE}{platform}/{pos}/{}", zip.trim_end_matches('/')))
}

#[tauri::command]
async fn download_chromium(app: AppHandle, milestone: u32, position: u64) -> Result<(), String> {
    let label = format!("chromium-M{milestone}");
    let target_dir = chrome_dir(&app)?.join(&label);
    if target_dir.exists() {
        return Err("该版本已存在".into());
    }

    // 定位快照（可能需要几秒，先推送一条进度让前端显示状态）
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            version: label.clone(),
            status: "downloading".into(),
            percent: 0,
            error: None,
        },
    );
    let url = match resolve_snapshot(&app, snapshot_platform(), position).await {
        Ok(u) => u,
        Err(e) => {
            let _ = app.emit(
                "download-progress",
                DownloadProgress {
                    version: label.clone(),
                    status: "error".into(),
                    percent: 0,
                    error: Some(e.clone()),
                },
            );
            return Err(e);
        }
    };

    download_and_extract(&app, &label, &url, &target_dir).await
}

#[tauri::command]
fn delete_version(app: AppHandle, version: String) -> Result<(), String> {
    validate_version(&version)?;
    // 在所有安装目录中定位后删除
    if let Some(dir) = find_version_dir(&app, &version) {
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 在系统文件管理器中打开指定目录
fn open_in_explorer(dir: &PathBuf) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(dir).spawn();
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(dir).spawn();

    result.map_err(|e| e.to_string())?;
    Ok(())
}

/// 在系统文件管理器中打开已安装版本的目录
#[tauri::command]
fn open_version_folder(app: AppHandle, version: String) -> Result<(), String> {
    validate_version(&version)?;
    let dir = find_version_dir(&app, &version).ok_or("目录不存在")?;
    open_in_explorer(&dir)
}

// ---------- 安装路径设置 ----------

#[derive(Serialize, Clone)]
pub struct InstallDirInfo {
    path: String,
    is_custom: bool,
}

/// 获取当前下载源
#[tauri::command]
fn get_download_source(app: AppHandle) -> Result<String, String> {
    Ok(download_source(&app))
}

/// 设置下载源（google | npmmirror）
#[tauri::command]
fn set_download_source(app: AppHandle, source: String) -> Result<(), String> {
    let s = if source == "npmmirror" { "npmmirror" } else { "google" };
    let mut cfg = load_config(&app);
    let obj = cfg.as_object_mut().ok_or("配置格式错误")?;
    obj.insert("download_source".into(), serde_json::json!(s));
    save_config(&app, &cfg)
}

#[tauri::command]
fn get_install_dir(app: AppHandle) -> Result<InstallDirInfo, String> {
    let is_custom = custom_install_dir(&app).is_some();
    let dir = chrome_dir(&app)?;
    Ok(InstallDirInfo {
        path: dir.to_string_lossy().to_string(),
        is_custom,
    })
}

/// 弹出系统文件夹选择器，返回选中路径（取消返回None）
#[tauri::command]
async fn pick_install_dir() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择Chrome版本安装目录")
            .pick_folder()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())
}

/// 设置安装目录；传空字符串则恢复默认。切换时把旧目录记入历史，“已安装”仍可扫到旧版本
#[tauri::command]
fn set_install_dir(app: AppHandle, path: String) -> Result<(), String> {
    let mut cfg = load_config(&app);
    // 先把当前生效目录记入历史
    if let Ok(cur) = chrome_dir(&app) {
        record_install_dir(&mut cfg, &cur.to_string_lossy());
    }
    let obj = cfg.as_object_mut().ok_or("配置格式错误")?;

    let trimmed = path.trim();
    if trimmed.is_empty() {
        obj.remove("install_dir");
    } else {
        let dir = PathBuf::from(trimmed);
        // 验证目录可创建/可写
        fs::create_dir_all(&dir).map_err(|e| format!("目录不可用: {e}"))?;
        obj.insert(
            "install_dir".into(),
            serde_json::json!(dir.to_string_lossy()),
        );
        // 新目录也记入历史
        record_install_dir(&mut cfg, &dir.to_string_lossy());
    }
    save_config(&app, &cfg)
}

/// 在文件管理器中打开当前安装目录
#[tauri::command]
fn open_install_dir(app: AppHandle) -> Result<(), String> {
    let dir = chrome_dir(&app)?;
    open_in_explorer(&dir)
}

// ---------- 浏览器配置（数据目录 + 自定义启动参数） ----------
// config.json 中 profiles: { "default": [], "work": ["--proxy-server=...", ...] }
// 数据目录为 profiles/<name>，参数存于配置

fn clean_args(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(50)
        .collect()
}

fn load_profiles_obj(app: &AppHandle) -> serde_json::Map<String, serde_json::Value> {
    let cfg = load_config(app);
    let mut m = cfg["profiles"].as_object().cloned().unwrap_or_default();
    if !m.contains_key("default") {
        m.insert("default".into(), serde_json::json!([]));
    }
    m
}

fn save_profiles_obj(
    app: &AppHandle,
    profiles: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let mut cfg = load_config(app);
    cfg.as_object_mut()
        .ok_or("配置格式错误")?
        .insert("profiles".into(), serde_json::Value::Object(profiles));
    save_config(app, &cfg)
}

fn profile_args(app: &AppHandle, name: &str) -> Vec<String> {
    load_profiles_obj(app)
        .get(name)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[tauri::command]
fn list_profiles(app: AppHandle) -> Result<Vec<ProfileInfo>, String> {
    let base = profiles_dir(&app)?;
    let m = load_profiles_obj(&app);
    let mut out: Vec<ProfileInfo> = m
        .iter()
        .map(|(name, v)| {
            let args = v
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            ProfileInfo {
                name: name.clone(),
                path: base.join(name).to_string_lossy().to_string(),
                args,
            }
        })
        .collect();
    // default 排最前，其余按名称
    out.sort_by(|a, b| match (a.name.as_str(), b.name.as_str()) {
        ("default", "default") => std::cmp::Ordering::Equal,
        ("default", _) => std::cmp::Ordering::Less,
        (_, "default") => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    Ok(out)
}

#[tauri::command]
fn create_profile(app: AppHandle, name: String, args: Vec<String>) -> Result<(), String> {
    validate_name(&name)?;
    let mut m = load_profiles_obj(&app);
    if m.contains_key(&name) {
        return Err("配置已存在".into());
    }
    fs::create_dir_all(profiles_dir(&app)?.join(&name)).map_err(|e| e.to_string())?;
    m.insert(name, serde_json::json!(clean_args(args)));
    save_profiles_obj(&app, m)
}

#[tauri::command]
fn update_profile(app: AppHandle, name: String, args: Vec<String>) -> Result<(), String> {
    validate_name(&name)?;
    let mut m = load_profiles_obj(&app);
    // 确保数据目录存在（包括首次为 default 设参）
    fs::create_dir_all(profiles_dir(&app)?.join(&name)).map_err(|e| e.to_string())?;
    m.insert(name, serde_json::json!(clean_args(args)));
    save_profiles_obj(&app, m)
}

#[tauri::command]
fn delete_profile(app: AppHandle, name: String) -> Result<(), String> {
    validate_name(&name)?;
    if name == "default" {
        return Err("默认配置不可删除".into());
    }
    let mut m = load_profiles_obj(&app);
    m.remove(&name);
    save_profiles_obj(&app, m)?;
    let dir = profiles_dir(&app)?.join(&name);
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
    Ok(())
}

/// 结束正在使用指定 user-data-dir 的 chrome 进程
/// 解决“关闭浏览器后再次启动无反应”：旧实例未真正退出会占用 profile 单例，
/// 导致新启动被转发给无窗口的“僵尸”进程。仅按完整 --user-data-dir 精确匹配，不会误杀用户自己的 Chrome。
fn kill_chrome_using_profile(profile_path: &PathBuf) {
    let arg = format!("--user-data-dir={}", profile_path.display());
    let mut sys = sysinfo::System::new();
    sys.refresh_processes();
    for proc_ in sys.processes().values() {
        if proc_.cmd().iter().any(|a| a == &arg) {
            proc_.kill();
        }
    }
}

#[tauri::command]
fn launch_chrome(app: AppHandle, version: String, profile: String) -> Result<(), String> {
    validate_version(&version)?;
    validate_name(&profile)?;

    let base = find_version_dir(&app, &version).ok_or("未找到该版本目录")?;

    // 尝试不同的目录结构定位chrome可执行文件（含Chromium快照的chrome-win等）
    let candidates = [
        base.join("chrome-win64").join("chrome.exe"),
        base.join("chrome-win32").join("chrome.exe"),
        base.join("chrome-win").join("chrome.exe"),
        base.join("chrome.exe"),
        base.join("chrome-linux64").join("chrome"),
        base.join("chrome-linux").join("chrome"),
        base.join("chrome-mac-arm64")
            .join("Google Chrome for Testing.app")
            .join("Contents")
            .join("MacOS")
            .join("Google Chrome for Testing"),
        base.join("chrome-mac")
            .join("Chromium.app")
            .join("Contents")
            .join("MacOS")
            .join("Chromium"),
    ];
    let chrome_path = candidates
        .iter()
        .find(|p| p.exists())
        .ok_or("未找到Chrome可执行文件")?;

    // 数据隔离：default 按版本独立目录（不同版本互不影响、可同时开）；命名配置跨版本共享
    let profile_path = if profile == "default" {
        profiles_dir(&app)?.join("default").join(&version)
    } else {
        profiles_dir(&app)?.join(&profile)
    };
    fs::create_dir_all(&profile_path).map_err(|e| e.to_string())?;

    // 启动前先结束该profile可能残留的旧实例，保证“关闭后再启动”总能打开新窗口
    kill_chrome_using_profile(&profile_path);

    let mut cmd = Command::new(chrome_path);
    cmd.arg(format!("--user-data-dir={}", profile_path.display()))
        .arg("--no-first-run");
    // 追加该配置的自定义启动参数
    for a in profile_args(&app, &profile) {
        cmd.arg(a);
    }
    cmd.current_dir(chrome_path.parent().unwrap_or(&base))
        // 不继承父进程(GUI)的stdio句柄，避免子进程启动异常退出
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    Ok(())
}

// ---------- 入口 ----------

/// 当前应用版本号
#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

const GITHUB_REPO: &str = "ahua8522/ChromeTester";

/// 检查 GitHub 最新 Release，与当前版本比较
#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let releases_page = format!("https://github.com/{GITHUB_REPO}/releases");
    let api = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");

    let resp = http_client()
        .get(&api)
        .header("User-Agent", "ChromeTester")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    // 尚未发布任何 Release
    if resp.status().as_u16() == 404 {
        return Ok(UpdateInfo {
            current: current.clone(),
            latest: current,
            has_update: false,
            url: releases_page,
            notes: "远端尚无正式发布".into(),
        });
    }
    if !resp.status().is_success() {
        return Err(format!("检查失败: HTTP {}", resp.status()));
    }

    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = data["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let url = data["html_url"].as_str().unwrap_or(&releases_page).to_string();
    let notes: String = data["body"].as_str().unwrap_or("").chars().take(400).collect();
    let has_update = !latest.is_empty() && version_key(&latest) > version_key(&current);

    Ok(UpdateInfo {
        current,
        latest,
        has_update,
        url,
        notes,
    })
}

/// 在默认浏览器中打开链接（仅允许 github.com）
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://github.com/") {
        return Err("非法链接".into());
    }
    #[cfg(target_os = "windows")]
    let r = Command::new("explorer").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let r = Command::new("open").arg(&url).spawn();
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let r = Command::new("xdg-open").arg(&url).spawn();
    r.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_available_versions,
            refresh_available_versions,
            get_installed_versions,
            get_chromium_milestones,
            refresh_chromium_milestones,
            get_download_source,
            set_download_source,
            download_version,
            download_chromium,
            delete_version,
            open_version_folder,
            get_install_dir,
            pick_install_dir,
            set_install_dir,
            open_install_dir,
            list_profiles,
            create_profile,
            update_profile,
            delete_profile,
            launch_chrome,
            get_app_version,
            check_update,
            open_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
