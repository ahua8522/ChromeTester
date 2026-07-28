# ChromeTester

> 一个轻量级的 Chrome / Chromium 多版本下载、管理与启动工具，用于跨版本的兼容性与渲染测试。

ChromeTester 是一款桌面客户端，帮助前端开发者、测试工程师在同一台机器上便捷地下载、管理并启动**多个不同版本**的 Chrome / Chromium，并通过独立的数据配置（Profile）实现多环境、多账号的隔离测试。

---

## 目录

- [功能特性](#功能特性)
- [技术方案](#技术方案)
- [数据来源与合规说明](#数据来源与合规说明)
- [目录结构](#目录结构)
- [环境要求](#环境要求)
- [开发与构建](#开发与构建)
- [数据存储位置](#数据存储位置)
- [常见问题（FAQ）](#常见问题faq)
- [开源许可](#开源许可)
- [免责声明](#免责声明)
- [致谢](#致谢)

---

## 功能特性

- **多版本下载**：在线浏览并下载指定版本的 Chrome / Chromium。
- **两个数据源**：
  - **Chrome for Testing**（M113 及之后，约 2023 年至今的正式构建）。
  - **Chromium 历史快照**（M59 ~ M112，覆盖更早期版本）。
- **大版本分组浏览**：左侧按大版本（M152、M151…）分组导航，右侧展示该主版本下的所有小版本。
- **国内镜像加速**：内置 `npmmirror`（淘宝）公共镜像作为可切换的下载源，国内网络无需代理即可直连下载。
- **本地版本库管理**：查看已安装版本、显示磁盘路径、一键在文件管理器中打开、删除。
- **数据隔离（Profile）**：为不同版本/场景创建独立的浏览器数据目录，实现 Cookie、书签、登录状态、缓存、扩展互不干扰。
- **自定义安装路径**：可将版本库存放到任意目录。
- **原生桌面体验**：独立窗口、系统托盘图标级别的轻量占用（内存约 30–50MB）。

---

## 技术方案

| 维度 | 选型 | 说明 |
|------|------|------|
| 应用框架 | **Tauri 2.x** | 复用系统自带 WebView2 渲染前端，不捆绑浏览器内核，安装包小、内存低 |
| 后端 | **Rust** | 负责下载、解压、进程管理、配置读写；编译为原生二进制 |
| 前端 | **原生 HTML / CSS / JavaScript** | 无构建工具、无框架依赖，静态文件直接加载 |
| 前后端通信 | **Tauri IPC**（`invoke` / `event`） | 替代本地 HTTP 服务，无端口占用、无跨站风险 |
| 下载 | **reqwest**（流式下载 + 进度事件） | 支持系统代理自动识别 |
| 解压 | **zip** | 解压浏览器压缩包 |
| 进程/系统 | **sysinfo**、**rfd**、**winreg** | 进程管理、原生文件夹选择、读取 Windows 系统代理 |

### 架构概览

```
┌──────────────────────────────────────────────┐
│              ChromeTester（单进程）             │
├──────────────────────────────────────────────┤
│  WebView2 前端（ui/）                          │
│    可用版本 / 已安装 / 配置管理                  │
│                  │ invoke() / listen()          │
│                  ▼                              │
│  Rust Commands（IPC 层）                        │
│    版本列表 / 下载 / 删除 / 启动 / Profile 管理  │
│                  ▼                              │
│  核心逻辑（reqwest 下载、zip 解压、进程启动）     │
│                  ▼                              │
│  数据层（%APPDATA%）                            │
│    chrome/（版本库）  profiles/（数据隔离目录）   │
└──────────────────────────────────────────────┘
```

### 关键实现说明

- **下载源切换**：配置持久化在 `config.json` 的 `download_source` 字段，可在「官方 Google」与「国内镜像 npmmirror」之间切换。
- **系统代理支持**：`reqwest` 默认只读环境变量代理；本工具额外读取 Windows 注册表中的系统代理设置（Internet 选项）并注入 HTTP 客户端，避免"命令行能通、应用内请求挂起"的问题。
- **历史里程碑缓存**：Chromium 历史版本的「里程碑 → 分支点」映射来自 Google 的 `chromiumdash`，首次成功获取后缓存到本地，之后可离线/免代理使用（历史里程碑数据不会变化）。
- **多实例启动稳健性**：每个 Profile 对应独立的 `--user-data-dir`；启动前会结束仍占用该 Profile 的旧进程（严格按完整 `--user-data-dir` 参数精确匹配，不会影响用户自己安装的 Chrome），从而解决"关闭浏览器后再次启动无反应"的单例冲突问题。

---

## 数据来源与合规说明

> **本工具是"下载器 + 启动器"，本身不打包、不再分发任何 Chrome / Chromium 二进制文件。** 所有浏览器文件都是在用户本机运行时，从下列**公开的官方源或公共镜像**下载的。

| 数据源 | 地址 | 提供方 | 用途 |
|--------|------|--------|------|
| Chrome for Testing | `https://googlechromelabs.github.io/chrome-for-testing/` · `https://storage.googleapis.com/chrome-for-testing-public/` | Google | 官方为自动化测试公开提供的 Chrome 构建 |
| Chromium 快照 | `https://commondatastorage.googleapis.com/chromium-browser-snapshots/` | Chromium 项目 / Google | Chromium 开源项目的持续构建快照 |
| 里程碑数据 | `https://chromiumdash.appspot.com/` | Google | Chromium 版本里程碑与分支点信息 |
| 国内镜像 | `https://registry.npmmirror.com/-/binary/` | 阿里巴巴（npmmirror / 淘宝镜像） | 上述 Google 存储桶的公共镜像 |

### 为什么这样设计以规避商业/法律风险

1. **不再分发二进制**：仓库与安装包中不包含任何 Chrome / Chromium 可执行文件，只包含本工具自身的代码。这避免了再分发第三方软件带来的授权问题。
2. **仅使用公开官方渠道**：
   - **Chrome for Testing** 是 Google 明确面向"自动化测试"场景公开发布的构建，任何人可自由下载。
   - **Chromium** 是采用 **BSD-3-Clause** 许可的开源项目，其快照构建同样公开可取。
3. **镜像为公共服务**：`npmmirror` 是阿里巴巴运营的公共镜像，仅作为下载加速的可选项，镜像的仍是上述官方文件。
4. **商标合规**：`Google Chrome`、`Chromium` 等名称与标识是 **Google LLC** 的商标。本项目为**独立的、非官方**工具，与 Google **无任何隶属、赞助或背书关系**。
5. **图标为原创**：应用图标为程序化绘制的原创图形，并非直接使用 Chrome / Chromium 官方 Logo。

> ⚠️ **命名提示**：产品名 `ChromeTester` 含有 "Chrome" 一词，属于对功能的描述性使用。若计划商用或大规模分发，建议咨询法律意见，或考虑更中性的名称（如 `BrowserTester`、`ChromiumHub` 等）以进一步降低商标风险。

---

## 目录结构

```
ChromeTester/
├── ui/                       # 前端（无需构建）
│   ├── index.html            # 界面结构与样式
│   └── app.js                # 前端逻辑（Tauri IPC）
├── src-tauri/
│   ├── Cargo.toml            # Rust 依赖
│   ├── tauri.conf.json       # Tauri 应用配置
│   ├── build.rs
│   ├── capabilities/         # IPC 权限声明
│   ├── icons/                # 应用图标
│   └── src/
│       ├── main.rs           # 入口
│       └── lib.rs            # 核心逻辑与全部 Command
├── make-icon.ps1             # 程序化生成图标脚本（可选）
└── README.md
```

---

## 环境要求

- **Rust** 工具链（1.7x 及以上）
- **Tauri CLI 2.x**：`cargo install tauri-cli --version "^2"`
- **Windows 10/11**：系统自带 WebView2 运行时（较老系统由安装包自动引导安装）
- 构建 Windows 安装包还需 **Visual Studio Build Tools**（C++ 生成工具）

> 目前主要面向 Windows 平台；代码中已为 macOS / Linux 预留路径处理，但未做完整验证。

---

## 开发与构建

```bash
# 开发模式（首次编译较慢，之后为增量编译）
cargo tauri dev

# 构建发布版
cargo tauri build
# 产物：
#   src-tauri/target/release/chrometester.exe            （绿色单文件）
#   src-tauri/target/release/bundle/nsis/*.exe           （NSIS 安装包）
```

替换应用图标：

```bash
# 用一张 png 生成全套图标
cargo tauri icon 你的图标.png
# 或运行仓库内的程序化绘制脚本
pwsh -File make-icon.ps1
```

---

## 数据存储位置

所有运行时数据存放在系统标准应用数据目录：

```
%APPDATA%\com.cvm.app\
├── chrome/                 # 已下载的版本库（可在"配置管理"中改为自定义路径）
├── profiles/               # 各配置（Profile）的独立浏览器数据
├── config.json             # 下载源、安装路径等设置
└── milestones-cache.json   # Chromium 里程碑缓存
```

> 说明：内部标识符（identifier）当前仍为 `com.cvm.app`，因此数据目录沿用该名称。若希望与新名称一致，可修改 `tauri.conf.json` 中的 `identifier`，但会改变数据目录位置（原有下载需手动迁移）。

---

## 常见问题（FAQ）

**Q：配置管理（Profile）是干什么用的？创建配置后好像没别的操作？**
A：每个"配置"就是一份**独立的浏览器数据目录**（`--user-data-dir`）。它的价值体现在**启动时**——在「已安装」页启动某个版本时，用下拉框选择使用哪个配置，即可让不同配置之间的 Cookie、书签、登录状态、缓存、扩展互相隔离。例如：用 `work` 配置登录工作账号、用 `test` 配置跑测试，互不干扰。`default` 为内置默认配置，首次启动自动创建。

**Q：点击"启动"后，关闭浏览器再点一次没反应？**
A：这是 Chrome 的"单例"机制导致的——同一数据目录若仍有未完全退出的进程，新启动会被转发给旧进程而不弹新窗口。本工具已在启动前自动结束占用该配置的旧进程来规避该问题。副作用是：对**正在运行**的同一配置再次点击"启动"，会重启该实例。

**Q：搜不到很老的版本（如 65.x）？**
A：Chrome for Testing 只提供 M113（2023 年）及之后的版本。更早的版本请切换到「Chromium 历史版本」数据源（M59 ~ M112）。注意 Chromium 快照为开源构建，不含 Chrome 品牌功能与部分私有音视频编解码器，适合做渲染 / 兼容性测试。

**Q：下载很慢或失败？**
A：官方 Google 源在国内通常需要代理。可在「配置管理 → 下载源」切换到「国内镜像 npmmirror」，直连下载无需代理。（Chromium 历史版本的里程碑列表首次仍需一次可访问 Google 的网络以建立缓存。）

---

## 开源许可

建议以 **MIT License** 开源本项目自身的代码（仓库根目录可添加 `LICENSE` 文件）。

请注意：本许可仅适用于 **ChromeTester 自身的源代码**。通过本工具下载的 Chrome / Chromium 二进制文件各自遵循其对应的许可与条款（Chromium 为 BSD-3-Clause；Chrome for Testing 遵循 Google 的相关条款）。

---

## 免责声明

- 本项目为个人 / 社区维护的**非官方**开源工具，**与 Google LLC 无任何隶属、赞助或背书关系**。
- `Google Chrome`、`Chromium` 及相关标识为其各自权利人的商标。
- 本工具仅从公开的官方渠道或公共镜像下载浏览器文件，**不对所下载文件的可用性、安全性、合规性作任何担保**。
- 请在遵守所在地法律法规及相关服务条款的前提下使用本工具，使用风险由使用者自行承担。

---

## 致谢

- [Tauri](https://tauri.app/) — 轻量跨平台桌面应用框架
- [Chrome for Testing](https://developer.chrome.com/blog/chrome-for-testing/) — Google 面向测试的官方构建
- [The Chromium Project](https://www.chromium.org/) — 开源浏览器项目
- [npmmirror（淘宝镜像）](https://npmmirror.com/) — 公共二进制镜像服务
