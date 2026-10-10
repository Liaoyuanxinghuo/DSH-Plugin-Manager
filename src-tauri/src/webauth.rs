//! DSH web 登录 cookie 自铸。
//!
//! 背景：`dsh web:` 的 token 行要等 Loader 全树结算才打印；dsh-mcp-connector 等插件
//! 在 apply() 里 `await connection.ready`，MCP 一多/一卡就永远不结算，token 行永不出现。
//! 而 launch token 是进程内随机数，外部拿不到。
//!
//! 但 cookie 的**签名密钥**是持久化在 `$DSH_HOME/.credentials.yaml` 的
//! `client-connection/browser-session` 记录里——用它按 dsh 同款算法铸出登录 cookie，
//! 经一次性本地 303 跳转注入浏览器，即可免 token 直接进入 web（cookie 不区分端口）。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// base64url 编码（无填充，`-_` 字母表）——与 dsh `encodeBase64Url` 一致。
pub fn b64url_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

/// base64url 解码（容错：接受有/无填充）。
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let raw: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(raw.len() * 3 / 4);
    for chunk in raw.chunks(4) {
        let mut n: u32 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i as u32);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// 从 DSH_HOME 读出 browser-session 签名密钥（32 字节原始值）。
/// 查找顺序：`dsh_home/.credentials.yaml` → `dsh_home/profiles/.credentials.yaml`
/// → 父目录 `.credentials.yaml`（结构差异兜底）。
/// 缺文件 / 缺记录 / 长度不对均返回 None——dsh 在 Connection 激活时才创建该密钥，
/// 首次启动或写入失败时可能没有。
pub fn read_browser_session_secret(dsh_home: &std::path::Path) -> Option<Vec<u8>> {
    // 1. 直接在其自身目录下查找
    let direct = dsh_home.join(".credentials.yaml");
    if direct.is_file() {
        if let Some(s) = read_secret_file(&direct) {
            return Some(s);
        }
    }
    // 2. 在 profiles 子目录下查找（部分打包结构放置于此）
    let in_profiles = dsh_home.join("profiles").join(".credentials.yaml");
    if in_profiles.is_file() {
        if let Some(s) = read_secret_file(&in_profiles) {
            return Some(s);
        }
    }
    // 3. 上级目录兜底（例如当前传入的 dsh_home 本身是 profiles 子目录）
    if let Some(parent) = dsh_home.parent() {
        let p = parent.join(".credentials.yaml");
        if p.is_file() {
            if let Some(s) = read_secret_file(&p) {
                return Some(s);
            }
        }
    }
    None
}

fn read_secret_file(path: &std::path::Path) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let secret_b64 = doc
        .get("records")?
        .get("client-connection/browser-session")?
        .get("payload")?
        .get("secret")?
        .as_str()?;
    let raw = b64url_decode(secret_b64)?;
    if raw.len() != 32 {
        return None;
    }
    Some(raw)
}

/// 等待密钥出现（dsh 在 Connection 激活时写入，可能比端口就绪略晚）。
/// `timeout_ms` 内每 250ms 重读一次；找不到返回 None。
pub fn wait_browser_session_secret(dsh_home: &std::path::Path, timeout_ms: u64) -> Option<Vec<u8>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if let Some(s) = read_browser_session_secret(dsh_home) {
            return Some(s);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// cookie 名：`dsh-auth-` + base64url(sha256(authority))。
pub fn cookie_name(authority: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(authority.as_bytes());
    format!("dsh-auth-{}", b64url_encode(&hasher.finalize()))
}

/// 铸造登录 cookie 值：`v1.{base64url(payload)}.{base64url(hmac-sha256(secret, body))}`。
/// `issued_at`/`expires_at` 为毫秒时间戳，与 dsh BrowserAuth 一致。
pub fn mint_cookie_value(secret: &[u8], authority: &str, issued_at: i64, expires_at: i64) -> String {
    let payload = format!(
        r#"{{"version":1,"authority":"{}","issuedAt":{},"expiresAt":{}}}"#,
        authority, issued_at, expires_at
    );
    let body = b64url_encode(payload.as_bytes());
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(body.as_bytes());
    let sig = b64url_encode(&mac.finalize().into_bytes());
    format!("v1.{body}.{sig}")
}

/// 一次性本地 HTTP 跳转：浏览器访问临时端口 → 响应带 Cookie 并重定向到 DSH。
/// 循环响应连接直至收到主跳转请求或超时，防浏览器预连接/冷启动导致端口提前关闭。
pub struct CookieHop {
    pub hop_url: String,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl CookieHop {
    /// 启动跳转服务；返回应打开的 hop_url。
    pub fn start(
        cookie_name: String,
        cookie_value: String,
        max_age_seconds: i64,
        redirect_to: String,
    ) -> std::io::Result<CookieHop> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let hop_url = format!("http://127.0.0.1:{port}/");
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_cloned = shutdown.clone();

        let join = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let max_serve_duration = std::time::Duration::from_secs(30);

            while start.elapsed() < max_serve_duration && !shutdown_cloned.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(1000)));
                        // 读满请求头再响应：只读一次就关连接时，未读完的请求字节
                        // 会让 Windows 发 RST（浏览器拿到连接重置 → 停在随机端口页面）。
                        let mut req: Vec<u8> = Vec::with_capacity(4096);
                        let mut chunk = [0u8; 2048];
                        loop {
                            match sock.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(n) => {
                                    req.extend_from_slice(&chunk[..n]);
                                    if req.windows(4).any(|w| w == b"\r\n\r\n") || req.len() > 16_384 {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let req_str = String::from_utf8_lossy(&req);

                        // 判断是否为针对根路径或普通页面的 GET 请求
                        let is_get = req_str.starts_with("GET ");
                        let is_favicon = req_str.contains("favicon.ico");

                        let expires = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64 + max_age_seconds)
                            .unwrap_or(0);

                        if is_get && !is_favicon {
                            // 主导航请求：发 303 + Set-Cookie，带 Location 重定向到 DSH
                            // 顺便显式设置 HTML 兜底 meta refresh / JS window.location，即使浏览器拦截 303 也双重跳转
                            let html_body = format!(
                                r#"<!DOCTYPE html><html><head><meta http-equiv="refresh" content="0;url={redirect_to}"><script>window.location.replace("{redirect_to}");</script></head><body>Redirecting to DSH...</body></html>"#
                            );
                            let resp = format!(
                                "HTTP/1.1 303 See Other\r\n\
Cache-Control: no-store\r\n\
Location: {redirect_to}\r\n\
Set-Cookie: {cookie_name}={cookie_value}; Max-Age={max_age_seconds}; Path=/; Expires={}; HttpOnly; SameSite=Strict\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n\
{html_body}",
                                http_date(expires),
                                html_body.len()
                            );
                            let _ = sock.write_all(resp.as_bytes());
                            let _ = sock.flush();
                            // 稍微留 300ms 处理可能的并发请求（如某些浏览器并行的次级连接），然后退出
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            break;
                        } else {
                            // 预检、favicon 或其他请求：快速返回 204 或 303
                            let resp = format!(
                                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
                            );
                            let _ = sock.write_all(resp.as_bytes());
                            let _ = sock.flush();
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        });

        Ok(CookieHop {
            hop_url,
            shutdown,
            join: Some(join),
        })
    }

    pub fn wait(&mut self, timeout_ms: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            if self
                .join
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(true)
            {
                if let Some(h) = self.join.take() {
                    let _ = h.join();
                }
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }
}

impl Drop for CookieHop {
    fn drop(&mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

fn http_date(unix_secs: i64) -> String {
    // Expires 仅作兜底，Max-Age 已生效；格式化成 IMF-fixdate 即可
    let secs = unix_secs.max(0) as u64;
    let days = secs / 86400;
    let rem = secs % 86400;
    // 1970-01-01 是 Thursday (4)
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days % 7) as usize];
    // 简化：用 UTC 近似（年月日用反推不完全精确，浏览器以 Max-Age 优先）
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        weekday,
        d,
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
            [(m - 1) as usize],
        y,
        hh,
        mm,
        ss
    )
}

/// days since epoch → (y, m, d)（Howard Hinnant civil_from_days）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 免 token 打开 DSH web：读密钥 → 铸 cookie → 一次性跳转注入浏览器。
/// 返回 hop_url 供测试断言。
/// yaml 里没有 secret 时（首启/写入失败）等 5s 重试；仍没有则报错，
/// 由调用方退到「等 token 行」兜底。
/// `browser_exe`：指定浏览器 exe（空串 = 系统默认），与「打开界面」下拉框一致。
pub fn open_web_via_cookie_hop(
    dsh_home: &std::path::Path,
    port: u16,
    browser_exe: &str,
) -> Result<String, String> {
    let secret = wait_browser_session_secret(dsh_home, 5_000).ok_or_else(|| {
        "未找到浏览器会话密钥（.credentials.yaml 的 client-connection/browser-session）；\
         首次启动时 dsh 会创建该密钥，稍后重试或查看 token 地址"
            .to_string()
    })?;
    let authority = format!("127.0.0.1:{port}");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let max_age_days = 30i64;
    let expires_at = now_ms + max_age_days * 86_400_000;
    let name = cookie_name(&authority);
    let value = mint_cookie_value(&secret, &authority, now_ms, expires_at);
    let redirect_to = format!("http://{authority}/");
    let mut hop = CookieHop::start(name, value, max_age_days * 86_400, redirect_to)
        .map_err(|e| format!("启动登录跳转失败: {e}"))?;
    crate::browsers::open_url_with(browser_exe, &hop.hop_url)?;
    // 等浏览器来取 cookie 并完成重定向跳转（最多 15s），完成后线程自动结束
    hop.wait(15_000);
    Ok(hop.hop_url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 功能测试：与 dsh 源码（Node crypto）同款算法必须逐字节一致。
    #[test]
    fn mint_cookie_matches_dsh_reference() {
        // 参考值由 dsh 源码同款 Node 脚本生成（issuedAt=1700000000000）
        let secret = b64url_decode("_DJYygmRe3RXVZb-SQF5wvwV6byRld315unRnHRgwEU").unwrap();
        assert_eq!(secret.len(), 32);
        let name = cookie_name("127.0.0.1:3081");
        assert_eq!(name, "dsh-auth-w3iJaA6qw3qDSBs2Itl-h4S-Y-ZeYCC-N_iZO-eI_qw");
        let value = mint_cookie_value(&secret, "127.0.0.1:3081", 1_700_000_000_000, 1_700_000_000_000 + 30 * 86_400_000);
        assert_eq!(
            value,
            "v1.eyJ2ZXJzaW9uIjoxLCJhdXRob3JpdHkiOiIxMjcuMC4wLjE6MzA4MSIsImlzc3VlZEF0IjoxNzAwMDAwMDAwMDAwLCJleHBpcmVzQXQiOjE3MDI1OTIwMDAwMDB9.LR1TUe17KXK-oMR-GgUz3VWntlfYfqTjwyfrtcu48cU"
        );
    }

    #[test]
    fn b64url_roundtrip() {
        let data = b"hello world!";
        let enc = b64url_encode(data);
        assert_eq!(b64url_decode(&enc).unwrap(), data.to_vec());
        assert!(!enc.contains('+') && !enc.contains('/') && !enc.contains('='));
    }

    #[test]
    fn read_secret_from_real_credentials_if_present() {
        // 真实 DSH_HOME 若存在则必须能读出 32 字节密钥
        let home = std::path::Path::new("C:/Users/xiaolei/.dsh-packs/better-deepseek-harness");
        if let Some(secret) = read_browser_session_secret(home) {
            assert_eq!(secret.len(), 32);
        }
    }

    #[test]
    fn missing_secret_returns_none_and_hop_reports_clear_error() {
        // yaml 缺失 / 缺记录 / 长度不对 → None；open_web_via_cookie_hop 应报「未找到密钥」并可退到 token 兜底
        let empty = std::env::temp_dir().join(format!("dshpm-nosuch-home-{}", std::process::id()));
        assert!(read_browser_session_secret(&empty).is_none(), "不存在的 home 应返回 None");
        assert!(
            wait_browser_session_secret(&empty, 300).is_none(),
            "等 300ms 仍找不到应返回 None"
        );
        let err = open_web_via_cookie_hop(&empty, 39999, "").unwrap_err();
        assert!(err.contains("未找到浏览器会话密钥"), "err={err}");

        // 有文件但没有 browser-session 记录
        let tmp = std::env::temp_dir().join(format!("dshpm-nosecret-home-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".credentials.yaml"), "version: 1\nrefs: {}\n").unwrap();
        assert!(read_browser_session_secret(&tmp).is_none());
        std::fs::remove_dir_all(&tmp).unwrap();

        // 记录在但 secret 长度不对
        let tmp2 = std::env::temp_dir().join(format!("dshpm-badsec-home-{}", std::process::id()));
        std::fs::create_dir_all(&tmp2).unwrap();
        std::fs::write(
            tmp2.join(".credentials.yaml"),
            "version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: short\n",
        )
        .unwrap();
        assert!(read_browser_session_secret(&tmp2).is_none(), "非法长度密钥应拒绝");
        std::fs::remove_dir_all(&tmp2).unwrap();
    }

    #[test]
    fn wait_secret_picks_up_file_written_later() {
        // 模拟首启竞态：home 先空，稍后 dsh 写入密钥
        let tmp = std::env::temp_dir().join(format!("dshpm-late-sec-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let writer = {
            let p = tmp.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::fs::write(
                    p.join(".credentials.yaml"),
                    "version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: _DJYygmRe3RXVZb-SQF5wvwV6byRld315unRnHRgwEU\n",
                )
                .unwrap();
            })
        };
        let secret = wait_browser_session_secret(&tmp, 3_000).expect("应等到稍后写入的密钥");
        assert_eq!(secret.len(), 32);
        writer.join().unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn cookie_hop_sets_cookie_and_redirects() {
        use std::io::{Read, Write};
        let mut hop = CookieHop::start(
            "dsh-auth-test".into(),
            "v1.abc.sig".into(),
            3600,
            "http://127.0.0.1:3081/".into(),
        )
        .unwrap();
        let url = hop.hop_url.clone();
        let port: u16 = url
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches('/')
            .parse()
            .unwrap();
        let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = String::new();
        sock.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("303"), "resp={resp}");
        assert!(resp.contains("Set-Cookie: dsh-auth-test=v1.abc.sig"), "resp={resp}");
        assert!(resp.contains("Location: http://127.0.0.1:3081/"), "resp={resp}");
        assert!(hop.wait(3000));
    }

    /// 密钥兜底查找：`dsh_home/profiles/.credentials.yaml` 与父目录结构均能读到。
    #[test]
    fn secret_found_in_profiles_subdir_or_parent_dir() {
        use std::path::PathBuf;
        let root = std::env::temp_dir().join(format!("dshpm-sec-locate-{}", std::process::id()));

        // 场景 A：密钥在 dsh_home/profiles/.credentials.yaml（打包结构差异）
        let home_a = root.join("home-a");
        std::fs::create_dir_all(home_a.join("profiles")).unwrap();
        std::fs::write(
            home_a.join("profiles").join(".credentials.yaml"),
            "version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: _DJYygmRe3RXVZb-SQF5wvwV6byRld315unRnHRgwEU\n",
        )
        .unwrap();
        let s = read_browser_session_secret(&home_a).expect("应从 profiles 子目录读到密钥");
        assert_eq!(s.len(), 32);

        // 场景 B：传入的是 profiles 目录本身，密钥在其父目录
        let home_b_root = root.join("home-b");
        let profiles_b: PathBuf = home_b_root.join("profiles");
        std::fs::create_dir_all(&profiles_b).unwrap();
        std::fs::write(
            home_b_root.join(".credentials.yaml"),
            "version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: _DJYygmRe3RXVZb-SQF5wvwV6byRld315unRnHRgwEU\n",
        )
        .unwrap();
        let s = read_browser_session_secret(&profiles_b).expect("应从父目录读到密钥");
        assert_eq!(s.len(), 32);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 修复回归：浏览器先来预连接/favicon，再发真正的 GET，
    /// 跳转服务必须继续循环监听并把真正的 GET 响应成 303 + Set-Cookie，
    /// 不能在第一个连接就退出（否则浏览器停在随机端口页面）。
    #[test]
    fn cookie_hop_survives_prefetch_then_serves_real_get() {
        use std::io::{Read, Write};
        let mut hop = CookieHop::start(
            "dsh-auth-test2".into(),
            "v1.abc.sig2".into(),
            3600,
            "http://127.0.0.1:3081/".into(),
        )
        .unwrap();
        let url = hop.hop_url.clone();
        let port: u16 = url
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches('/')
            .parse()
            .unwrap();

        // 第一个连接：favicon 预检（应被快速 204 吃掉，服务继续活着）
        let mut pre = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        pre.write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut pre_resp = String::new();
        pre.read_to_string(&mut pre_resp).unwrap();
        assert!(pre_resp.contains("204"), "预检应返回 204, resp={pre_resp}");

        // 第二个连接：真正的导航请求（必须仍然是 303 + Set-Cookie + Location）
        let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = String::new();
        sock.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("303"), "resp={resp}");
        assert!(resp.contains("Set-Cookie: dsh-auth-test2=v1.abc.sig2"), "resp={resp}");
        assert!(resp.contains("Location: http://127.0.0.1:3081/"), "resp={resp}");
        assert!(hop.wait(3000));
    }
}
