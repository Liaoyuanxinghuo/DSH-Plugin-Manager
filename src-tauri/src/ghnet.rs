//! 统一 GitHub 网络保障：检查更新 / 下载安装包 / 整合包市场等全部走此模块。
//!
//! 策略（大陆无 VPN）：
//! 1. 按顺序轮流尝试镜像：**用户设置的镜像最先** → 内置可用镜像 → **最后直连**
//! 2. 每条通道约 10s 超时，失败/超时自动切下一条（日志提示，避免用户以为卡住）
//! 3. 无总时限；全部失败才报「网络有问题」

use std::io::Read;
use std::time::Duration;

/// 内置 GitHub 镜像（本机大陆网络实测可代理 raw / release 资产）
pub const BUILTIN_GH_MIRRORS: &[&str] = &[
    "https://gh-proxy.com",
    "https://ghproxy.net",
    "https://gh.ddlc.top",
    "https://ghfast.top",
];

/// 单通道超时（秒）
pub const PER_MIRROR_TIMEOUT_SECS: u64 = 10;

/// 生成候选 URL：用户设置镜像链（主镜像 + 自定义列表）→ 内置镜像 → 直连
pub fn candidate_urls(direct_url: &str) -> Vec<String> {
    let mut mirrors: Vec<String> = crate::settings::github_mirror_list();
    for m in BUILTIN_GH_MIRRORS {
        let s = m.to_string();
        if !mirrors.iter().any(|x| x == &s) {
            mirrors.push(s);
        }
    }
    let mut out: Vec<String> = mirrors
        .iter()
        .map(|m| format!("{m}/{direct_url}"))
        .collect();
    out.push(direct_url.to_string());
    out
}

/// 某 URL 是否为 GitHub 域（仅这些走镜像轮询）
pub fn is_github_url(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("https://github.com/")
        || u.starts_with("https://raw.githubusercontent.com/")
        || u.starts_with("https://api.github.com/")
        || u.starts_with("https://codeload.github.com/")
        || u.starts_with("https://objects.githubusercontent.com/")
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// 文本 GET：镜像轮询。返回 (最终 URL, body)。全部失败 Err(各通道错误摘要)。
pub fn fetch_text(direct_url: &str) -> Result<(String, String), String> {
    let mut errs: Vec<String> = Vec::new();
    let urls = if is_github_url(direct_url) {
        candidate_urls(direct_url)
    } else {
        vec![direct_url.to_string()]
    };
    for u in urls {
        let c = match client(Duration::from_secs(PER_MIRROR_TIMEOUT_SECS)) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        match c.get(&u).header("User-Agent", "dsh-manager").send() {
            Ok(r) if r.status().is_success() => {
                let body = r.text().unwrap_or_default();
                return Ok((u, body));
            }
            Ok(r) => errs.push(format!("{u} → HTTP {}", r.status())),
            Err(e) => errs.push(format!("{u} → {e}")),
        }
    }
    Err(format!("全部通道失败：{}", errs.join("；")))
}

/// 二进制 GET（下载安装包等）：镜像轮询。返回 (最终 URL, bytes)。
pub fn fetch_bytes(direct_url: &str) -> Result<(String, Vec<u8>), String> {
    let (u, resp) = send_get(direct_url)?;
    let mut buf = Vec::new();
    let mut r = resp;
    r.read_to_end(&mut buf)
        .map_err(|e| format!("读取下载内容失败: {e}"))?;
    Ok((u, buf))
}

/// 打开流式 GET（大文件下载）：镜像轮询，返回 (最终 URL, Response)
pub fn send_get(direct_url: &str) -> Result<(String, reqwest::blocking::Response), String> {
    let mut errs: Vec<String> = Vec::new();
    let urls = if is_github_url(direct_url) {
        candidate_urls(direct_url)
    } else {
        vec![direct_url.to_string()]
    };
    for u in urls {
        // 连接 10s；下载允许更长（大文件）
        let c = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(PER_MIRROR_TIMEOUT_SECS))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        match c.get(&u).header("User-Agent", "dsh-manager").send() {
            Ok(r) if r.status().is_success() => return Ok((u, r)),
            Ok(r) => errs.push(format!("{u} → HTTP {}", r.status())),
            Err(e) => errs.push(format!("{u} → {e}")),
        }
    }
    Err(format!("全部通道失败：{}", errs.join("；")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_user_first_builtin_then_direct() {
        // 不写用户配置时（测试环境默认有 ghfast.top 主镜像）链路仍含内置与直连
        let urls = candidate_urls("https://github.com/a/b/releases/download/V1/x.exe");
        assert!(urls.iter().any(|u| u.starts_with("https://gh-proxy.com/")));
        assert!(urls.iter().any(|u| u.starts_with("https://gh.ddlc.top/")));
        assert_eq!(
            urls.last().unwrap(),
            "https://github.com/a/b/releases/download/V1/x.exe"
        );
    }

    #[test]
    fn non_github_url_untouched() {
        // fetch_text/send_get 仅对 GitHub 域展开镜像；非 GitHub 直接原 URL
        assert!(!is_github_url("https://example.com/x"));
    }

    #[test]
    fn is_github_detects_hosts() {
        assert!(is_github_url("https://raw.githubusercontent.com/a/b/main/f"));
        assert!(is_github_url("https://api.github.com/repos/a/b"));
        assert!(is_github_url("https://github.com/a/b/releases/download/v/x"));
        assert!(!is_github_url("https://example.com/x"));
    }

    /// 大陆网络实测：VERSION + release 资产下载走镜像轮询。
    /// `cargo test --lib real_ghnet_fetch -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_ghnet_fetch() {
        let repo = "Liaoyuanxinghuo/DSH-Plugin-Manager";
        let (u, body) = fetch_text(&format!(
            "https://raw.githubusercontent.com/{repo}/main/VERSION"
        ))
        .expect("应能通过镜像拿到 VERSION");
        println!("VERSION via {u}: {}", body.trim());
        assert!(body.trim().starts_with("0.") || body.trim().starts_with('v'));

        // 打开 release 资产下载流（能连上即镜像可用）
        let ver = body.trim().trim_start_matches('v');
        let direct = format!(
            "https://github.com/{repo}/releases/download/V{ver}/DSH.Manager_{ver}_x64-setup.exe"
        );
        let (du, mut resp) = send_get(&direct).expect("应能通过镜像打开安装包下载");
        let len = resp.content_length().unwrap_or(0);
        println!("asset via {du}: content-length={len}");
        assert!(len > 100_000, "安装包应有百 KB 以上");
    }
}
