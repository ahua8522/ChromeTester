// Chrome版本管理器 - 前端逻辑（Tauri IPC）
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let allVersions = [];
let installedVersions = [];
let allMilestones = [];
// 当前数据源：chrome (M113+) | chromium (历史快照)
let source = 'chrome';
// 当前选中的大版本号（chrome源分组导航）
let selectedMajor = null;
// 下载中状态：version -> { status, percent }
const downloading = {};

// ===== Tab切换 =====
document.querySelectorAll('.tab').forEach(tab => {
    tab.addEventListener('click', () => {
        document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
        document.querySelectorAll('.panel').forEach(p => p.classList.remove('active'));
        tab.classList.add('active');
        document.getElementById(tab.dataset.panel).classList.add('active');
    });
});

// ===== Toast提示 =====
let toastTimer = null;
function showToast(message, duration = 3000) {
    const toast = document.getElementById('toast');
    toast.textContent = message;
    toast.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => toast.classList.remove('show'), duration);
}

// ===== 数据源切换 =====
const SOURCE_HINTS = {
    chrome: '数据源：Chrome for Testing 官方构建，仅提供 M113（2023年）及之后的版本；更早的版本请切换到「Chromium 历史版本」。',
    chromium: '数据源：Chromium 官方快照存档，每个主版本对应分支点构建（非 Chrome 品牌版，无私有编解码器），适合渲染/兼容性测试。',
};

document.querySelectorAll('.source-btn').forEach(btn => {
    btn.addEventListener('click', () => {
        if (btn.dataset.source === source) return;
        document.querySelectorAll('.source-btn').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        source = btn.dataset.source;
        document.getElementById('sourceHint').textContent = SOURCE_HINTS[source];
        document.getElementById('searchAvailable').placeholder = source === 'chrome'
            ? '搜索版本号 (如: 114, 100.0.4896)'
            : '搜索主版本 (如: 65, 100)';
        if (source === 'chromium' && allMilestones.length === 0) {
            loadMilestones();
        }
        renderCurrentSource();
    });
});

// 按当前数据源重绘可用版本列表
function renderCurrentSource() {
    const majorList = document.getElementById('majorList');
    if (source === 'chrome') {
        majorList.style.display = '';
        renderChromeSource();
    } else {
        // Chromium源本身就是主版本粒度，隐藏左侧分组栏
        majorList.style.display = 'none';
        renderMilestones(filteredMilestones());
    }
}

// ===== 可用版本 =====
let chromeState = 'idle'; // idle | loading | loaded | error

async function loadAvailableVersions() {
    if (chromeState === 'loading') return;
    chromeState = 'loading';
    renderCurrentSource();
    try {
        allVersions = await invoke('get_available_versions');
        chromeState = 'loaded';
        document.getElementById('availableCount').textContent = allVersions.length;
    } catch (e) {
        chromeState = 'error';
        showToast('加载版本列表失败: ' + e);
    }
    renderCurrentSource();
}

function filteredAvailable() {
    const keyword = document.getElementById('searchAvailable').value.trim().toLowerCase();
    return keyword ? allVersions.filter(v => v.version.includes(keyword)) : allVersions;
}

function isInstalled(version) {
    return installedVersions.some(v => v.version === version);
}

function actionHtml(v) {
    if (downloading[v.version]) {
        const d = downloading[v.version];
        const text = d.status === 'extracting' ? '解压中...' : d.percent + '%';
        return `
            <div class="progress-wrap">
                <div class="progress-bar"><div class="progress-fill" id="fill-${v.version}" style="width:${d.percent}%"></div></div>
                <span class="progress-text" id="ptext-${v.version}">${text}</span>
            </div>`;
    }
    if (isInstalled(v.version)) {
        return `<button class="btn btn-success" onclick="launchChrome('${v.version}')">启动</button>`;
    }
    return `<button class="btn btn-primary" onclick="downloadVersion('${v.version}', '${v.download_url}')">下载</button>`;
}

// 复制文本到剪贴板
async function copyText(text) {
    try {
        await navigator.clipboard.writeText(text);
        showToast('下载地址已复制');
    } catch (e) {
        // 降级方案
        const ta = document.createElement('textarea');
        ta.value = text;
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        ta.remove();
        showToast('下载地址已复制');
    }
}

function renderAvailableVersions(versions) {
    const list = document.getElementById('availableList');
    if (versions.length === 0) {
        list.innerHTML = '<div class="empty-state"><div class="icon">📦</div>' +
            '<div>没有找到版本</div>' +
            '<div style="margin-top:8px;font-size:13px;">Chrome 源仅含 M113+ 版本，更早的版本请切换上方「Chromium 历史版本」数据源</div></div>';
        return;
    }

    list.innerHTML = versions.map(v => `
        <div class="version-item">
            <div class="version-info">
                <div>
                    <span class="version-num">${v.version}</span>
                    ${isInstalled(v.version) ? '<span class="version-badge installed">已安装</span>' : ''}
                    <div class="version-path" title="点击复制下载地址" onclick="copyText('${v.download_url}')">🔗 ${v.download_url}</div>
                </div>
            </div>
            <div class="actions" id="actions-${v.version}">${actionHtml(v)}</div>
        </div>
    `).join('');
}

// 按大版本号分组（保持版本倒序，Map键按插入顺序即大版本倒序）
function buildGroups(versions) {
    const groups = new Map();
    for (const v of versions) {
        const major = v.version.split('.')[0];
        if (!groups.has(major)) groups.set(major, []);
        groups.get(major).push(v);
    }
    return groups;
}

// Chrome源：左侧大版本导航 + 右侧具体版本列表
function renderChromeSource() {
    const majorList = document.getElementById('majorList');
    const list = document.getElementById('availableList');

    // 数据尚未加载成功时的状态展示
    if (allVersions.length === 0) {
        majorList.innerHTML = '';
        if (chromeState === 'error') {
            list.innerHTML = '<div class="empty-state"><div class="icon">⚠️</div>' +
                '<div>版本列表加载失败，请检查网络/代理</div>' +
                '<button class="btn btn-primary" style="margin-top:14px;" onclick="loadAvailableVersions()">重试</button></div>';
        } else {
            list.innerHTML = '<div class="empty-state"><div class="icon">⏳</div><div>正在加载版本列表...</div></div>';
        }
        return;
    }

    const filtered = filteredAvailable();
    const groups = buildGroups(filtered);
    const majors = [...groups.keys()];

    if (majors.length === 0) {
        majorList.innerHTML = '';
        renderAvailableVersions([]);
        return;
    }

    // 当前选中的大版本不在结果内时，自动切到第一个
    if (!selectedMajor || !groups.has(selectedMajor)) {
        selectedMajor = majors[0];
    }

    majorList.innerHTML = majors.map(m => `
        <div class="major-item ${m === selectedMajor ? 'active' : ''}" onclick="selectMajor('${m}')">
            <span>M${m}</span>
            <span class="major-count">${groups.get(m).length}</span>
        </div>
    `).join('');

    renderAvailableVersions(groups.get(selectedMajor));
}

function selectMajor(major) {
    selectedMajor = major;
    renderChromeSource();
}

// ===== Chromium历史版本（快照源） =====
let milestonesState = 'idle'; // idle | loading | loaded | error

async function loadMilestones() {
    if (milestonesState === 'loading') return;
    milestonesState = 'loading';
    renderCurrentSource();
    try {
        allMilestones = await invoke('get_chromium_milestones');
        milestonesState = 'loaded';
    } catch (e) {
        milestonesState = 'error';
        showToast('加载Chromium历史版本失败: ' + e);
    }
    renderCurrentSource();
}

function filteredMilestones() {
    const keyword = document.getElementById('searchAvailable').value.trim().toLowerCase();
    if (!keyword) return allMilestones;
    return allMilestones.filter(m =>
        String(m.milestone).includes(keyword) || `m${m.milestone}`.includes(keyword));
}

function renderMilestones(milestones) {
    const list = document.getElementById('availableList');
    if (allMilestones.length === 0) {
        if (milestonesState === 'error') {
            list.innerHTML = '<div class="empty-state"><div class="icon">⚠️</div>' +
                '<div>里程碑列表加载失败，请检查网络/代理</div>' +
                '<button class="btn btn-primary" style="margin-top:14px;" onclick="loadMilestones()">重试</button></div>';
        } else {
            list.innerHTML = '<div class="empty-state"><div class="icon">⏳</div><div>正在加载里程碑列表...</div></div>';
        }
        return;
    }
    if (milestones.length === 0) {
        list.innerHTML = '<div class="empty-state"><div class="icon">📦</div><div>没有找到匹配的主版本</div></div>';
        return;
    }

    list.innerHTML = milestones.map(m => {
        const label = `chromium-M${m.milestone}`;
        let action;
        if (downloading[label]) {
            const d = downloading[label];
            const text = d.status === 'extracting' ? '解压中...' : d.percent + '%';
            action = `
            <div class="progress-wrap">
                <div class="progress-bar"><div class="progress-fill" id="fill-${label}" style="width:${d.percent}%"></div></div>
                <span class="progress-text" id="ptext-${label}">${text}</span>
            </div>`;
        } else if (isInstalled(label)) {
            action = `<button class="btn btn-success" onclick="launchChrome('${label}')">启动</button>`;
        } else {
            action = `<button class="btn btn-primary" onclick="downloadChromium(${m.milestone}, ${m.position})">下载</button>`;
        }
        return `
        <div class="version-item">
            <div class="version-info">
                <div>
                    <span class="version-num">M${m.milestone}</span>
                    <span class="version-badge">Chromium</span>
                    ${isInstalled(label) ? '<span class="version-badge installed">已安装</span>' : ''}
                    <div class="version-path" title="点击复制快照地址（下载时自动定位最近构建）" onclick="copyText('${m.snapshot_url}')">🔗 ${m.snapshot_url}</div>
                </div>
            </div>
            <div class="actions">${action}</div>
        </div>`;
    }).join('');
}

async function downloadChromium(milestone, position) {
    const label = `chromium-M${milestone}`;
    if (downloading[label]) return;
    downloading[label] = { status: 'downloading', percent: 0 };
    renderCurrentSource();
    showToast(`开始下载 Chromium M${milestone}...`);

    try {
        await invoke('download_chromium', { milestone, position });
    } catch (e) {
        delete downloading[label];
        renderCurrentSource();
        showToast(`下载失败: ${e}`);
    }
}

// ===== 已安装版本 =====
async function loadInstalledVersions() {
    try {
        installedVersions = await invoke('get_installed_versions');
        document.getElementById('installedCount').textContent = installedVersions.length;
        renderInstalledVersions(filteredInstalled());
        renderCurrentSource();
    } catch (e) {
        showToast('加载已安装版本失败: ' + e);
    }
}

function filteredInstalled() {
    const keyword = document.getElementById('searchInstalled').value.trim().toLowerCase();
    return keyword ? installedVersions.filter(v => v.version.includes(keyword)) : installedVersions;
}

function renderInstalledVersions(versions) {
    const list = document.getElementById('installedList');
    if (versions.length === 0) {
        list.innerHTML = '<div class="empty-state"><div class="icon">📭</div><div>暂无已安装版本</div></div>';
        return;
    }

    list.innerHTML = versions.map(v => `
        <div class="version-item">
            <div class="version-info">
                <div>
                    <span class="version-num">${v.version}</span>
                    <span class="version-badge installed">已安装</span>
                    <div class="version-path" title="点击在文件管理器中打开" onclick="openVersionFolder('${v.version}')">📁 ${v.path}</div>
                </div>
            </div>
            <div class="actions">
                <select class="btn btn-secondary" id="profile-${v.version}">
                    <option value="default">默认配置</option>
                </select>
                <button class="btn btn-success" onclick="launchChrome('${v.version}')">启动</button>
                <button class="btn btn-danger" onclick="deleteVersion('${v.version}')">删除</button>
            </div>
        </div>
    `).join('');

    loadProfileOptions();
}

// 在系统文件管理器中打开版本目录
async function openVersionFolder(version) {
    try {
        await invoke('open_version_folder', { version });
    } catch (e) {
        showToast('打开目录失败: ' + e);
    }
}

// ===== 安装路径设置 =====
async function loadInstallDir() {
    try {
        const info = await invoke('get_install_dir');
        document.getElementById('installDirPath').textContent = '📁 ' + info.path;
        document.getElementById('installDirBadge').style.display = info.is_custom ? '' : 'none';
    } catch (e) {
        document.getElementById('installDirPath').textContent = '读取失败: ' + e;
    }
}

async function changeInstallDir() {
    try {
        const picked = await invoke('pick_install_dir');
        if (!picked) return; // 用户取消
        await invoke('set_install_dir', { path: picked });
        showToast('安装路径已更新');
        loadInstallDir();
        loadInstalledVersions();
    } catch (e) {
        showToast('设置失败: ' + e);
    }
}

async function resetInstallDir() {
    try {
        await invoke('set_install_dir', { path: '' });
        showToast('已恢复默认路径');
        loadInstallDir();
        loadInstalledVersions();
    } catch (e) {
        showToast('设置失败: ' + e);
    }
}

async function openInstallDir() {
    try {
        await invoke('open_install_dir');
    } catch (e) {
        showToast('打开目录失败: ' + e);
    }
}

// ===== 下载源切换 =====
async function loadDownloadSource() {
    try {
        const s = await invoke('get_download_source');
        document.querySelectorAll('[data-dlsource]').forEach(b => {
            b.classList.toggle('active', b.dataset.dlsource === s);
        });
    } catch (e) {
        // 忽略
    }
}

document.querySelectorAll('[data-dlsource]').forEach(btn => {
    btn.addEventListener('click', async () => {
        const s = btn.dataset.dlsource;
        if (btn.classList.contains('active')) return;
        try {
            await invoke('set_download_source', { source: s });
            document.querySelectorAll('[data-dlsource]').forEach(b =>
                b.classList.toggle('active', b === btn));
            showToast(s === 'npmmirror' ? '已切换到国内镜像' : '已切换到 Google 官方源');
            // 重置并重新加载两个数据源
            allVersions = [];
            chromeState = 'idle';
            allMilestones = [];
            milestonesState = 'idle';
            selectedMajor = null;
            loadAvailableVersions();
            if (source === 'chromium') loadMilestones();
        } catch (e) {
            showToast('切换失败: ' + e);
        }
    });
});

// ===== 下载（进度由事件推送） =====
async function downloadVersion(version, url) {
    if (downloading[version]) return;
    downloading[version] = { status: 'downloading', percent: 0 };
    renderAvailableVersions(filteredAvailable());
    showToast(`开始下载 Chrome ${version}...`);

    try {
        // 注意：Rust端snake_case参数在JS端用camelCase
        await invoke('download_version', { version, downloadUrl: url });
    } catch (e) {
        delete downloading[version];
        renderCurrentSource();
        showToast(`下载失败: ${e}`);
    }
}

// 监听后端下载进度事件
listen('download-progress', (event) => {
    const { version, status, percent, error } = event.payload;

    if (status === 'completed') {
        delete downloading[version];
        showToast(`Chrome ${version} 下载完成`);
        loadInstalledVersions();
        return;
    }
    if (status === 'error') {
        delete downloading[version];
        renderCurrentSource();
        showToast(`下载失败: ${error}`);
        return;
    }

    // downloading / extracting：原地更新进度条，避免整表重绘
    const prev = downloading[version];
    downloading[version] = { status, percent };
    if (!prev || prev.status !== status) {
        renderCurrentSource();
        return;
    }
    const fill = document.getElementById(`fill-${version}`);
    const text = document.getElementById(`ptext-${version}`);
    if (fill) fill.style.width = percent + '%';
    if (text) text.textContent = status === 'extracting' ? '解压中...' : percent + '%';
});

// ===== 删除版本 =====
async function deleteVersion(version) {
    if (!confirm(`确定删除 Chrome ${version}？`)) return;
    try {
        await invoke('delete_version', { version });
        showToast('已删除');
        loadInstalledVersions();
    } catch (e) {
        showToast('删除失败: ' + e);
    }
}

// ===== 启动Chrome =====
async function launchChrome(version) {
    const select = document.getElementById(`profile-${version}`);
    const profile = select ? select.value : 'default';
    try {
        await invoke('launch_chrome', { version, profile });
        showToast('Chrome 已启动');
    } catch (e) {
        showToast('启动失败: ' + e);
    }
}

// ===== 配置管理 =====
async function loadProfiles() {
    try {
        const profiles = await invoke('list_profiles');
        // 计数含内置 default（后端列表只包含已创建目录，default 首次启动才创建）
        const userCount = profiles.filter(p => p.name !== 'default').length;
        document.getElementById('profileCount').textContent = userCount + 1;
        renderProfiles(profiles);
        return profiles;
    } catch (e) {
        showToast('加载配置失败: ' + e);
        return [];
    }
}

function renderProfiles(profiles) {
    const list = document.getElementById('profileList');
    // 始终展示内置 default 配置（首次启动自动创建，不可删除）
    const builtin = `
        <div class="version-item">
            <div class="version-info">
                <span class="version-num">default</span>
                <span class="version-badge">内置默认</span>
            </div>
            <div class="actions"><span class="hint" style="margin:0;">启动时自动使用</span></div>
        </div>`;
    const items = profiles
        .filter(p => p.name !== 'default')
        .map(p => `
        <div class="version-item">
            <div class="version-info">
                <span class="version-num">${p.name}</span>
            </div>
            <div class="actions">
                <button class="btn btn-danger" onclick="deleteProfile('${p.name}')">删除</button>
            </div>
        </div>`)
        .join('');
    list.innerHTML = builtin + items;
}

async function createProfile() {
    const name = document.getElementById('newProfileName').value.trim();
    if (!name) {
        showToast('请输入配置名称');
        return;
    }

    try {
        await invoke('create_profile', { name });
        document.getElementById('newProfileName').value = '';
        showToast('配置已创建');
        loadProfiles();
    } catch (e) {
        showToast('创建失败: ' + e);
    }
}

async function deleteProfile(name) {
    if (!confirm(`确定删除配置 "${name}"？`)) return;
    try {
        await invoke('delete_profile', { name });
        showToast('配置已删除');
        loadProfiles();
    } catch (e) {
        showToast('删除失败: ' + e);
    }
}

// 加载配置选项到已安装列表的下拉框
async function loadProfileOptions() {
    const profiles = await loadProfiles();
    document.querySelectorAll('select[id^="profile-"]').forEach(select => {
        const current = select.value;
        select.innerHTML = '<option value="default">默认配置</option>' +
            profiles.filter(p => p.name !== 'default')
                    .map(p => `<option value="${p.name}">${p.name}</option>`).join('');
        select.value = current || 'default';
    });
}

// ===== 搜索过滤 =====
document.getElementById('searchAvailable').addEventListener('input', () => {
    renderCurrentSource();
});

document.getElementById('searchInstalled').addEventListener('input', () => {
    renderInstalledVersions(filteredInstalled());
});

// ===== 初始化 =====
loadInstalledVersions();
loadAvailableVersions();
loadProfiles();
loadInstallDir();
loadDownloadSource();
