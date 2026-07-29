// ChromeTester 前端逻辑（Tauri IPC）
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---------- 状态 ----------
let chromeVersions = [];     // [{version, download_url}]
let milestones = [];         // [{milestone, position, snapshot_url}]
let installed = [];          // [{version, path}]
let profiles = [];           // [{name, path, args}]
let kind = 'chrome';         // chrome | chromium
let selectedMajor = null;
const downloading = {};       // label -> {status, percent}

// ---------- 工具 ----------
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

let toastTimer = null;
function toast(msg) {
    const t = $('toast');
    t.textContent = msg;
    t.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => t.classList.remove('show'), 2600);
}

async function copyText(text) {
    try { await navigator.clipboard.writeText(text); toast('已复制：' + text); }
    catch { toast('复制失败'); }
}

// 自定义确认弹框（返回 Promise<boolean>）
function confirmDialog(message, title = '确认') {
    return new Promise((resolve) => {
        const ov = $('modalOverlay'), ok = $('modalOk'), cancel = $('modalCancel');
        $('modalTitle').textContent = title;
        $('modalMsg').textContent = message;
        ov.classList.add('show');
        const done = (val) => {
            ov.classList.remove('show');
            ok.removeEventListener('click', onOk);
            cancel.removeEventListener('click', onCancel);
            ov.removeEventListener('mousedown', onBg);
            document.removeEventListener('keydown', onKey);
            resolve(val);
        };
        const onOk = () => done(true);
        const onCancel = () => done(false);
        const onBg = (e) => { if (e.target === ov) done(false); };
        const onKey = (e) => { if (e.key === 'Escape') done(false); else if (e.key === 'Enter') done(true); };
        ok.addEventListener('click', onOk);
        cancel.addEventListener('click', onCancel);
        ov.addEventListener('mousedown', onBg);
        document.addEventListener('keydown', onKey);
        ok.focus();
    });
}

// ---------- 主题（跟随系统 / 浅 / 深） ----------
const ICONS = {
    system: '<path d="M4 5h16v10H4zM8 20h8M12 15v5" stroke-linecap="round" stroke-linejoin="round"/>',
    light: '<circle cx="12" cy="12" r="4.2"/><path d="M12 2v2.5M12 19.5V22M2 12h2.5M19.5 12H22M4.9 4.9l1.8 1.8M17.3 17.3l1.8 1.8M19.1 4.9l-1.8 1.8M6.7 17.3l-1.8 1.8" stroke-linecap="round"/>',
    dark: '<path d="M21 12.8A8.5 8.5 0 1 1 11.2 3a6.6 6.6 0 0 0 9.8 9.8z" stroke-linejoin="round"/>',
};
const THEME_LABEL = { system: '跟随系统', light: '浅色', dark: '深色' };
const mql = window.matchMedia('(prefers-color-scheme: light)');

function themeMode() { return localStorage.getItem('ct-theme') || 'system'; }
function applyTheme() {
    const mode = themeMode();
    const eff = mode === 'system' ? (mql.matches ? 'light' : 'dark') : mode;
    document.documentElement.setAttribute('data-theme', eff);
    $('themeIcon').innerHTML = ICONS[mode];
    $('themeLabel').textContent = THEME_LABEL[mode];
}
$('themeToggle').addEventListener('click', () => {
    const next = { system: 'light', light: 'dark', dark: 'system' }[themeMode()];
    if (next === 'system') localStorage.removeItem('ct-theme');
    else localStorage.setItem('ct-theme', next);
    applyTheme();
});
mql.addEventListener('change', () => { if (themeMode() === 'system') applyTheme(); });
applyTheme();

// ---------- 侧边栏导航 ----------
document.querySelectorAll('.nav-item').forEach((btn) => {
    btn.addEventListener('click', () => {
        document.querySelectorAll('.nav-item').forEach((b) => b.classList.remove('active'));
        document.querySelectorAll('.panel').forEach((p) => p.classList.remove('active'));
        btn.classList.add('active');
        $(btn.dataset.panel).classList.add('active');
    });
});

// ---------- 数据源类型（Chrome / Chromium） ----------
$('kindSeg').querySelectorAll('button').forEach((b) => {
    b.addEventListener('click', () => {
        if (b.dataset.kind === kind) return;
        kind = b.dataset.kind;
        $('kindSeg').querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
        $('availableSub').textContent = kind === 'chrome'
            ? '选择版本下载。数据来自本地快照，点刷新获取最新。'
            : 'Chromium 历史版本 (M59–M112)，开源构建，适合渲染/兼容性测试。';
        $('searchAvailable').placeholder = kind === 'chrome' ? '搜索版本号 (如 114 / 100.0.4896)' : '搜索主版本 (如 65 / 100)';
        selectedMajor = null;
        renderAvailable();
    });
});

// ---------- 可用版本：加载 & 渲染 ----------
function updateAvailableCount() {
    $('cntAvailable').textContent = chromeVersions.length + milestones.length;
}
async function loadAvailable() {
    try { chromeVersions = await invoke('get_available_versions'); }
    catch (e) { toast('加载版本失败: ' + e); }
    updateAvailableCount();
    renderAvailable();
}
async function loadMilestones() {
    try { milestones = await invoke('get_chromium_milestones'); }
    catch (e) { toast('加载里程碑失败: ' + e); }
    updateAvailableCount();
    renderAvailable();
}

$('refreshBtn').addEventListener('click', async () => {
    const btn = $('refreshBtn');
    if (btn.classList.contains('spin')) return;
    btn.classList.add('spin');
    try {
        if (kind === 'chrome') {
            chromeVersions = await invoke('refresh_available_versions');
            toast('版本列表已更新（' + chromeVersions.length + '）');
        } else {
            milestones = await invoke('refresh_chromium_milestones');
            toast('里程碑已更新（' + milestones.length + '）');
        }
        updateAvailableCount();
        renderAvailable();
    } catch (e) {
        toast('刷新失败: ' + e);
    } finally {
        btn.classList.remove('spin');
    }
});

$('searchAvailable').addEventListener('input', () => { selectedMajor = null; renderAvailable(); });

const isInstalled = (label) => installed.some((v) => v.version === label);

function filteredChrome() {
    const kw = $('searchAvailable').value.trim().toLowerCase();
    return kw ? chromeVersions.filter((v) => v.version.includes(kw)) : chromeVersions;
}
function filteredMilestones() {
    const kw = $('searchAvailable').value.trim().toLowerCase();
    return kw ? milestones.filter((m) => String(m.milestone).includes(kw)) : milestones;
}

function renderAvailable() {
    const majors = $('majorList');
    if (kind === 'chrome') {
        majors.style.display = '';
        renderChrome();
    } else {
        majors.style.display = 'none';
        majors.innerHTML = '';
        renderChromium();
    }
}

function renderChrome() {
    const groups = new Map();
    for (const v of filteredChrome()) {
        const mj = v.version.split('.')[0];
        if (!groups.has(mj)) groups.set(mj, []);
        groups.get(mj).push(v);
    }
    const keys = [...groups.keys()];
    const majors = $('majorList');
    if (keys.length === 0) {
        majors.innerHTML = '';
        $('availableList').innerHTML = emptyBox('📦', '没有匹配的版本');
        return;
    }
    if (!selectedMajor || !groups.has(selectedMajor)) selectedMajor = keys[0];
    majors.innerHTML = keys.map((k) =>
        `<div class="major ${k === selectedMajor ? 'on' : ''}" data-mj="${k}">
            <span>M${k}</span><span class="n">${groups.get(k).length}</span></div>`).join('');
    majors.querySelectorAll('.major').forEach((el) =>
        el.addEventListener('click', () => { selectedMajor = el.dataset.mj; renderChrome(); }));

    $('availableList').innerHTML = groups.get(selectedMajor).map((v) => {
        const inst = isInstalled(v.version);
        return `<div class="row">
            <div class="main">
                <div class="ver">${v.version}${inst ? '<span class="tag installed">已安装</span>' : ''}</div>
                <div class="meta" data-copy="${esc(v.download_url)}" title="点击复制下载地址">🔗 ${esc(v.download_url)}</div>
            </div>
            <div class="actions" id="act-${cssId(v.version)}">${actionHtml(v.version, v.version, v.download_url, null, null)}</div>
        </div>`;
    }).join('');
    bindRowEvents();
}

function renderChromium() {
    const list = filteredMilestones();
    if (milestones.length === 0) {
        $('availableList').innerHTML = `<div class="load-hint">正在加载里程碑…</div>`;
        return;
    }
    if (list.length === 0) { $('availableList').innerHTML = emptyBox('📦', '没有匹配的主版本'); return; }
    $('availableList').innerHTML = list.map((m) => {
        const label = 'chromium-M' + m.milestone;
        const inst = isInstalled(label);
        return `<div class="row">
            <div class="main">
                <div class="ver">M${m.milestone}<span class="tag kind">Chromium</span>${inst ? '<span class="tag installed">已安装</span>' : ''}</div>
                <div class="meta" data-copy="${esc(m.snapshot_url)}" title="点击复制快照地址">🔗 ${esc(m.snapshot_url)}</div>
            </div>
            <div class="actions" id="act-${cssId(label)}">${actionHtml(label, label, null, m.milestone, m.position)}</div>
        </div>`;
    }).join('');
    bindRowEvents();
}

// action 区：进度 / 启动 / 下载
function actionHtml(label, version, url, milestone, position) {
    const d = downloading[label];
    if (d) {
        const txt = d.status === 'extracting' ? '解压中' : d.percent + '%';
        return `<div class="prog"><div class="bar"><div class="fill" id="fill-${cssId(label)}" style="width:${d.percent}%"></div></div>
            <span class="pct" id="pct-${cssId(label)}">${txt}</span></div>`;
    }
    if (isInstalled(label)) {
        return `<button class="btn ok" data-launch="${esc(label)}">启动</button>`;
    }
    if (url) return `<button class="btn primary" data-dl="${esc(version)}" data-url="${esc(url)}">下载</button>`;
    return `<button class="btn primary" data-dlc="${milestone}" data-pos="${position}">下载</button>`;
}

const cssId = (s) => s.replace(/[^a-zA-Z0-9]/g, '_');

function bindRowEvents() {
    document.querySelectorAll('#availableList [data-copy]').forEach((el) =>
        el.addEventListener('click', () => copyText(el.dataset.copy)));
    document.querySelectorAll('#availableList [data-dl]').forEach((el) =>
        el.addEventListener('click', () => downloadChrome(el.dataset.dl, el.dataset.url)));
    document.querySelectorAll('#availableList [data-dlc]').forEach((el) =>
        el.addEventListener('click', () => downloadChromium(+el.dataset.dlc, +el.dataset.pos)));
    document.querySelectorAll('#availableList [data-launch]').forEach((el) =>
        el.addEventListener('click', () => launch(el.dataset.launch)));
}

function emptyBox(icon, text, retry) {
    return `<div class="empty"><div class="big">${icon}</div><div>${text}</div>${retry || ''}</div>`;
}

// ---------- 下载 ----------
async function downloadChrome(version, url) {
    if (downloading[version]) return;
    downloading[version] = { status: 'downloading', percent: 0 };
    renderAvailable();
    toast('开始下载 ' + version);
    try { await invoke('download_version', { version, downloadUrl: url }); }
    catch (e) { delete downloading[version]; renderAvailable(); toast('下载失败: ' + e); }
}
async function downloadChromium(milestone, position) {
    const label = 'chromium-M' + milestone;
    if (downloading[label]) return;
    downloading[label] = { status: 'downloading', percent: 0 };
    renderAvailable();
    toast('开始下载 Chromium M' + milestone);
    try { await invoke('download_chromium', { milestone, position }); }
    catch (e) { delete downloading[label]; renderAvailable(); toast('下载失败: ' + e); }
}

listen('download-progress', (ev) => {
    const { version: label, status, percent, error } = ev.payload;
    if (status === 'completed') { delete downloading[label]; toast(label + ' 下载完成'); loadInstalled(); return; }
    if (status === 'error') { delete downloading[label]; renderAvailable(); toast('下载失败: ' + error); return; }
    const prev = downloading[label];
    downloading[label] = { status, percent };
    if (!prev || prev.status !== status) { renderAvailable(); return; }
    const fill = $('fill-' + cssId(label));
    const pct = $('pct-' + cssId(label));
    if (fill) fill.style.width = percent + '%';
    if (pct) pct.textContent = status === 'extracting' ? '解压中' : percent + '%';
});

// ---------- 已安装 ----------
async function loadInstalled() {
    try { installed = await invoke('get_installed_versions'); }
    catch (e) { toast('加载已安装失败: ' + e); }
    $('cntInstalled').textContent = installed.length;
    renderInstalled();
    renderAvailable();
}
$('searchInstalled').addEventListener('input', renderInstalled);

function renderInstalled() {
    const kw = $('searchInstalled').value.trim().toLowerCase();
    const list = kw ? installed.filter((v) => v.version.toLowerCase().includes(kw)) : installed;
    if (list.length === 0) { $('installedList').innerHTML = emptyBox('📭', '暂无已安装版本'); return; }
    const opts = profiles.map((p) => `<option value="${esc(p.name)}">${esc(p.name)}</option>`).join('');
    $('installedList').innerHTML = list.map((v) => `
        <div class="row">
            <div class="main">
                <div class="ver">${esc(v.version)}<span class="tag installed">已安装</span></div>
                <div class="meta" data-open="${esc(v.version)}" title="点击在文件管理器中打开">📁 ${esc(v.path)}</div>
            </div>
            <div class="actions">
                <select class="mini" id="prof-${cssId(v.version)}">${opts}</select>
                <button class="btn ok" data-launch="${esc(v.version)}">启动</button>
                <button class="btn danger" data-del="${esc(v.version)}" title="删除">删除</button>
            </div>
        </div>`).join('');
    $('installedList').querySelectorAll('[data-open]').forEach((el) =>
        el.addEventListener('click', () => openFolder(el.dataset.open)));
    $('installedList').querySelectorAll('[data-launch]').forEach((el) =>
        el.addEventListener('click', () => launch(el.dataset.launch)));
    $('installedList').querySelectorAll('[data-del]').forEach((el) =>
        el.addEventListener('click', () => delVersion(el.dataset.del)));
}

async function launch(version) {
    const sel = $('prof-' + cssId(version));
    const profile = sel ? sel.value : 'default';
    try { await invoke('launch_chrome', { version, profile }); toast('已启动 ' + version); }
    catch (e) { toast('启动失败: ' + e); }
}
async function openFolder(version) {
    try { await invoke('open_version_folder', { version }); }
    catch (e) { toast('打开失败: ' + e); }
}
async function delVersion(version) {
    if (!(await confirmDialog(`确定删除 ${version}？`, '删除版本'))) return;
    try { await invoke('delete_version', { version }); toast('已删除'); loadInstalled(); }
    catch (e) { toast('删除失败: ' + e); }
}

// ---------- 下载源 ----------
async function loadSource() {
    let src = 'google';
    try { src = await invoke('get_download_source'); } catch {}
    $('sourceSeg').querySelectorAll('button').forEach((b) => b.classList.toggle('on', b.dataset.src === src));
}
$('sourceSeg').querySelectorAll('button').forEach((b) => {
    b.addEventListener('click', async () => {
        if (b.classList.contains('on')) return;
        try {
            await invoke('set_download_source', { source: b.dataset.src });
            $('sourceSeg').querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
            toast(b.dataset.src === 'npmmirror' ? '已切换到国内镜像' : '已切换到 Google 官方源');
            // 重新用新源构造下载地址（本地即时）
            await loadAvailable();
            await loadMilestones();
        } catch (e) { toast('切换失败: ' + e); }
    });
});

// ---------- 安装路径 ----------
async function loadInstallDir() {
    try {
        const info = await invoke('get_install_dir');
        $('installDir').textContent = info.path;
        $('dirBadge').style.display = info.is_custom ? '' : 'none';
    } catch (e) { $('installDir').textContent = '读取失败: ' + e; }
}
$('installDir').addEventListener('click', async () => {
    try { await invoke('open_install_dir'); } catch (e) { toast('打开失败: ' + e); }
});
$('changeDir').addEventListener('click', async () => {
    try {
        const picked = await invoke('pick_install_dir');
        if (!picked) return;
        await invoke('set_install_dir', { path: picked });
        toast('安装路径已更新');
        loadInstallDir(); loadInstalled();
    } catch (e) { toast('设置失败: ' + e); }
});
$('resetDir').addEventListener('click', async () => {
    try { await invoke('set_install_dir', { path: '' }); toast('已恢复默认路径'); loadInstallDir(); loadInstalled(); }
    catch (e) { toast('设置失败: ' + e); }
});

// ---------- 浏览器配置（Profile） ----------
async function loadProfiles() {
    try { profiles = await invoke('list_profiles'); } catch (e) { toast('加载配置失败: ' + e); profiles = []; }
    $('cntProfiles').textContent = profiles.length;
    renderProfiles();
    renderInstalled();
}

function renderProfiles() {
    $('profileList').innerHTML = profiles.map((p) => {
        const isDefault = p.name === 'default';
        const preview = p.args.length ? esc(p.args.join('  ')) : '无自定义参数';
        return `<div class="profile" data-name="${esc(p.name)}">
            <div class="profile-head">
                <span class="nm">${esc(p.name)}${isDefault ? '<span class="tag kind" style="margin-left:8px">默认</span>' : ''}</span>
                <span class="profile-args-preview">${preview}</span>
                <button class="btn ghost" data-edit="${esc(p.name)}">编辑参数</button>
                ${isDefault ? '' : `<button class="btn danger" data-delp="${esc(p.name)}">删除</button>`}
            </div>
            <div class="profile-edit">
                <label>自定义启动参数（每行一个，如 --lang=en-US、--proxy-server=127.0.0.1:7897）</label>
                <textarea data-args="${esc(p.name)}">${esc(p.args.join('\n'))}</textarea>
                <div class="card-actions">
                    <button class="btn primary" data-save="${esc(p.name)}">保存</button>
                    <button class="btn ghost" data-cancel="${esc(p.name)}">取消</button>
                </div>
            </div>
        </div>`;
    }).join('');

    const byName = (n) => $('profileList').querySelector(`.profile[data-name="${CSS.escape(n)}"]`);
    $('profileList').querySelectorAll('[data-edit]').forEach((el) =>
        el.addEventListener('click', () => byName(el.dataset.edit).classList.toggle('editing')));
    $('profileList').querySelectorAll('[data-cancel]').forEach((el) =>
        el.addEventListener('click', () => { byName(el.dataset.cancel).classList.remove('editing'); renderProfiles(); }));
    $('profileList').querySelectorAll('[data-save]').forEach((el) =>
        el.addEventListener('click', () => saveProfile(el.dataset.save)));
    $('profileList').querySelectorAll('[data-delp]').forEach((el) =>
        el.addEventListener('click', () => delProfile(el.dataset.delp)));
}

async function saveProfile(name) {
    const ta = $('profileList').querySelector(`textarea[data-args="${CSS.escape(name)}"]`);
    const args = ta.value.split('\n').map((s) => s.trim()).filter(Boolean);
    try { await invoke('update_profile', { name, args }); toast('配置已保存'); loadProfiles(); }
    catch (e) { toast('保存失败: ' + e); }
}
async function delProfile(name) {
    if (!(await confirmDialog(`确定删除配置 “${name}”？其独立数据也会被删除。`, '删除配置'))) return;
    try { await invoke('delete_profile', { name }); toast('配置已删除'); loadProfiles(); }
    catch (e) { toast('删除失败: ' + e); }
}
$('createProfile').addEventListener('click', async () => {
    const name = $('newProfileName').value.trim();
    if (!name) { toast('请输入配置名称'); return; }
    try { await invoke('create_profile', { name, args: [] }); $('newProfileName').value = ''; toast('配置已创建'); loadProfiles(); }
    catch (e) { toast('创建失败: ' + e); }
});

// ---------- 版本号 & 检查更新 ----------
async function loadAppMeta() {
    try { $('appVer').textContent = 'v' + (await invoke('get_app_version')); } catch {}
}
$('checkUpdate').addEventListener('click', async () => {
    const btn = $('checkUpdate');
    btn.textContent = '检查中…';
    try {
        const u = await invoke('check_update');
        if (u.has_update) {
            btn.textContent = '→ v' + u.latest;
            if (await confirmDialog(`发现新版本 v${u.latest}（当前 v${u.current}）。前往下载页？`, '发现新版本')) await invoke('open_url', { url: u.url });
        } else {
            toast(u.notes ? '已是最新 · ' + u.notes : '已是最新 v' + u.current);
            btn.textContent = '已是最新';
            setTimeout(() => { btn.textContent = '检查更新'; }, 3000);
        }
    } catch (e) { toast('检查更新失败: ' + e); btn.textContent = '检查更新'; }
});

// ---------- 初始化 ----------
loadProfiles();
loadInstalled();
loadAvailable();
loadMilestones();
loadSource();
loadInstallDir();
loadAppMeta();
