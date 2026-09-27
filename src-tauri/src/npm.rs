//! npm registry 查询与 DSH 插件兼容性预检

use serde::{Deserialize, Serialize};
use std::collections::HashMap;


/// 搜索命中项
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpmSearchHit {
    pub name: String,
    pub description: String,
    pub version: String,
    pub score: f64,
}

/// 包详情（含全部版本与 peerDependencies）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpmPackageInfo {
    pub name: String,
    pub description: String,
    pub dist_tags: HashMap<String, String>,
    pub versions: Vec<NpmVersionInfo>,
}

/// 单个版本信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpmVersionInfo {
    pub version: String,
    pub published_at: Option<String>,
    pub peer_dependencies: HashMap<String, String>,
    pub is_bundle: Option<bool>,
}

/// 兼容性问题
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerIssue {
    pub peer: String,
    pub requirement: String,
    pub runtime: String,
    pub satisfied: bool,
}

/// 搜索 npm 包
pub fn npm_search(query: &str, registry: &str) -> Result<Vec<NpmSearchHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("搜索关键词不能为空".to_string());
    }
    let url = format!("{registry}/-/v1/search?text={}&size=20", urlencode(q));
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|e| format!("搜索失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("registry 返回状态 {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析失败: {e}"))?;
    let mut hits = Vec::new();
    if let Some(objs) = json.get("objects").and_then(|o| o.as_array()) {
        for obj in objs {
            let pkg = obj.get("package").cloned().unwrap_or_default();
            hits.push(NpmSearchHit {
                name: pkg.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                description: pkg
                    .get("description")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                version: pkg.get("version").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                score: obj
                    .get("score")
                    .and_then(|s| s.get("final"))
                    .and_then(|s| s.as_f64())
                    .unwrap_or(0.0),
            });
        }
    }
    if hits.is_empty() {
        return Err("未搜索到匹配的包".to_string());
    }
    Ok(hits)
}

/// 获取包全部版本信息
pub fn npm_package_info(name: &str, registry: &str) -> Result<NpmPackageInfo, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("包名不能为空".to_string());
    }
    let url = format!("{registry}/{}", urlencode(name));
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|e| format!("查询失败: {e}"))?;
    if !resp.status().is_success() {
        if resp.status().as_u16() == 404 {
            return Err(format!("包不存在: {name}"));
        }
        return Err(format!("registry 返回状态 {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析失败: {e}"))?;
    let mut versions: Vec<NpmVersionInfo> = Vec::new();
    if let Some(vmap) = json.get("versions").and_then(|v| v.as_object()) {
        for (ver, vinfo) in vmap {
            let peer = vinfo
                .get("peerDependencies")
                .and_then(|p| p.as_object())
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            let published = vinfo
                .get("dist")
                .and_then(|d| d.get("tarball"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            versions.push(NpmVersionInfo {
                version: ver.clone(),
                published_at: published,
                peer_dependencies: peer,
                is_bundle: None,
            });
        }
    }
    // 按 semver 降序（粗略：优先数值比较，rc 版本靠后）
    versions.sort_by(|a, b| cmp_versions(&b.version, &a.version));
    let dist_tags = json
        .get("dist-tags")
        .and_then(|d| d.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    Ok(NpmPackageInfo {
        name: json.get("name").and_then(|n| n.as_str()).unwrap_or(name).to_string(),
        description: json.get("description").and_then(|n| n.as_str()).unwrap_or("").to_string(),
        dist_tags,
        versions,
    })
}

/// 查询包的最新版本（dist-tag latest）
pub fn npm_latest_version(name: &str, registry: &str) -> Option<String> {
    let info = npm_package_info(name, registry).ok()?;
    info.dist_tags.get("latest").cloned()
}

/// 兼容性预检：对插件某版本的 peerDependencies 与运行时版本比对
pub fn check_peer_compat(
    peer_deps: &HashMap<String, String>,
    runtime_version: &str,
) -> Vec<PeerIssue> {
    let mut issues = Vec::new();
    for (peer, req_str) in peer_deps {
        let satisfied = simple_satisfies(runtime_version, req_str);
        issues.push(PeerIssue {
            peer: peer.clone(),
            requirement: req_str.clone(),
            runtime: runtime_version.to_string(),
            satisfied,
        });
    }
    issues
}

/// 解析后的版本
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ParsedVer {
    major: u64,
    minor: u64,
    patch: u64,
    pre: String,
}

fn parse_full(v: &str) -> ParsedVer {
    let v = v.trim();
    let (main, pre) = match v.split_once('-') {
        Some((m, p)) => (m, p.to_string()),
        None => (v, String::new()),
    };
    let main = main.split('+').next().unwrap_or(main);
    let parts: Vec<&str> = main.split('.').collect();
    ParsedVer {
        major: parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0),
        minor: parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
        patch: parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
        pre,
    }
}

/// npm 语义 range 匹配（覆盖 DSH 插件 peer 常见写法）
pub fn simple_satisfies(version: &str, range: &str) -> bool {
    if range.trim().is_empty() || range == "*" || range == "latest" {
        return true;
    }
    range.split("||").any(|r| single_range_satisfies(version, r.trim()))
}

fn single_range_satisfies(version: &str, range: &str) -> bool {
    let ver = parse_full(version);
    range.split_whitespace().all(|part| part_satisfies(&ver, part))
}

fn part_satisfies(ver: &ParsedVer, part: &str) -> bool {
    let part = part.trim();
    if part.is_empty() || part == "*" || part == "x" || part == "X" || part == "latest" {
        return true;
    }
    if let Some(s) = part.strip_prefix(">=") {
        return ver >= &parse_full(s);
    }
    if let Some(s) = part.strip_prefix("<=") {
        return ver <= &parse_full(s);
    }
    if let Some(s) = part.strip_prefix('>') {
        return ver > &parse_full(s);
    }
    if let Some(s) = part.strip_prefix('<') {
        return ver < &parse_full(s);
    }
    if let Some(s) = part.strip_prefix('^') {
        return caret_match(ver, &parse_full(s));
    }
    if let Some(s) = part.strip_prefix('~') {
        return tilde_match(ver, &parse_full(s));
    }
    // 裸版本：npm 语义 = 精确匹配（含 x.y / x 部分形式）
    let req = parse_full(part);
    if req.pre.is_empty() && !part.contains('.') {
        // 仅 "0" / "1" → major 匹配
        return ver.major == req.major;
    }
    if !part.contains('.') && part.chars().all(|c| c.is_ascii_digit()) {
        return ver.major == req.major;
    }
    let dots = part.split('.').count();
    match dots {
        1 => ver.major == req.major,
        2 => ver.major == req.major && ver.minor == req.minor,
        _ => {
            // 完整版本：精确（含预发布）
            ver.major == req.major
                && ver.minor == req.minor
                && ver.patch == req.patch
                && pre_eq(&ver.pre, &req.pre)
        }
    }
}

fn pre_eq(a: &str, b: &str) -> bool {
    if a.is_empty() && b.is_empty() {
        return true;
    }
    // 预发布版本：一方有 pre 另一方无 → 不相等；都有 → 字符串相等
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b
}

fn caret_match(ver: &ParsedVer, req: &ParsedVer) -> bool {
    if req.major > 0 {
        // ^1.x → >=1.x <2.0.0
        ver.major == req.major && (ver.major, ver.minor, ver.patch) >= (req.major, req.minor, req.patch)
    } else if req.minor > 0 {
        // ^0.x → >=0.x.0 <0.(x+1).0
        ver.major == 0 && ver.minor == req.minor && ver.patch >= req.patch
    } else {
        // ^0.0.x → >=0.0.x <0.0.(x+1)
        ver.major == 0 && ver.minor == 0 && ver.patch >= req.patch
    }
}

fn tilde_match(ver: &ParsedVer, req: &ParsedVer) -> bool {
    // ~1.2.3 → >=1.2.3 <1.3.0；~1.2 → >=1.2.0 <1.3.0
    if req.minor > 0 || req.patch > 0 {
        ver.major == req.major && ver.minor == req.minor && ver.patch >= req.patch
    } else {
        ver.major == req.major
    }
}

/// 简单的版本比较（支持 rc/pre 后缀）
/// 版本比较（供其他模块复用）
pub fn cmp_versions_pub(a: &str, b: &str) -> std::cmp::Ordering {
    cmp_versions(a, b)
}

fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = parse_num(a);
    let pb = parse_num(b);
    let main = pa.cmp(&pb);
    if main != std::cmp::Ordering::Equal {
        return main;
    }
    let pre_a = parse_pre(a);
    let pre_b = parse_pre(b);
    match (pre_a, pre_b) {
        // 正式版 > 预发布版
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
        (Some(x), Some(y)) => cmp_pre(&x, &y),
    }
}

/// 解析预发布段（-alpha.1 → ["alpha","1"]）
fn parse_pre(v: &str) -> Option<Vec<String>> {
    let after = v.split('-').nth(1)?;
    Some(after.split('.').map(|s| s.to_string()).collect())
}

/// npm 预发布段比较：数字段按数值、字母段按 ASCII、数字 < 字母、前缀相同则更长的大
fn cmp_pre(a: &[String], b: &[String]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(m), Ok(n)) => m.cmp(&n),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

fn parse_num(v: &str) -> (u64, u64, u64) {
    let main = v.split(['-', '+']).next().unwrap_or("0");
    let parts: Vec<&str> = main.split('.').collect();
    let get = |i: usize| -> u64 {
        parts
            .get(i)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    (get(0), get(1), get(2))
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'@' => out.push_str("%40"),
            b'/' => out.push_str("%2F"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urlencode() {
        assert_eq!(urlencode("@scope/pkg"), "%40scope%2Fpkg");
        assert_eq!(urlencode("dshmarket"), "dshmarket");
    }

    #[test]
    fn test_cmp_versions() {
        assert!(cmp_versions("1.0.0", "0.9.9") == std::cmp::Ordering::Greater);
        assert!(cmp_versions("1.1.0", "1.0.9") == std::cmp::Ordering::Greater);
        assert!(cmp_versions("1.1.19", "1.1.18") == std::cmp::Ordering::Greater);
        // 预发布排后面（更"新"的版本先展示）
        assert!(cmp_versions("1.0.0-rc.1", "1.0.0") == std::cmp::Ordering::Less);
    }

    #[test]
    fn test_check_peer_compat() {
        let mut peers = HashMap::new();
        peers.insert(
            "@deepseek-ai/dsh-client-runtime".to_string(),
            "0.1.0-rc.8 || 0.1.1-rc.2".to_string(),
        );
        let issues = check_peer_compat(&peers, "0.1.7-rc.2");
        // 0.1.7-rc.2 不满足 0.1.0-rc.8 || 0.1.1-rc.2
        assert!(issues.iter().all(|i| !i.satisfied));

        let issues2 = check_peer_compat(&peers, "0.1.1-rc.2");
        assert!(issues2.iter().any(|i| i.satisfied));
    }

    #[test]
    fn test_check_peer_compat_caret() {
        let mut peers = HashMap::new();
        peers.insert("@deepseek-ai/cordis".to_string(), "^4.0.2".to_string());
        let issues = check_peer_compat(&peers, "4.0.2");
        assert!(issues.iter().all(|i| i.satisfied));
        let issues2 = check_peer_compat(&peers, "3.9.0");
        assert!(issues2.iter().all(|i| !i.satisfied));
    }
}
