// DSH Plugin Manager 前端类型定义

export type EnvSource = "globalCli" | "manual" | "scanDir";

export interface DshEnv {
  id: string;
  name: string;
  source: EnvSource;
  version: string;
  homeDir: string;
  runCommand: string;
  binPath: string | null;
  scanProfilesDir: string | null;
}

export interface ScanDirEntry {
  id: string;
  path: string;
  label: string;
  profilesDir: string;
}

export interface ProfileInfo {
  name: string;
  /** 所在 profiles 目录（来源），任意 dsh × 任意来源 profile 组合启动时注入 DSH_HOME */
  profilesDir: string;
  path: string;
  pluginCount: number;
  dataSize: number;
  modified: string;
  hasPatch: boolean;
  hasLock: boolean;
  hasPackage: boolean;
}

export interface PluginInfo {
  name: string;
  spec: string;
  isBundle: boolean;
  compatible: boolean | null;
  incompatibleReason: string | null;
  isDisabled: boolean;
}

// ===== M3 插件管理 =====
export interface PluginUpdate {
  name: string;
  current: string;
  latest: string;
  updatable: boolean;
  error: string | null;
}

// ===== M4 依赖健康 / 残留清理 =====
export interface DepIssue {
  name: string;
  declared: string;
  installed: boolean;
  issue: string; // "ok" | "missing"
}

export interface JunkEntry {
  name: string;
  path: string;
  size: number;
}

// ===== M5 设置 / DSH 下载 =====
export interface Settings {
  npmRegistry: string;
  dshDownloadDir: string;
}

export interface DshVersionInfo {
  version: string;
  tags: string[];
}

export interface DshInstallResult {
  success: boolean;
  exitCode: number | null;
  summary: string;
  binPath: string | null;
  version: string;
}

export interface RunningProcess {
  envId: string;
  /** profile 来源目录 */
  profilesDir: string;
  profile: string;
  pid: number;
  port: number;
  startedAt: string;
  /** 带 token 的 web 访问地址（点「打开界面」时使用） */
  url: string;
  /** 本次运行日志文件（保留，供「打开界面」实时提取最新 token） */
  logPath: string;
}

export interface DshStatus {
  running: boolean;
  process: RunningProcess | null;
  portOpen: boolean;
}

export interface StartResult {
  success: boolean;
  pid: number | null;
  message: string;
  port: number;
  url: string;
}

export interface EnvPaths {
  homeDir: string;
  profilesDir: string;
  sessionsDir: string;
  logsDir: string;
  binPath: string | null;
}

export interface ExportResult {
  fileCount: number;
  zipSize: number;
  skippedSymlinks: string[];
  targetPath: string;
}

export interface ImportResult {
  profileName: string;
  finalName: string;
  renamed: boolean;
  pluginCount: number;
  targetPath: string;
}

export interface CreateProfileResult {
  name: string;
  path: string;
}

// ===== M2 在线插件安装 =====
export interface NpmSearchHit {
  name: string;
  description: string;
  version: string;
  score: number;
}

export interface NpmVersionInfo {
  version: string;
  publishedAt?: string;
  peerDependencies: Record<string, string>;
  isBundle?: boolean;
}

export interface NpmPackageInfo {
  name: string;
  description: string;
  distTags: Record<string, string>;
  versions: NpmVersionInfo[];
}

export interface PeerIssue {
  peer: string;
  requirement: string;
  runtime: string;
  satisfied: boolean;
}

export interface InstallOutcome {
  success: boolean;
  exitCode: number | null;
  summary: string;
}

export interface InstallLogLine {
  line: string;
  kind: "stdout" | "stderr";
}

export interface InstallDone {
  success: boolean;
  exitCode: number | null;
}

// ===== 插件市场目录（M2 增强） =====
export interface MarketPlugin {
  name: string;
  owner: string;
  url: string;
  category: string;
  descriptionZh: string;
  descriptionEn: string;
  npm: string | null;
  version: string | null;
  stars: number | null;
  downloads: number | null;
  install: string;
  githubOnly: boolean;
}

export interface MarketCatalog {
  name: string;
  count: number;
  updated: string;
  categories: string[];
  plugins: MarketPlugin[];
}
