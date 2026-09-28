# DSH_HOME 定义与目录结构

> 本文档记录 DSH Manager 中 **DSH_HOME** 的统一定义，供启动注入、文件系统快捷入口、整合包导入导出等所有功能共用。

## 定义

**DSH_HOME 是 DSH 的数据根目录**，即：

> **直接包含 `profiles` 目录的那个文件夹**（与 `profiles` 同级的通常还有 `sessions`、`logs` 等目录）。

也就是说，DSH_HOME 并不是 `profiles` 目录本身，而是它的**父目录**：

```
<DSH_HOME>
├── profiles/          ← 所有 profile 平铺于此（每个 profile 一个子目录）
│   ├── my-profile/
│   ├── another-profile/
│   └── ...
├── sessions/          ← 会话数据（与 profiles 同级）
└── logs/              ← 日志（与 profiles 同级）
```

## 为什么是这个定义

DSH CLI 的语义是 **`$DSH_HOME/profiles/<profile名>`**：

- DSH 从 `$DSH_HOME` 出发，在 `$DSH_HOME/profiles/` 下查找 profile；
- `sessions`、`logs` 等也以 `$DSH_HOME` 为根生成。

因此注入 `DSH_HOME` 时必须指向 `profiles` 的**父目录**，而不能指向 `profiles` 本身（否则 DSH 会去找不存在的 `$DSH_HOME/profiles`）。

## 在应用中的体现

### 启动注入

启动「任意 DSH 版本 × 任意 Profile」组合时：

1. 取当前选中 profile 的**来源目录**（`profilesDir`，即直接包含该 profile 的目录）；
2. `DSH_HOME = profilesDir 的父目录`；
3. 以环境变量 `DSH_HOME=<父目录>` 注入启动进程。

### 文件系统快捷入口

「文件系统快捷入口 → DSH_HOME」打开的就是上述同一个父目录（直接包含 `profiles` 的文件夹），与启动注入语义保持一致。

| 入口 | 路径 |
|---|---|
| 当前 Profile 目录 | `$DSH_HOME/profiles/<profile名>` |
| Profiles 目录 | `$DSH_HOME/profiles` |
| **DSH_HOME** | **`$DSH_HOME`（profiles 的父目录）** |
| 会话目录 | `$DSH_HOME/sessions` |
| 日志目录 | `$DSH_HOME/logs` |

### 扫描目录

「添加本地 Profile 扫描目录」时，允许选择 `DSH_HOME`、`profiles` 目录或单个 profile 目录三者之一：

- 选 `DSH_HOME` → 识别其下的 `profiles/` 子目录；
- 选 `profiles` 目录 → 直接使用；
- 选单个 profile 目录 → 取其父目录作为来源。

### 整合包（.dspack）

- 导出为整合包时，`home/` 级内容（如 `sessions`、`logs`、配置）以 `DSH_HOME` 为基准收集；
- 导入整合包时，`home/` 级内容写回 **profiles 目录的父目录**（即 DSH_HOME 语义）。

## 代码位置

| 位置 | 说明 |
|---|---|
| `src-tauri/src/runner.rs` `dsh_home_of()` | 由 `profiles_dir` 计算 DSH_HOME（取其父目录） |
| `src-tauri/src/lib.rs` `start_dsh_cmd` | 启动时注入 `DSH_HOME=<profiles_dir 的父目录>` |
| `src-tauri/src/lib.rs` `get_env_paths` | 返回 `dshHome` / `sessionsDir` / `logsDir` 供前端快捷入口 |
| `src/App.tsx` | 文件系统快捷入口「DSH_HOME」按钮打开 `paths.dshHome` |
