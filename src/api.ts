// API 封装：调用 Tauri 后端命令
import { invoke } from "@tauri-apps/api/core";
import type {
  DshEnv,
  PackExportResult,
  PackImportResult,
  PackMarketEntry,
  ProfileNotesMap,
  DshStatus,
  EnvPaths,
  ExportResult,
  ImportResult,
  CreateProfileResult,
  DepIssue,
  JunkEntry,
  Settings,
  DshVersionInfo,
  DshInstallResult,
  InstallOutcome,
  MarketCatalog,
  NpmPackageInfo,
  NpmSearchHit,
  PeerIssue,
  PluginInfo,
  PluginUpdate,
  ProfileInfo,
  RunningProcess,
  ScanDirEntry,
  StartResult,
  UpdateInfo,
} from "./types";

import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

export const api = {
  scanEnvs: () => invoke<DshEnv[]>("scan_envs"),
  addManualEnv: (name: string, command: string) =>
    invoke<DshEnv>("add_manual_env", { name, command }),
  removeManualEnv: (id: string) =>
    invoke<void>("remove_manual_env", { id }),
  // 在线插件（M2）
  npmSearch: (query: string) => invoke<NpmSearchHit[]>("npm_search_cmd", { query }),
  npmPackageInfo: (name: string) => invoke<NpmPackageInfo>("npm_package_info_cmd", { name }),
  checkCompat: (name: string, version: string, runtimeVersion: string) =>
    invoke<PeerIssue[]>("check_compat_cmd", { name, version, runtimeVersion }),
  installPlugin: (envId: string, profile: string, profilesDir: string, spec: string) =>
    invoke<InstallOutcome>("install_plugin_cmd", { envId, profile, profilesDirStr: profilesDir, spec }),
  installLocalPlugin: (envId: string, profile: string, profilesDir: string, path: string) =>
    invoke<InstallOutcome>("install_local_plugin_cmd", { envId, profile, profilesDirStr: profilesDir, path }),
  removePlugin: (envId: string, profile: string, profilesDir: string, name: string) =>
    invoke<InstallOutcome>("remove_plugin_cmd", { envId, profile, profilesDirStr: profilesDir, name }),
  allowVersion: (envId: string, profile: string, profilesDir: string, pkgSpec: string) =>
    invoke<InstallOutcome>("allow_version_cmd", { envId, profile, profilesDirStr: profilesDir, pkgSpec }),
  disallowVersion: (envId: string, profile: string, profilesDir: string, pkgSpec: string) =>
    invoke<InstallOutcome>("disallow_version_cmd", { envId, profile, profilesDirStr: profilesDir, pkgSpec }),
  // 插件市场目录
  marketCatalog: () => invoke<MarketCatalog>("market_catalog_cmd"),
  // 修复 pnpm 构建白名单（git 源插件）
  fixBuildPermit: (envId: string, profile: string, packages: string[]) =>
    invoke<string[]>("fix_build_permit_cmd", { envId, profile, packages }),
  // 扫描目录
  addScanDir: (path: string) => invoke<ScanDirEntry>("add_scan_dir", { path }),
  removeScanDir: (id: string) => invoke<void>("remove_scan_dir", { id }),
  listScanDirs: () => invoke<ScanDirEntry[]>("list_scan_dirs"),
  // 扫描 dsh 本体（返回新添加的环境）
  addDshScanDir: (path: string) => invoke<DshEnv[]>("add_dsh_scan_dir", { path }),
  getEnv: (envId: string) => invoke<DshEnv | null>("get_env", { envId }),
  listAllProfiles: () => invoke<ProfileInfo[]>("list_all_profiles"),
  listProfiles: (envId: string) =>
    invoke<ProfileInfo[]>("list_profiles", { envId }),
  listPlugins: (envId: string, profile: string, profilesDir: string) =>
    invoke<PluginInfo[]>("list_plugins", { envId, profile, profilesDirStr: profilesDir }),
  // M3 插件管理
  setPluginEnabled: (envId: string, profile: string, profilesDir: string, packageName: string, enabled: boolean) =>
    invoke<string[]>("set_plugin_enabled_cmd", { envId, profile, profilesDirStr: profilesDir, packageName, enabled }),
  checkUpdates: (envId: string, profile: string, profilesDir: string) =>
    invoke<PluginUpdate[]>("check_updates_cmd", { envId, profile, profilesDirStr: profilesDir }),
  startDsh: (envId: string, profile: string, profilesDir: string, port?: number) => {
    const args: Record<string, unknown> = { envId, profile, profilesDirStr: profilesDir };
    if (port !== undefined) args.port = port;
    return invoke<StartResult>("start_dsh_cmd", args);
  },
  stopDsh: (envId: string, profile: string, profilesDir: string) =>
    invoke<void>("stop_dsh_cmd", { envId, profile, profilesDirStr: profilesDir }),
  dshStatus: (envId: string, profile: string, profilesDir: string) =>
    invoke<DshStatus>("dsh_status", { envId, profile, profilesDirStr: profilesDir }),
  listRunning: () => invoke<RunningProcess[]>("list_running_cmd"),
  /** 停止全部运行中的 DSH（关闭软件前用）；返回停止失败项 */
  stopAllDsh: () => invoke<string[]>("stop_all_dsh_cmd"),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  openUrl: (url: string) => invoke<void>("open_url_cmd", { url }),
  /** 打开 DSH web 界面：后端实时从日志提取带 token 的地址 */
  openDshWeb: (envId: string, profile: string, profilesDir: string) =>
    invoke<void>("open_dsh_web_cmd", { envId, profile, profilesDirStr: profilesDir }),
  getEnvPaths: (envId: string, profile: string, profilesDir: string) =>
    invoke<EnvPaths>("get_env_paths", { envId, profile, profilesDirStr: profilesDir }),
  // profile 导出/导入
  exportProfile: (
    envId: string,
    profile: string,
    profilesDir: string,
    targetPath: string,
    excludeNodeModules: boolean,
  ) => invoke<ExportResult>("export_profile_cmd", { envId, profile, profilesDirStr: profilesDir, targetPath, excludeNodeModules }),
  importProfile: (envId: string, zipPath: string, profilesDir: string) =>
    invoke<ImportResult>("import_profile_cmd", { envId, zipPath, profilesDirStr: profilesDir }),
  createProfile: (envId: string, name: string, profilesDir: string) =>
    invoke<CreateProfileResult>("create_profile_cmd", { envId, name, profilesDirStr: profilesDir }),
  deleteProfile: (envId: string, name: string, profilesDir: string) =>
    invoke<void>("delete_profile_cmd", { envId, name, profilesDirStr: profilesDir }),
  // M4 依赖健康 / 残留清理 / 诊断
  checkDeps: (envId: string, profile: string, profilesDir: string) =>
    invoke<DepIssue[]>("check_deps_cmd", { envId, profile, profilesDirStr: profilesDir }),
  fixDeps: (envId: string, profile: string, profilesDir: string) =>
    invoke<InstallOutcome>("fix_deps_cmd", { envId, profile, profilesDirStr: profilesDir }),
  scanJunk: (envId: string, profile: string, profilesDir: string) =>
    invoke<JunkEntry[]>("scan_junk_cmd", { envId, profile, profilesDirStr: profilesDir }),
  cleanJunk: (envId: string, profile: string, profilesDir: string, names: string[]) =>
    invoke<string[]>("clean_junk_cmd", { envId, profile, profilesDirStr: profilesDir, names }),
  exportDiag: (envId: string, targetPath: string) =>
    invoke<ExportResult>("export_diag_cmd", { envId, targetPath }),
  // M5 设置 / DSH 下载
  getSettings: () => invoke<Settings>("get_settings_cmd"),
  checkUpdate: () => invoke<UpdateInfo>("check_update_cmd"),
  downloadUpdate: (version: string) => invoke<string>("download_update_cmd", { version }),
  launchInstallerAndExit: (path: string) => invoke<void>("launch_installer_and_exit_cmd", { path }),
  setSettings: (npmRegistry: string, dshDownloadDir: string, githubMirror: string) =>
    invoke<void>("set_settings_cmd", { npmRegistry, dshDownloadDir, githubMirror }),
  listDshVersions: () => invoke<DshVersionInfo[]>("list_dsh_versions_cmd"),
  installDshVersion: (version: string, targetDir: string) =>
    invoke<DshInstallResult>("install_dsh_version_cmd", { version, targetDir }),
  profileNodeModulesSize: (envId: string, profile: string, profilesDir: string) =>
    invoke<number>("profile_node_modules_size", { envId, profile, profilesDirStr: profilesDir }),
  // 整合包（DSH-PackForge）
  exportPack: (
    envId: string,
    profile: string,
    profilesDir: string,
    targetPath: string,
    packName: string,
    packVersion: string,
    displayName: string,
  ) => invoke<PackExportResult>("export_pack_cmd", { envId, profile, profilesDirStr: profilesDir, targetPath, packName, packVersion, displayName }),
  marketPacks: () => invoke<PackMarketEntry[]>("market_packs_cmd"),
  downloadPack: (url: string, sha256: string, size: number) =>
    invoke<string>("download_pack_cmd", { url, sha256, size }),
  importPack: (envId: string, packPath: string, profilesDir: string) =>
    invoke<PackImportResult>("import_pack_cmd", { envId, packPath, profilesDirStr: profilesDir }),
  // profile 备注
  getProfileNotes: () => invoke<ProfileNotesMap>("get_profile_notes_cmd"),
  saveProfileNote: (profile: string, profilesDir: string, note: string, hintVersion: string) =>
    invoke<void>("save_profile_note_cmd", { profile, profilesDirStr: profilesDir, note, hintVersion }),
};

/** 打开保存对话框，返回用户选择的路径（取消返回 null） */
export async function pickSaveZipPath(defaultName: string): Promise<string | null> {
  const path = await saveDialog({
    title: "导出 Profile 压缩包",
    defaultPath: defaultName,
    filters: [{ name: "ZIP 压缩包", extensions: ["zip"] }],
  });
  return path ?? null;
}

/** 打开文件选择对话框选 zip */
export async function pickZipFile(): Promise<string | null> {
  const path = await openDialog({
    title: "选择 Profile 备份压缩包",
    multiple: false,
    filters: [{ name: "ZIP 压缩包", extensions: ["zip"] }],
  });
  return typeof path === "string" ? path : null;
}

/** 打开文件夹选择对话框 */
export async function pickFolder(title: string): Promise<string | null> {
  const path = await openDialog({
    title,
    directory: true,
    multiple: false,
  });
  return typeof path === "string" ? path : null;
}

/** 打开文件选择对话框选 tgz 压缩包 */
export async function pickTgzFile(): Promise<string | null> {
  const path = await openDialog({
    title: "选择插件压缩包（.tgz）",
    multiple: false,
    filters: [
      { name: "tgz 压缩包", extensions: ["tgz", "tar.gz"] },
      { name: "所有文件", extensions: ["*"] },
    ],
  });
  return typeof path === "string" ? path : null;
}

/** 打开文件选择对话框选 .dspack 整合包 */
export async function pickDspackFile(): Promise<string | null> {
  const path = await openDialog({
    title: "选择整合包（.dspack）",
    multiple: false,
    filters: [
      { name: "整合包", extensions: ["dspack"] },
      { name: "所有文件", extensions: ["*"] },
    ],
  });
  return typeof path === "string" ? path : null;
}

/** 打开保存对话框选 .dspack 整合包位置 */
export async function pickSavePackPath(defaultName: string): Promise<string | null> {
  const path = await saveDialog({
    title: "导出整合包",
    defaultPath: defaultName,
    filters: [{ name: "整合包", extensions: ["dspack"] }],
  });
  return path ?? null;
}

/** 格式化文件大小 */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}
