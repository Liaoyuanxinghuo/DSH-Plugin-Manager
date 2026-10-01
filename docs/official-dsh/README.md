# DSH 官方文档（本地副本）

来源：GitHub `deepseek-ai/deepseek-harness`（master 分支）+ npm `@deepseek-ai/dsh@0.2.0-rc.2`。
下载时间：2026-09-30。用途：DSH Manager 对照官方语义做修复与完善。

## 文件名映射

下载时把路径中的 `/` 换成了 `__`，例如 `packages__boot__plugin-manager__README.zh.md`
对应仓库路径 `packages/boot/plugin-manager/README.zh.md`。

## 目录

### 入口 / CLI
| 本地文件 | 仓库路径 | 内容 |
|---|---|---|
| `README.zh.md` | `README.zh.md` | 仓库总览 |
| `apps__cli__README.zh.md` | `apps/cli/README.zh.md` | dsh 启动器：profile / plugin 命令 |
| `apps__cli__reference__README.zh.md` | `apps/cli/reference/README.zh.md` | **CLI 行为参考（层优先级、关闭行为，权威）** |

### 启动 / 插件管理（本项目最相关）
| 本地文件 | 仓库路径 | 内容 |
|---|---|---|
| `packages__boot__plugin-manager__README.zh.md` | `packages/boot/plugin-manager/README.zh.md` | **插件/组合包开关语义（权威）** |
| `packages__boot__plugin-manager__README.md` | 同上英文版 | 同上 |
| `packages__boot__app-boot__README.zh.md` | `packages/boot/app-boot/README.zh.md` | profile 启动生命周期 |
| `packages__boot__cmdline__README.zh.md` | `packages/boot/cmdline/README.zh.md` | 命令行参数约定 |
| `packages__boot__hmr__README.zh.md` | `packages/boot/hmr/README.zh.md` | 配置热重载 |
| `packages__util__package-manifest__README.zh.md` | `packages/util/package-manifest/README.zh.md` | package.json manifest 读写 |

### 组合包（bundles）
| 本地文件 | 仓库路径 | 内容 |
|---|---|---|
| `packages__bundle__README.zh.md` | `packages/bundle/README.zh.md` | bundle 总览 |
| `packages__bundle__base__README.zh.md` | `packages/bundle/base/README.zh.md` | dsh-base |
| `packages__bundle__web-app__README.zh.md` | `packages/bundle/web-app/README.zh.md` | dsh-web-app |

### 子系统 / 架构
| 本地文件 | 仓库路径 | 内容 |
|---|---|---|
| `docs__architecture.zh.md` | `docs/architecture.zh.md` | 总体架构 |
| `docs__glossary.zh.md` | `docs/glossary.zh.md` | 术语表 |
| `docs__subsystems__boot.zh.md` | `docs/subsystems/boot.zh.md` | 启动子系统 |
| `docs__subsystems__mcp.zh.md` | `docs/subsystems/mcp.zh.md` | MCP 子系统 |
| `docs__subsystems__extensions.zh.md` | `docs/subsystems/extensions.zh.md` | 扩展/插件 |

### 用户指南
| 本地文件 | 仓库路径 | 内容 |
|---|---|---|
| `docs__user__guide__index.zh.md` | `docs/user/guide/index.zh.md` | 用户指南入口 |
| `docs__user__guide__mcp-memory.zh.md` | `docs/user/guide/mcp-memory.zh.md` | MCP 记忆 |
| `docs__user__develop__basic__config.zh.md` | `docs/user/develop/basic/config.zh.md` | 配置编写 |

### 插件管理 UI
| 本地文件 | 仓库路径 | 内容 |
|---|---|---|
| `packages__client__ui-plugin-manager__README.zh.md` | `packages/client/ui-plugin-manager/README.zh.md` | Web 端插件管理界面 |
| `packages__client__ui-settings-plugins__README.zh.md` | `packages/client/ui-settings-plugins/README.zh.md` | 设置页插件列表 |

## 与 DSH Manager 直接相关的官方语义摘要

摘自 `packages/boot/plugin-manager/README.zh.md`（原文见该文件）：

1. **插件开关**（单行/服务级）只更新 profile `cordis.patch.yml` 中最后一条匹配覆盖项的
   `disabled`；没有匹配项时追加。匹配依据是**条目 id**，以及覆盖项声明的**模块名称**。
2. **组合包开关**（bundle 级）修改 `package.json` 的有序 `dsh.profile.bundles` 列表。
   **关闭保留依赖**；开启追加到列表末尾（可能改变配置优先级）。
3. 安装新组合包默认启用。home 级和单次启动 patch 保留更高优先级。
4. 更新依赖会保留已停用的组合包选择。
5. 卸载顺序：从 `dsh.profile.bundles` 移除 → 卸载运行时贡献 → `pnpm remove`。

摘自 `apps/cli/README.zh.md`：

- 配置树层叠顺序：`dsh.profile.bundles` 各组合包 patch → profile `cordis.patch.yml`
  → `$DSH_HOME/cordis.patch.yml` → `--patch` 覆盖层。
- `dsh plugin` 在 profile 目录转发给 pnpm，并按已安装状态对账 `dsh.profile.bundles`
  （安装了且声明 `dsh.bundle` 的依赖会加入层列表；卸载后离开列表；模板内置组合包不受影响）。
