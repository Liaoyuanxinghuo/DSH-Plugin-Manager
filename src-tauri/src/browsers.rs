//! 已安装浏览器探测 + 图标提取（Windows）。
//! 注册表 `HKLM/HKCU\SOFTWARE\Clients\StartMenuInternet` 枚举浏览器客户端，
//! 图标用 ExtractAssociatedIcon 落成 PNG 缓存，供前端下拉框展示。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInfo {
    /// 稳定 id：空串 = 系统默认浏览器；其余用 exe 小写路径（全局记忆用）
    pub id: String,
    /// 展示名（如 Google Chrome）
    pub name: String,
    /// exe 路径；默认浏览器项为空串
    pub exe_path: String,
    /// data URL（image/png;base64,...）；失败则空串
    pub icon: String,
    /// 是否系统默认浏览器
    pub is_default: bool,
}

fn icon_cache_dir() -> std::path::PathBuf {
    let dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("dsh-plugin-manager")
        .join("browser-icons");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 从 exe 抽 32px 图标 → PNG data URL（磁盘缓存）。
fn icon_data_url(exe: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    exe.to_lowercase().hash(&mut h);
    let cache = icon_cache_dir().join(format!("{:016x}.png", h.finish()));
    if let Ok(bytes) = std::fs::read(&cache) {
        if !bytes.is_empty() {
            return format!("data:image/png;base64,{}", base64_encode(&bytes));
        }
    }
    // PowerShell：ExtractAssociatedIcon → PNG
    let out = cache.to_string_lossy().replace('\'', "''");
    let exe_q = exe.replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Drawing; \
         $i=[System.Drawing.Icon]::ExtractAssociatedIcon('{exe_q}'); \
         if($i){{ $b=$i.ToBitmap(); $b.Save('{out}',[System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose(); $i.Dispose(); exit 0 }} else {{ exit 1 }}"
    );
    let ok = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        if let Ok(bytes) = std::fs::read(&cache) {
            if !bytes.is_empty() {
                return format!("data:image/png;base64,{}", base64_encode(&bytes));
            }
        }
    }
    String::new()
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(T[(n >> 6) as usize & 63] as char);
        out.push(T[n as usize & 63] as char);
    }
    let pad = (3 - bytes.len() % 3) % 3;
    for _ in 0..pad {
        out.pop();
    }
    for _ in 0..pad {
        out.push('=');
    }
    out
}

/// 注册表枚举浏览器客户端：返回 (显示名, exe 路径)。
fn registry_browsers() -> Vec<(String, String)> {
    let script = r#"
$paths = @(
  'HKLM:\SOFTWARE\Clients\StartMenuInternet',
  'HKCU:\SOFTWARE\Clients\StartMenuInternet'
)
foreach ($root in $paths) {
  if (-not (Test-Path $root)) { continue }
  Get-ChildItem $root | ForEach-Object {
    $name = (Get-ItemProperty -Path $_.PSPath -ErrorAction SilentlyContinue).'(default)'
    if (-not $name) { $name = $_.PSChildName }
    $cmdKey = Join-Path $_.PSPath 'shell\open\command'
    $cmd = (Get-ItemProperty -Path $cmdKey -ErrorAction SilentlyContinue).'(default)'
    if ($cmd) {
      # 命令可能带引号/参数："C:\...\chrome.exe" -- %1
      if ($cmd -match '"([^"]+\.exe)"') { $exe = $Matches[1] }
      elseif ($cmd -match '(\S+\.exe)') { $exe = $Matches[1] }
      else { $exe = $null }
      if ($exe -and (Test-Path $exe)) { Write-Output ($name + "`t" + $exe) }
    }
  }
}
"#;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output();
    let mut seen = std::collections::HashSet::new();
    let mut list = Vec::new();
    if let Ok(o) = out {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, '\t');
            let (name, exe) = match (parts.next(), parts.next()) {
                (Some(n), Some(e)) if !n.trim().is_empty() && !e.trim().is_empty() => {
                    (n.trim().to_string(), e.trim().to_string())
                }
                _ => continue,
            };
            let key = exe.to_lowercase();
            if seen.insert(key) {
                list.push((name, exe));
            }
        }
    }
    // 常见浏览器兜底（注册表缺失时）
    let fallbacks: [(&str, &str); 8] = [
        ("Google Chrome", "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"),
        ("Google Chrome", "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe"),
        ("Microsoft Edge", "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe"),
        ("Microsoft Edge", "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe"),
        ("Mozilla Firefox", "C:\\Program Files\\Mozilla Firefox\\firefox.exe"),
        ("Brave", "C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"),
        ("Opera", "C:\\Program Files\\Opera\\launcher.exe"),
        ("Vivaldi", "C:\\Program Files\\Vivaldi\\application\\vivaldi.exe"),
    ];
    for (name, exe) in fallbacks {
        if std::path::Path::new(exe).is_file() {
            let key = exe.to_lowercase();
            if seen.insert(key) {
                list.push((name.to_string(), exe.to_string()));
            }
        }
    }
    list
}

/// 系统默认浏览器显示名（注册表 UserChoice）。
fn default_browser_name() -> Option<String> {
    let script = r#"
$progId = (Get-ItemProperty 'HKCU:\SOFTWARE\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice' -ErrorAction SilentlyContinue).ProgId
if ($progId) {
  $k = "HKLM:\SOFTWARE\Classes\$progId"
  $name = (Get-ItemProperty $k -ErrorAction SilentlyContinue).'(default)'
  if (-not $name) {
    $name = (Get-ItemProperty "$k\Application" -ErrorAction SilentlyContinue).ApplicationName
  }
  if ($name) { Write-Output $name }
}
"#;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// 枚举浏览器：第一项固定为「默认浏览器」，其后是已安装浏览器（带图标）。
pub fn list_browsers() -> Vec<BrowserInfo> {
    let mut result = Vec::new();
    let def_name = default_browser_name().unwrap_or_else(|| "默认浏览器".to_string());
    result.push(BrowserInfo {
        id: String::new(),
        name: format!("{def_name}（默认）"),
        exe_path: String::new(),
        icon: default_browser_icon(),
        is_default: true,
    });
    for (name, exe) in registry_browsers() {
        result.push(BrowserInfo {
            id: exe.to_lowercase(),
            name,
            icon: icon_data_url(&exe),
            exe_path: exe,
            is_default: false,
        });
    }
    result
}

/// 默认浏览器项的图标：用系统默认浏览器 exe 抽取，失败给空。
fn default_browser_icon() -> String {
    let script = r#"
$progId = (Get-ItemProperty 'HKCU:\SOFTWARE\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice' -ErrorAction SilentlyContinue).ProgId
if ($progId) {
  $cmd = (Get-ItemProperty "HKLM:\SOFTWARE\Classes\$progId\shell\open\command" -ErrorAction SilentlyContinue).'(default)'
  if (-not $cmd) { $cmd = (Get-ItemProperty "HKCU:\SOFTWARE\Classes\$progId\shell\open\command" -ErrorAction SilentlyContinue).'(default)' }
  if ($cmd -match '"([^"]+\.exe)"') { Write-Output $Matches[1] }
  elseif ($cmd -match '(\S+\.exe)') { Write-Output $Matches[1] }
}
"#;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output();
    if let Ok(o) = out {
        let exe = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !exe.is_empty() {
            return icon_data_url(&exe);
        }
    }
    String::new()
}

/// 用指定浏览器打开 URL；exe 为空 → 系统默认（rundll32 协议分发）。
pub fn open_url_with(exe_path: &str, url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL 为空".to_string());
    }
    if exe_path.trim().is_empty() {
        return crate::fsutil::open_url(url);
    }
    let exe = exe_path.trim();
    if !std::path::Path::new(exe).is_file() {
        return Err(format!("浏览器不存在: {exe}"));
    }
    std::process::Command::new(exe)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开浏览器: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_browsers_always_has_default_first() {
        let list = list_browsers();
        assert!(!list.is_empty(), "至少有默认浏览器项");
        assert!(list[0].is_default);
        assert_eq!(list[0].id, "");
        // 其余项 exe 必须存在
        for b in &list[1..] {
            assert!(std::path::Path::new(&b.exe_path).is_file(), "exe 应存在: {}", b.exe_path);
            assert!(!b.id.is_empty());
        }
    }

    #[test]
    fn open_url_with_missing_exe_is_error() {
        let err = open_url_with("C:\\no-such-browser.exe", "http://127.0.0.1/").unwrap_err();
        assert!(err.contains("浏览器不存在"), "err={err}");
        assert!(open_url_with("C:\\x.exe", "").is_err());
    }

    #[test]
    fn base64_encode_known_values() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn icon_data_url_returns_png_or_empty() {
        // 对真实 exe（powershell）抽图标：Windows 上应能拿到 PNG data URL
        let icon = icon_data_url("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        assert!(icon.is_empty() || icon.starts_with("data:image/png;base64,"));
    }

    /// 系统测试：真实浏览器经 open_url_with 访问本地端口（会打开一个标签页）。
    /// 默认 cargo test --lib 不跑；系统测试阶段用 `cargo test -- --ignored` 单独执行。
    #[test]
    #[ignore = "系统测试：真实拉起浏览器标签页，按需单独运行"]
    fn system_open_real_browser_hits_local_server() {
        use std::io::Read;
        use std::sync::mpsc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let body = "<html><body>系统测试：浏览器打开链路正常</body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n{body}"
                );
                let _ = std::io::Write::write_all(&mut sock, resp.as_bytes());
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            }
        });
        // 探测列表里第一个真实浏览器（非默认项有 exe 路径）
        let list = list_browsers();
        let b = list
            .iter()
            .find(|x| !x.exe_path.is_empty())
            .expect("本机应探测到至少一个已安装浏览器");
        // 图标必须成功抽出为 PNG data URL（验证 ExtractAssociatedIcon + base64 链路）
        assert!(
            b.icon.starts_with("data:image/png;base64,"),
            "浏览器图标应为 PNG data URL, icon={}",
            b.icon
        );
        let url = format!("http://127.0.0.1:{port}/dshpm-sys-test");
        open_url_with(&b.exe_path, &url).expect("拉起浏览器失败");
        let req = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("浏览器应在 20s 内访问本地端口");
        assert!(req.contains("dshpm-sys-test"), "浏览器请求应命中测试路径, req={req}");
    }

    /// 系统测试：exe 为空走系统默认浏览器（rundll32 协议分发），同样应访问本地端口。
    #[test]
    #[ignore = "系统测试：真实拉起默认浏览器标签页，按需单独运行"]
    fn system_default_browser_hits_local_server() {
        use std::io::Read;
        use std::sync::mpsc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let _ = std::io::Write::write_all(
                    &mut sock,
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\nok",
                );
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            }
        });
        let url = format!("http://127.0.0.1:{port}/dshpm-sys-default");
        open_url_with("", &url).expect("默认浏览器打开失败");
        let req = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("默认浏览器应在 20s 内访问本地端口");
        assert!(req.contains("dshpm-sys-default"), "req={req}");
    }
}
