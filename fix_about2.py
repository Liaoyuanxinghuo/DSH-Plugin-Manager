# -*- coding: utf-8 -*-
p = 'E:/workspace/dshcjaz/src-tauri/src/lib.rs'
s = open(p, encoding='utf-8').read()

old = '''/// 检查更新：GitHub releases/latest（走用户配置的 GitHub 镜像），与当前版本比较
#[tauri::command]
fn check_update_cmd() -> Result<serde_json::Value, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let repo_url = format!("https://github.com/{PROJECT_REPO}");
    let api_url = format!("https://api.github.com/repos/{PROJECT_REPO}/releases/latest");
    let mirror = settings::load_settings().github_mirror;
    let fetch_url = settings::github_proxy(&api_url, &mirror);

    let resp = match reqwest::blocking::Client::new()
        .get(&fetch_url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(serde_json::json!({
                "current": current,
                "latest": "",
                "hasUpdate": false,
                "url": repo_url,
                "error": format!("检查更新失败：{e}（请检查网络或 GitHub 镜像设置）"),
            }));
        }
    };
    if !resp.status().is_success() {
        return Ok(serde_json::json!({
            "current": current,
            "latest": "",
            "hasUpdate": false,
            "url": repo_url,
            "error": format!("检查更新失败：GitHub API 返回状态 {}", resp.status()),
        }));
    }
    let json: serde_json::Value = match resp.json() {
        Ok(j) => j,
        Err(e) => {
            return Ok(serde_json::json!({
                "current": current,
                "latest": "",
                "hasUpdate": false,
                "url": repo_url,
                "error": format!("检查更新失败：解析响应出错（{e}）"),
            }));
        }
    };
    let tag = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let latest = tag.trim_start_matches('v').to_string();
    let has_update = !latest.is_empty() && compare_versions(&latest, &current) > 0;
    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        "url": repo_url,
        "error": "",
    }))
}'''
new = '''/// 走 GitHub 镜像拉取文本（raw 文件 / API），返回 (HTTP 状态码, 响应文本)
fn github_get_text(fetch_url: &str) -> (u16, String) {
    match reqwest::blocking::Client::new()
        .get(fetch_url)
        .timeout(std::time::Duration::from_secs(25))
        .send()
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            (status, body)
        }
        Err(e) => (0, format!("请求失败: {e}")),
    }
}

/// 检查更新（走 GitHub 镜像，大陆无 VPN 可用）：
/// 1) 优先 GitHub releases/latest API；2) 回退仓库根 VERSION 文件（main / master）。
#[tauri::command]
fn check_update_cmd() -> Result<serde_json::Value, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let repo_url = format!("https://github.com/{PROJECT_REPO}");
    let mirror = settings::load_settings().github_mirror;

    let mut latest = String::new();
    let mut err_hint = String::new();

    // 1) GitHub API（走镜像）
    let api_url = format!("https://api.github.com/repos/{PROJECT_REPO}/releases/latest");
    let (status, body) = github_get_text(&settings::github_proxy(&api_url, &mirror));
    if status == 200 {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(tag) = json.get("tag_name").and_then(|t| t.as_str()) {
                latest = tag.trim_start_matches('v').to_string();
            }
        }
        if latest.is_empty() {
            err_hint = "GitHub API 响应中未找到版本号".to_string();
        }
    } else if status != 0 && status != 404 {
        err_hint = format!("GitHub API 返回状态 {status}");
    }

    // 2) 回退：仓库根 VERSION 文件（main / master，走镜像）
    if latest.is_empty() {
        for branch in ["main", "master"] {
            let raw_url = format!(
                "https://raw.githubusercontent.com/{PROJECT_REPO}/{branch}/VERSION"
            );
            let (status2, body2) = github_get_text(&settings::github_proxy(&raw_url, &mirror));
            if status2 == 200 {
                let v = body2.trim().trim_start_matches('v').to_string();
                if !v.is_empty() && v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    latest = v;
                    break;
                }
            }
        }
        if latest.is_empty() && err_hint.is_empty() {
            err_hint = "未找到发布版本（GitHub API 与 VERSION 文件均不可用）".to_string();
        }
    }

    let has_update = !latest.is_empty() && compare_versions(&latest, &current) > 0;
    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        "url": repo_url,
        "error": if err_hint.is_empty() { "" } else { format!("检查更新失败：{err_hint}（请检查网络或 GitHub 镜像设置）") },
    }))
}'''
assert old in s, 'check_update_cmd 未找到'
s = s.replace(old, new, 1)
open(p, 'w', encoding='utf-8', newline='').write(s)
print('OK')
