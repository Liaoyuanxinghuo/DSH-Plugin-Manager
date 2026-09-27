//! cordis.patch.yml 文本级编辑（保留注释与原有结构）
//! 停用插件 = 条目加 disabled: true；启用 = 移除 disabled。
//! 必须逐行处理，不能用 YAML 全量序列化（会丢失注释）。

/// 在 patch 内容中设置某插件（服务 id）的 disabled 状态。
/// 返回新的完整内容。
pub fn set_plugin_disabled(content: &str, plugin_id: &str, disabled: bool) -> Result<String, String> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return Err("插件 id 不能为空".to_string());
    }
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let orig_has_tail = content.ends_with('\n');
    // lines() 按 \n 分割会保留 CRLF 的 \r，需剥离（join 时统一用 eol）
    let lines: Vec<String> = content
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    let join = |out: Vec<String>| -> String {
        let mut s = out.join(eol);
        if orig_has_tail && !s.ends_with(eol) {
            s.push_str(eol);
        }
        s
    };

    // 找出条目（- id: X）所在行
    let mut entry_idx: Option<usize> = None;
    let mut entry_indent = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("- id:") {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let id = trimmed[5..].trim();
        if id == plugin_id {
            entry_idx = Some(i);
            entry_indent = indent;
            break;
        }
    }

    let mut out: Vec<String> = lines.clone();

    match entry_idx {
        Some(idx) => {
            // 计算条目范围：从 id 行到下一个同缩进 `- ` 行或结束
            let mut end = idx + 1;
            while end < out.len() {
                let l = out[end].trim_start();
                if l.starts_with("- ") {
                    let indent = out[end].len() - out[end].trim_start().len();
                    if indent == entry_indent {
                        break;
                    }
                }
                end += 1;
            }
            // 在条目内查找 disabled 键
            let mut dis_idx: Option<usize> = None;
            let mut dis_val: Option<bool> = None;
            for j in idx..end {
                let t = out[j].trim_start();
                if let Some(rest) = t.strip_prefix("disabled:") {
                    dis_idx = Some(j);
                    dis_val = rest.trim().parse::<bool>().ok();
                    break;
                }
            }
            let indent_str = " ".repeat(entry_indent + 2);
            if disabled {
                match (dis_idx, dis_val) {
                    (Some(_), Some(true)) => Ok(join(out)), // 已是禁用
                    (Some(j), _) => {
                        // 有 disabled 但为 false/无法解析 → 改为 true
                        out[j] = format!("{indent_str}disabled: true");
                        Ok(join(out))
                    }
                    (None, _) => {
                        // 无 disabled → 在条目末尾插入
                        out.insert(end, format!("{indent_str}disabled: true"));
                        Ok(join(out))
                    }
                }
            } else {
                // 启用：删除 disabled 行
                match dis_idx {
                    Some(j) => {
                        out.remove(j);
                        // 若条目只剩 id 行 → 删除整个条目（含其后空行/注释）
                        let new_end = end.saturating_sub(1);
                        let mut only_id = true;
                        for k in (idx + 1)..new_end {
                            let t = out[k].trim_start();
                            if !t.is_empty() && !t.starts_with('#') {
                                only_id = false;
                                break;
                            }
                        }
                        if only_id {
                            // 移除 id 行及紧随的空行
                            out.remove(idx);
                            if idx < out.len() && out[idx].trim().is_empty() {
                                out.remove(idx);
                            }
                        }
                        Ok(join(out))
                    }
                    None => Ok(content.to_string()), // 已启用，原样返回
                }
            }
        }
        None => {
            if disabled {
                // 追加新条目
                let mut add = vec![format!("- id: {plugin_id}"), format!("  disabled: true")];
                if !out.is_empty() {
                    add.insert(0, String::new()); // 前置空行分隔
                }
                out.extend(add);
                Ok(join(out))
            } else {
                Ok(content.to_string()) // 无条目且要启用 → 不变
            }
        }
    }
}

/// 从 patch 内容列出已禁用插件的 id 列表
pub fn list_disabled_ids(content: &str) -> Vec<String> {
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut disabled = Vec::new();
    let mut current_id: Option<String> = None;
    let mut id_indent = 0usize;
    for line in lines {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if let Some(rest) = t.strip_prefix("- id:") {
            current_id = Some(rest.trim().to_string());
            id_indent = indent;
            continue;
        }
        if let Some(rest) = t.strip_prefix("disabled:") {
            if let Some(id) = &current_id {
                // 确认这条 disabled 属于当前条目（缩进大于 id 行）
                if indent > id_indent && rest.trim() == "true" {
                    disabled.push(id.clone());
                }
            }
        }
        // 条目边界：新的顶层条目 → 重置 current_id
        if t.starts_with("- ") && !t.starts_with("- id:") && current_id.is_some() {
            current_id = None;
        }
    }
    disabled
}

/// 从 npm 包名推导 cordis 服务 id（@scope/dsh-xxx → xxx；xxx → xxx）
/// 注意：真实服务 id 以 dsh --dump-config 为准（如 dshmarket → dsh-market），
/// 此推导仅作 fallback。
pub fn derive_service_id(package_name: &str) -> String {
    let base = package_name.rsplit('/').next().unwrap_or(package_name);
    base.strip_prefix("dsh-").unwrap_or(base).to_string()
}

/// 解析 dsh --dump-config 输出，构建「包名 → 服务 id」映射。
/// 输出格式（每段）：
///   # == <something>
///   - id: dsh-market
///     name: dshmarket
pub fn parse_service_ids(dump: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut current_id: Option<String> = None;
    for line in dump.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- id:") {
            current_id = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = t.strip_prefix("name:") {
            if let Some(id) = &current_id {
                let name = rest.trim().trim_matches('"').trim_matches('\'').to_string();
                map.insert(name, id.clone());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# 我的补丁注释
- id: llm-pi-ai
  name: \"@deepseek-ai/dsh-llm-pi-ai\"
  config:
    provider: xiaomi
- id: include
  disabled: true
- id: agent-default-model
  name: \"@deepseek-ai/dsh-agent-default-model\"
";

    #[test]
    fn test_disable_existing() {
        let out = set_plugin_disabled(SAMPLE, "llm-pi-ai", true).unwrap();
        assert!(out.contains("# 我的补丁注释"), "注释必须保留");
        let lines: Vec<&str> = out.lines().collect();
        let idx = lines.iter().position(|l| l.contains("llm-pi-ai")).unwrap();
        assert!(lines[idx + 1..].iter().any(|l| l.trim() == "disabled: true"));
        // 原有 include 不动
        assert!(out.contains("- id: include\n  disabled: true"));
    }

    #[test]
    fn test_enable_existing() {
        let out = set_plugin_disabled(SAMPLE, "include", false).unwrap();
        assert!(!out.contains("- id: include"), "空条目应被删除");
        assert!(out.contains("# 我的补丁注释"));
        // 其他条目保留
        assert!(out.contains("llm-pi-ai"));
        assert!(out.contains("agent-default-model"));
    }

    #[test]
    fn test_disable_new() {
        let out = set_plugin_disabled(SAMPLE, "brand-new-plugin", true).unwrap();
        assert!(out.contains("- id: brand-new-plugin"));
        let lines: Vec<&str> = out.lines().collect();
        let idx = lines.iter().position(|l| l.contains("brand-new-plugin")).unwrap();
        assert!(lines[idx + 1].trim() == "disabled: true");
    }

    #[test]
    fn test_enable_non_existing_noop() {
        let out = set_plugin_disabled(SAMPLE, "nope", false).unwrap();
        assert_eq!(out, SAMPLE);
    }

    #[test]
    fn test_roundtrip_disable_enable() {
        let out1 = set_plugin_disabled(SAMPLE, "agent-default-model", true).unwrap();
        assert!(out1.contains("disabled: true"));
        let out2 = set_plugin_disabled(&out1, "agent-default-model", false).unwrap();
        // agent-default-model 条目（有 name/config）禁用再启用后应还原
        assert!(out2.contains("name: \"@deepseek-ai/dsh-agent-default-model\""));
        let lines: Vec<&str> = out2.lines().collect();
        let idx = lines.iter().position(|l| l.contains("agent-default-model")).unwrap();
        let mut has_dis = false;
        for l in &lines[idx + 1..] {
            if l.trim_start().starts_with("- ") {
                break;
            }
            if l.trim().starts_with("disabled:") {
                has_dis = true;
            }
        }
        assert!(!has_dis, "agent-default-model 条目内不应再有 disabled");
        // include 的禁用保持
        assert!(out2.contains("- id: include\n  disabled: true"));
    }

    #[test]
    fn test_disable_enable_with_crlf() {
        let sample_crlf = SAMPLE.replace('\n', "\r\n");
        let out = set_plugin_disabled(&sample_crlf, "llm-pi-ai", true).unwrap();
        assert!(out.contains("\r\n"), "CRLF 应保留");
        assert!(out.contains("disabled: true"));
        // 再次禁用（已禁用）应原样返回
        let noop = set_plugin_disabled(&out, "llm-pi-ai", true).unwrap();
        assert_eq!(noop, out);
        // 启用还原
        let enabled = set_plugin_disabled(&out, "llm-pi-ai", false).unwrap();
        assert!(enabled.contains("llm-pi-ai"));
        let lines: Vec<&str> = enabled.lines().collect();
        let idx = lines.iter().position(|l| l.contains("llm-pi-ai")).unwrap();
        let mut has_dis = false;
        for l in &lines[idx + 1..] {
            if l.trim_start().starts_with("- ") {
                break;
            }
            if l.trim().starts_with("disabled:") {
                has_dis = true;
            }
        }
        assert!(!has_dis, "llm-pi-ai 条目内不应再有 disabled");
        // include 的禁用保留
        assert!(enabled.contains("- id: include"));
    }

    #[test]
    fn test_list_disabled() {
        let disabled = list_disabled_ids(SAMPLE);
        assert_eq!(disabled, vec!["include"]);
    }

    #[test]
    fn test_derive_service_id() {
        assert_eq!(derive_service_id("@deepseek-ai/dsh-llm-pi-ai"), "llm-pi-ai");
        assert_eq!(derive_service_id("@deepseek-ai/dsh-agent-default-model"), "agent-default-model");
        assert_eq!(derive_service_id("@michengai/dsh-codex-ui"), "codex-ui");
        assert_eq!(derive_service_id("dshmarket"), "dshmarket");
    }

    #[test]
    fn test_parse_service_ids() {
        let dump = "# == dshmarket\n- id: dsh-market\n  name: dshmarket\n# == llm\n- id: llm-pi-ai\n  name: \"@deepseek-ai/dsh-llm-pi-ai\"\n";
        let map = parse_service_ids(dump);
        assert_eq!(map.get("dshmarket").map(String::as_str), Some("dsh-market"));
        assert_eq!(map.get("@deepseek-ai/dsh-llm-pi-ai").map(String::as_str), Some("llm-pi-ai"));
        assert_eq!(map.len(), 2);
    }
}
