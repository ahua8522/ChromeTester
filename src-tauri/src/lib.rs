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
}

#[derive(Serialize, Clone)]
pub struct DownloadProgress {
    version: String,
    status: String, // downloading | extracting | completed | error
    percent: u32,
    error: Option<String>,
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

#[tauri::command]
async fn get_available_versions(app: AppHandle) -> Result<Vec<VersionInfo>, String> {
    let platform = current_platform();
    let mut versions = if download_source(&app) == "npmmirror" {
        fetch_cft_versions_npm(platform).await?
    } else {
        fetch_cft_versions_google(platform).await?
    };
    // 按版本号倒序排列
    versions.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    Ok(versions)
}

/// Google 官方 Chrome for Testing 版本列表
async fn fetch_cft_versions_google(platform: &str) -> Result<Vec<VersionInfo>, String> {
    let resp = http_client()
        .get(CFT_API)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    let empty = vec![];
    Ok(data["versions"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| {
            let version = v["version"].as_str()?.to_string();
            let url = v["downloads"]["chrome"]
                .as_array()?
                .iter()
                .find(|d| d["platform"].as_str() == Some(platform))?["url"]
                .as_str()?
                .to_string();
            Some(VersionInfo {
                version,
                download_url: url,
            })
        })
        .collect())
}

/// npmmirror 镜像 Chrome for Testing 版本列表（从二进制目录列表构建）
async fn fetch_cft_versions_npm(platform: &str) -> Result<Vec<VersionInfo>, String> {
    let resp = http_client()
        .get(NPM_CFT_BASE)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    let empty = vec![];
    Ok(data
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|e| {
            if e["type"].as_str() != Some("dir") {
                return None;
            }
            let version = e["name"].as_str()?.trim_end_matches('/').to_string();
            if validate_version(&version).is_err() {
                return None;
            }
            let download_url =
                format!("{NPM_CFT_BASE}{version}/{platform}/chrome-{platform}.zip");
            Some(VersionInfo {
                version,
                download_url,
            })
        })
        .collect())
}

#[tauri::command]
fn get_installed_versions(app: AppHandle) -> Result<Vec<InstalledVersion>, String> {
    let dir = chrome_dir(&app)?;
    let mut installed: Vec<InstalledVersion> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            Some(InstalledVersion {
                version: entry.file_name().to_string_lossy().to_string(),
                path: entry.path().to_string_lossy().to_string(),
            })
        })
        .collect();

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
async fn get_chromium_milestones(app: AppHandle) -> Result<Vec<MilestoneInfo>, String> {
    let cache = data_dir(&app)?.join("milestones-cache.json");

    // 里程碑→分支点映射仅 chromiumdash 提供（需梯子）；
    // 成功后缓存到本地，之后无网/镜像模式也能用（历史里程碑不会变）
    let pairs: Vec<(u32, u64)> = match fetch_milestone_pairs().await {
        Ok(p) if !p.is_empty() => {
            let _ = fs::write(&cache, serde_json::to_string(&p).unwrap_or_default());
            p
        }
        _ => fs::read_to_string(&cache)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<(u32, u64)>>(&s).ok())
            .filter(|v| !v.is_empty())
            .ok_or("里程碑列表需一次可访问 chromiumdash 的网络（如梯子）以建立缓存")?,
    };

    let platform = snapshot_platform();
    let npm = download_source(&app) == "npmmirror";
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
    Ok(list)
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
        if let Some(&p) = positions.iter().find(|&&p| p >= target) {
            chosen = Some(p);
            break;
        }
        // 全部小于分支点时退而求其次取最大的
        if let Some(&p) = positions.last() {
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
        .find(|&&p| p >= target)
        .copied()
        .or_else(|| positions.last().copied())
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
    let dir = chrome_dir(&app)?.join(&version);
    if dir.exists() {
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
    let dir = chrome_dir(&app)?.join(&version);
    if !dir.exists() {
        return Err("目录不存在".into());
    }
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

/// 设置安装目录；传空字符串则恢复默认
#[tauri::command]
fn set_install_dir(app: AppHandle, path: String) -> Result<(), String> {
    let mut cfg = load_config(&app);
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
    }
    save_config(&app, &cfg)
}

/// 在文件管理器中打开当前安装目录
#[tauri::command]
fn open_install_dir(app: AppHandle) -> Result<(), String> {
    let dir = chrome_dir(&app)?;
    open_in_explorer(&dir)
}

#[tauri::command]
fn list_profiles(app: AppHandle) -> Result<Vec<ProfileInfo>, String> {
    let dir = profiles_dir(&app)?;
    let profiles = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            Some(ProfileInfo {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry.path().to_string_lossy().to_string(),
            })
        })
        .collect();
    Ok(profiles)
}

#[tauri::command]
fn create_profile(app: AppHandle, name: String) -> Result<(), String> {
    validate_name(&name)?;
    let dir = profiles_dir(&app)?.join(&name);
    if dir.exists() {
        return Err("配置已存在".into());
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_profile(app: AppHandle, name: String) -> Result<(), String> {
    validate_name(&name)?;
    let dir = profiles_dir(&app)?.join(&name);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
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

    let base = chrome_dir(&app)?.join(&version);

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

    // 每个profile独立的user-data-dir实现数据隔离
    let profile_path = profiles_dir(&app)?.join(&profile);
    fs::create_dir_all(&profile_path).map_err(|e| e.to_string())?;

    // 启动前先结束该profile可能残留的旧实例，保证“关闭后再启动”总能打开新窗口
    kill_chrome_using_profile(&profile_path);

    let mut cmd = Command::new(chrome_path);
    cmd.arg(format!("--user-data-dir={}", profile_path.display()))
        .arg("--no-first-run")
        .current_dir(chrome_path.parent().unwrap_or(&base))
        // 不继承父进程(GUI)的stdio句柄，避免子进程启动异常退出
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    Ok(())
}

// ---------- 入口 ----------

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_available_versions,
            get_installed_versions,
            get_chromium_milestones,
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
            delete_profile,
            launch_chrome,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
