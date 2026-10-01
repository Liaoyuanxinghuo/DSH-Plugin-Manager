//! 数据模型：DSH 环境、Profile、插件等结构定义

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// DSH 环境来源类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EnvSource {
    /// 全局安装的 dsh CLI（npm/pnpm 全局包）
    GlobalCli,
    /// 用户手动添加的环境（如 pnpm dlx 临时版本、本地目录安装版）
    Manual,
    /// 用户手动添加的本地 profile 扫描目录（不绑定 dsh 运行时，仅管理/导出/导入）
    ScanDir,
}

/// 一个可用的 DSH 运行环境
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshEnv {
    /// 唯一标识
    pub id: String,
    /// 显示名称（如 "Global CLI" / "Manual: 0.1.7-rc.2"）
    pub name: String,
    /// 来源类型
    pub source: EnvSource,
    /// 版本号（dsh --version 输出，如 0.1.0-rc.7；扫描目录为 "—"）
    pub version: String,
    /// 数据根目录（DSH_HOME，未设置时为 ~/.dsh）
    pub home_dir: String,
    /// 用于探测/启动的完整命令模板（不含 --profile 等运行参数）
    /// 如 "dsh"、"pnpm dlx @deepseek-ai/dsh@0.1.7-rc.2"；扫描目录为空
    pub run_command: String,
    /// dsh 可执行文件位置（where 结果，可能为空）
    pub bin_path: Option<String>,
    /// 扫描目录专用：实际存放 profiles 的目录（可能本身是 DSH_HOME 或 profiles 目录）
    pub scan_profiles_dir: Option<String>,
}

/// 用户添加的本地 profile 扫描目录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanDirEntry {
    pub id: String,
    /// 用户输入的路径
    pub path: String,
    /// 显示名称
    pub label: String,
    /// 解析出的实际 profiles 目录
    pub profiles_dir: String,
    /// 绑定的 dsh 本体可执行文件（dsh.cmd 等）；绑定后该扫描目录环境可用它启动
    #[serde(default)]
    pub dsh_bin: Option<String>,
}

/// 单个 profile 的信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInfo {
    pub name: String,
    /// 所在 profiles 目录（来源），用于任意 dsh × profile 组合启动时注入 DSH_HOME
    pub profiles_dir: String,
    pub path: String,
    /// 已安装插件数量
    pub plugin_count: usize,
    /// 数据目录大小（字节）
    pub data_size: u64,
    /// 最后修改时间（ISO 字符串）
    pub modified: String,
    /// 是否存在 cordis.patch.yml
    pub has_patch: bool,
    /// 是否存在 pnpm-lock.yaml
    pub has_lock: bool,
    /// package.json 是否存在
    pub has_package: bool,
}

/// 插件信息（从 profile package.json 解析）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    /// 包名
    pub name: String,
    /// package.json 中声明的版本 spec（如 ^1.1.18、0.2.59）
    pub spec: String,
    /// 是否同时注册为 bundle 组合包
    pub is_bundle: bool,
    /// 兼容性预检结果（None = 未检测）
    pub compatible: Option<bool>,
    /// 不兼容原因说明
    pub incompatible_reason: Option<String>,
    /// 是否在 cordis.patch.yml 中被禁用
    pub is_disabled: bool,
}

/// 插件更新信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub name: String,
    pub current: String,
    pub latest: String,
    pub updatable: bool,
    pub error: Option<String>,
}

/// profile package.json 解析结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePackage {
    pub name: String,
    pub dependencies: HashMap<String, String>,
    pub bundles: Vec<String>,
}

/// 运行中的 DSH 进程信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningProcess {
    pub env_id: String,
    /// 该 profile 所在的 profiles 目录（来源）
    pub profiles_dir: String,
    pub profile: String,
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
    /// 带 token 的 web 访问地址（用户点「打开界面」时使用）
    pub url: String,
    /// 本次运行日志文件（保留，供「打开界面」实时提取最新 token）
    pub log_path: String,
}

/// DSH 运行状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshStatus {
    pub running: bool,
    pub process: Option<RunningProcess>,
    pub port_open: bool,
    /// 真正可服务：端口通 **且** 日志已出现带 token 的 web URL（`dsh web: http://...`）。
    /// 仅端口通不代表 web 就绪（MCP 初始化中前端会一直「连接中」）。
    pub web_ready: bool,
    /// 日志中提取到的带 token URL（web_ready 时给出，供打开界面）
    pub url: Option<String>,
}

/// 孤儿 node 进程候选（同 bin.js + 同 profile 名，不在运行表中；只展示，待用户确认后清理）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanProcess {
    pub pid: u32,
    pub port: u16,
    pub cmdline: String,
}

/// 启动结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub success: bool,
    pub pid: Option<u32>,
    pub message: String,
    pub port: u16,
    pub url: String,
}

/// 通用命令执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}
