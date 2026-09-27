# DSH Plugin Manager

面向 **本地 CLI 版 DSH** 的桌面管理工具（Tauri 2 + React 19 + TypeScript）。在图形界面中完成 DSH 多版本管理、Profile 管理、插件安装与管理、多实例独立启动，无需手敲命令行。

> 仅管理本地 CLI 版 DSH，不包含、不依赖 DSH Desktop。



***

## 功能特性

![功能总览](docs/images/features.png)


### 插件管理



* **在线安装**：内置插件市场（参考 `dsh-market/dsh-market`）、NPM 搜索安装、GitHub 安装、自定义源安装

* **本地安装**：选择**文件夹**（含 `package.json`）或 **.tgz 压缩包**，按 `file:` 协议复制安装到指定 Profile

* **插件列表**：卸载 / 更新（检查 npm 新版本）/ 启用停用（拉杆开关，写 `cordis.patch.yml`）

* **版本豁免**：插件与当前 dsh runtime 的 `peerDependencies` 不兼容时，可显式授权豁免安装

### Profile 管理



* 新建 / 删除 Profile

* **导入 / 导出**为 zip 压缩包（可排除 `node_modules`；重名自动改名）

* **扫描目录**：把本地任意 profiles 目录（如 DSH Desktop 的 `.dsh-packs`）加入列表

### DSH 环境管理



* **下载多版本 DSH** 到指定目录，电脑可同时拥有多个版本

* **扫描本体**：从已安装目录（全局 npm / DSH Desktop 内置 / 本地项目）自动探测 `dsh.cmd` / `bin.js` 并读取版本

* **镜像源设置**：自定义 npm registry（默认 `registry.npmmirror.com`），解决大陆网络下载失败

### 多实例运行



* 任意 **DSH 版本 × 任意 Profile** 可自由组合，各自**独立进程**

* **端口自动分配**（从 3080 起避开已占用），互不冲突

* 每个实例独立 **启动 / 停止 / 重启**；进程退出自动清理、重启按钮自动还原

* 「打开界面」实时从运行日志提取**带 token 的认证地址**，一步到位进入 DSH Web UI

### 体验细节



* 环境与 Profile 列表**拖拽排序**（重启保留）

* **记住上次选择**的 DSH 环境与 Profile

* 左栏 / 中栏标题右侧 `?` 按钮提供板块使用说明

* 全程无命令行黑框（所有后台命令均以无窗口方式执行）



***

## 界面布局

![三栏界面布局](docs/images/layout.png)




| 区域 | 内容                                                   |
| -- | ---------------------------------------------------- |
| 左栏 | DSH 环境（每个 = 一个 DSH 程序本体，可多版本共存）                      |
| 中栏 | Profiles（合并默认 `~/.dsh/profiles` + 扫描目录，不绑定任何 DSH 版本） |
| 右栏 | 当前 Profile 的插件列表与操作、运行控制（启动 / 停止 / 重启 / 打开界面）        |

## 核心概念

![任意 DSH 版本 × 任意 Profile 独立进程](docs/images/concept.png)




* **DSH 环境**：一个 DSH 可执行文件（版本 / 安装位置）。`下载DSH`、`扫本体` 或全局 CLI 自动识别产生。

* **Profile**：一套独立的插件集合与配置（`package.json` + `cordis.patch.yml`），类似 DSH 的 "配置档"。

* **组合启动**：选中任意环境 + 任意 Profile → 启动。应用自动注入 `DSH_HOME` 指向该 Profile 的来源目录，实现任意版本加载任意来源的 Profile。



***

## 快速开始（Windows）

两个产物任选其一（均在 `src-tauri/target/release/` 下）：



1. **便携版单文件 exe**：`dsh-plugin-manager.exe`，绿色免安装，**单文件无 dll**，复制即用

2. **NSIS 安装器**：`bundle/nsis/dsh-plugin-manager_0.1.0_x64-setup.exe`，双击安装

> 依赖系统 
>
> **WebView2 运行时**
>
> （Win10/11 一般自带）。配置数据保存在 
>
> `%AppData%\dsh-plugin-manager`
>
> 。

首次使用建议顺序：下载 / 扫描 DSH → 新建或扫描 Profile → 选中组合 → 启动 → 打开界面。



***

## 开发指南

### 环境要求



* Windows 10/11

* [Rust](https://www.rust-lang.org/)（stable）

* [Node.js](https://nodejs.org/) ≥ 18 与 [pnpm](https://pnpm.io/)

### 本地开发



```
\# 安装依赖

pnpm install

\# 启动前端开发服务（Vite，端口 1420）

pnpm dev

\# 另开终端，启动桌面应用（开发模式）

pnpm tauri dev
```

### 构建 / 打包



```
\# 前端构建

pnpm build

\# 打包（app 裸 exe + NSIS 安装器）

pnpm tauri build
```

### 测试



```
\# 前端单元测试（Vitest + Testing Library）

pnpm test

\# 后端单元测试（Rust）

cd src-tauri && cargo test --lib
```

当前基线：前端 42 项测试、后端 67 项测试（持续更新）。

### 项目结构



```
dshcjaz/

├── src/                    # 前端（React + TS + Vite）

│   ├── App.tsx             # 主界面（三栏布局、运行控制）

│   ├── InstallDialog.tsx   # 插件安装对话框（在线/本地）

│   ├── api.ts              # Tauri invoke 封装

│   ├── reorder.ts          # 拖拽排序工具

│   └── \*.test.tsx          # 前端测试

├── src-tauri/              # 后端（Rust + Tauri 2）

│   └── src/

│       ├── lib.rs          # 命令注册与业务编排

│       ├── runner.rs       # 进程调度：启动/停止/端口探测/token 提取

│       ├── scanner.rs      # 环境扫描、Profile 探测

│       ├── installer.rs    # 插件安装/卸载/豁免

│       ├── market.rs       # 插件市场数据

│       ├── npm.rs          # npm 搜索与更新检查

│       ├── profile\_io.rs   # Profile 导入导出

│       ├── dsh\_install.rs  # 多版本 DSH 下载安装

│       └── settings.rs     # 镜像源等设置持久化

└── 需求文档.md              # 需求与迭代记录
```



***

## 数据与配置



| 数据                    | 位置                                                     |
| --------------------- | ------------------------------------------------------ |
| 应用设置（镜像源、DSH 下载目录）    | `%AppData%\dsh-plugin-manager\settings.json`           |
| 排序与上次选择（localStorage） | 随应用数据目录                                                |
| Profile（默认）           | `~/.dsh/profiles`                                      |
| 运行日志                  | 系统临时目录 `dshpm-run-<profile>-<ts>.log`（供「打开界面」提取 token） |



***

## 常见问题（FAQ）

**Q：为什么「打开界面」要用带 token 的地址？**

dsh web 只监听 `127.0.0.1`，且访问需认证 token（`dsh web: http://127.0.0.1:<port>/?token=...`）。应用在启动后保留日志，「打开界面」时实时提取最新 token，避免出现 `authentication required`。

**Q：如何让多个实例同时运行？**

任意环境 × Profile 组合各自独立进程，端口从 3080 起自动分配（可手动指定），每个实例独立停止 / 重启。

**Q：为什么装了插件还是加载不了？**

插件与 dsh runtime 的 `peerDependencies` 不兼容时会被拒绝；可在插件管理中对特定版本授权豁免（`allow-version`）。

**Q：启动后进程立即退出？**

通常是 DSH 版本与 Profile 不匹配（如旧版 dsh 加载新版 Profile 导致 `ERR_MODULE_NOT_FOUND`）。请选择匹配的 DSH 版本，或查看运行日志诊断。

**Q：国内网络下载失败怎么办？**

在「设置」中切换 npm 镜像源（默认已为 `registry.npmmirror.com`），DSH 下载与插件安装均使用该源。



***

## 技术栈



* **桌面框架**：Tauri 2（Rust 后端 + 系统 WebView2）

* **前端**：React 19、TypeScript、Vite 8

* **测试**：Vitest + Testing Library（前端）、cargo test（后端）

* **包管理**：pnpm