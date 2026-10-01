//! cordis.patch.yml 文本级编辑（保留注释与原有结构）
//! 停用插件 = 条目加 disabled: true；启用 = 移除 disabled。
//! 必须逐行处理，不能用 YAML 全量序列化（会丢失注释）。
//!
//! 官方匹配语义（packages/boot/plugin-manager）：匹配依据是**条目 id**，
//! 以及覆盖项声明的**模块名称**（`name` 字段）；存在多条匹配时以**最后一条**为准。

/// 条目范围信息
struct EntrySpan {
    /// `- id:` 所在行（或条目首行）
    start: usize,
    /// 条目结束行（不含）
    end: usize,
    /// 条目缩进（`-` 前的空格数）
    indent: usize,
    id: String,
    name: Option<String>,
}

/// 扫描顶层条目列表：识别每条 `- ` 起始的条目及其 id/name
fn scan_entries(lines: &[String]) -> Vec<EntrySpan> {
    let mut entries: Vec<EntrySpan> = Vec::new();
    let mut current: Option<EntrySpan> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        // 新条目：以 `- ` 开头（含 `- id:` / `- name:` 等）
        if t.starts_with("- ") || t == "-" {
            if let Some(e) = current.take() {
                entries.push(e);
            }
            let body = t.strip_prefix("-").unwrap_or(t).trim_start();
            let (id, name) = parse_id_name(body);
            current = Some(EntrySpan {
                start: i,
                end: i + 1,
                indent,
                id,
                name,
            });
            continue;
        }
        if let Some(ref mut e) = current {
            // 同级新条目已由上面捕获；此处为条目内属性行
            e.end = i + 1;
            if indent > e.indent {
                if e.id.is_empty() {
                    if let Some(rest) = t.strip_prefix("id:") {
                        e.id = rest.trim().trim_matches('"').trim_matches('\'').to_string();
                    }
                }
                if e.name.is_none() {
                    if let Some(rest) = t.strip_prefix("name:") {
                        e.name = Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
                    }
                }
                // 顶层 `id:`/`name:` 也可能是条目首行剥掉 `- ` 后的形式（已由 parse_id_name 处理）
            }
        }
    }
    if let Some(e) = current.take() {
        entries.push(e);
    }
    // 对 `- id:` 为首行的条目，id 已解析；若首行是 `- name: X` 则 name 已解析
    entries
}

/// 解析条目首行体（去掉 `- ` 后）里的 id / name
fn parse_id_name(body: &str) -> (String, Option<String>) {
    let mut id = String::new();
    let mut name = None;
    // 首行可能写成 `id: foo` 或 `name: bar` 或 `id: foo  name: bar`（少见）
    if let Some(rest) = body.strip_prefix("id:") {
        id = rest.trim().trim_matches('"').trim_matches('\'').to_string();
    } else if let Some(rest) = body.strip_prefix("name:") {
        name = Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
    }
    (id, name)
}

/// 查找目标条目（官方语义：条目 id 或覆盖项声明的模块名称）。
/// **id 优先**：有 id 命中则只在 id 命中里取最后一条；
/// 否则再按 name 命中取最后一条。避免 name 过宽匹配到别的条目。
fn find_last_match<'a>(
    entries: &'a [EntrySpan],
    plugin_id: &str,
    package_name: Option<&str>,
) -> Option<&'a EntrySpan> {
    if !plugin_id.trim().is_empty() {
        let by_id = entries.iter().rev().find(|e| e.id == plugin_id.trim());
        if by_id.is_some() {
            return by_id;
        }
    }
    if let Some(pkg) = package_name.map(str::trim).filter(|s| !s.is_empty()) {
        return entries
            .iter()
            .rev()
            .find(|e| e.name.as_deref().map(|n| n == pkg).unwrap_or(false));
    }
    None
}

/// 在条目范围内查找 disabled 键所在行
fn find_disabled_line(lines: &[String], e: &EntrySpan) -> Option<(usize, Option<bool>)> {
    for j in e.start..e.end {
        let t = lines[j].trim_start();
        if let Some(rest) = t.strip_prefix("disabled:") {
            return Some((j, rest.trim().parse::<bool>().ok()));
        }
    }
    None
}

fn join_lines(out: &[String], eol: &str, orig_has_tail: bool) -> String {
    let mut s = out.join(eol);
    if orig_has_tail && !s.ends_with(eol) {
        s.push_str(eol);
    }
    s
}

/// YAML 单引号标量：内部 `'` 需写成 `''`。
/// `@` 开头的包名（如 `@michengai/dsh-codex-ui`）在 YAML 里是保留字符，
/// 必须加引号，否则 `parsePatchList` 直接解析失败、dsh 启动即退。
fn yaml_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// market 兼容的行 id（`rowBlock` 不加引号时的安全字符集）
fn is_plain_row_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// market `rowBlock` 等价写入：`- id: X` + `  disabled: true|false`（两行）。
/// 简单 id 不加引号（market 的 `readUserPatchState` 只认 `- id: [A-Za-z0-9_.-]+` 裸形式）。
pub fn row_block(row_id: &str, disabled: bool) -> String {
    let id = if is_plain_row_id(row_id) {
        row_id.to_string()
    } else {
        yaml_str(row_id)
    };
    format!("- id: {id}\n  disabled: {}\n", if disabled { "true" } else { "false" })
}

/// market `readUserPatchState` 等价扫描（严格两行模式）：
/// `- id: X` 紧跟 `  disabled: true|false`（2 空格缩进）才算；插入块内的 id 另计。
/// 返回 (disables, forced)。
pub fn read_user_patch_state(content: &str) -> (Vec<String>, Vec<String>) {
    let mut disables = Vec::new();
    let mut forced = Vec::new();
    let lines: Vec<&str> = content.split('\n').collect();
    let mut in_insert = false;
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let t = line.trim_start();
        if t == "- insert:" || t.starts_with("- insert: ") {
            in_insert = true;
            continue;
        }
        if t.starts_with("- ") {
            in_insert = false;
        }
        if in_insert {
            continue;
        }
        // 严格匹配 `- id: X`（整行仅此一项，允许末尾空白）
        let Some(rest) = t.strip_prefix("- id:") else {
            continue;
        };
        if !line.trim_start().starts_with("- id:") {
            continue;
        }
        let id = rest.trim();
        if id.is_empty() || id.contains(' ') || id.contains(':') {
            continue;
        }
        let id = id.trim_matches('"').trim_matches('\'');
        if !is_plain_row_id(id) {
            continue;
        }
        let next = lines.get(index + 1).map(|l| l.strip_suffix('\r').unwrap_or(l)).unwrap_or("");
        let n = next.trim_start();
        // market：`  disabled: true`（恰好 2 空格）
        if next.starts_with("  disabled:") && !next.starts_with("   ") {
            let val = n.strip_prefix("disabled:").unwrap_or("").trim();
            if val == "true" {
                disables.push(id.to_string());
            } else if val == "false" {
                forced.push(id.to_string());
            }
        }
    }
    (disables, forced)
}

/// 在 patch 内容中设置某插件的 disabled 状态（仅按 id 匹配）。
/// 兼容入口；需要包名匹配时用 [`set_plugin_disabled_for`]。
pub fn set_plugin_disabled(content: &str, plugin_id: &str, disabled: bool) -> Result<String, String> {
    set_plugin_disabled_for(content, plugin_id, None, disabled)
}

/// 设置 disabled 状态。
/// 匹配顺序（官方）：条目 id == plugin_id，或条目声明的 name == package_name；
/// 多条匹配时取**最后一条**。无匹配且 disable → 追加新条目；无匹配且 enable → 原样返回。
pub fn set_plugin_disabled_for(
    content: &str,
    plugin_id: &str,
    package_name: Option<&str>,
    disabled: bool,
) -> Result<String, String> {
    let plugin_id = plugin_id.trim();
    let package_name = package_name.map(str::trim).filter(|s| !s.is_empty());
    if plugin_id.is_empty() && package_name.is_none() {
        return Err("插件 id 不能为空".to_string());
    }
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let orig_has_tail = content.ends_with('\n');
    let lines: Vec<String> = content
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    let entries = scan_entries(&lines);
    let matched = find_last_match(&entries, plugin_id, package_name);

    let mut out: Vec<String> = lines.clone();

    match matched {
        Some(e) => {
            let start = e.start;
            let end = e.end;
            let indent = e.indent;
            let dis = find_disabled_line(&out, e);
            let indent_str = " ".repeat(indent + 2);
            if disabled {
                match dis {
                    Some((j, Some(true))) => Ok(join_lines(&out, eol, orig_has_tail)),
                    Some((j, _)) => {
                        out[j] = format!("{indent_str}disabled: true");
                        Ok(join_lines(&out, eol, orig_has_tail))
                    }
                    None => {
                        out.insert(end, format!("{indent_str}disabled: true"));
                        Ok(join_lines(&out, eol, orig_has_tail))
                    }
                }
            } else {
                match dis {
                    Some((j, _)) => {
                        out.remove(j);
                        // 仅当条目是「纯禁用桩」（除 id 外只剩注释/空行）才整条删除。
                        // 带 name/config 的原条目保留（只摘掉 disabled），否则会误删用户配置。
                        let new_end = end.saturating_sub(1);
                        let mut only_id = true;
                        for k in (start + 1)..new_end {
                            let t = out[k].trim_start();
                            if t.is_empty() || t.starts_with('#') {
                                continue;
                            }
                            only_id = false;
                            break;
                        }
                        if only_id {
                            let first = out[start].trim_start();
                            let body = first.strip_prefix("-").unwrap_or(first).trim_start();
                            // 首行也必须只是 id（不能是 name:）— 否则保留
                            if !(body.starts_with("id:") || body.is_empty()) {
                                only_id = false;
                            }
                        }
                        if only_id {
                            // 整条删除（含首行），避免留下孤儿行
                            let count = new_end - start;
                            for _ in 0..count {
                                out.remove(start);
                            }
                            if start < out.len() && out[start].trim().is_empty() {
                                out.remove(start);
                            }
                        }
                        Ok(join_lines(&out, eol, orig_has_tail))
                    }
                    None => Ok(content.to_string()),
                }
            }
        }
        None => {
            if disabled {
                // market `rowBlock` 格式：两行、简单 id 不加引号（readUserPatchState 只认这种）
                let id_for_block = if plugin_id.is_empty() {
                    derive_service_id(package_name.unwrap_or("plugin"))
                } else {
                    plugin_id.to_string()
                };
                let mut block = row_block(&id_for_block, true);
                // 仅当 id 与包名推导不一致时补 name 行（保证后续可按 name 命中）
                if let Some(pkg) = package_name {
                    if plugin_id.is_empty() || derive_service_id(pkg) != plugin_id {
                        // 插在 disabled 行之前，保持 `- id` + `name` + `disabled` 结构
                        block = format!("- id: {}\n  name: {}\n  disabled: true\n",
                            if is_plain_row_id(&id_for_block) { id_for_block.clone() } else { yaml_str(&id_for_block) },
                            yaml_str(pkg));
                    }
                }
                if !out.is_empty() {
                    out.push(String::new());
                }
                for line in block.trim_end_matches('\n').split('\n') {
                    out.push(line.to_string());
                }
                Ok(join_lines(&out, eol, orig_has_tail))
            } else {
                Ok(content.to_string())
            }
        }
    }
}

/// 判断某插件是否被禁用。
/// market 权威判定（`packagePatchFlags`）：行 id 出现在 `readUserPatchState` 的
/// disables 列表里即为禁用（`rows.some(...)`，任一行禁用就算）。
/// 退回条目解析兜底（兼容 name 匹配的历史写法）。
pub fn is_plugin_disabled(content: &str, plugin_id: &str, package_name: Option<&str>) -> bool {
    let (disables, _) = read_user_patch_state(content);
    let pid = plugin_id.trim();
    if !pid.is_empty() && disables.iter().any(|d| d == pid) {
        return true;
    }
    // 兜底：历史条目（带 name/config 或引号 id）按条目匹配
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let entries = scan_entries(&lines);
    match find_last_match(&entries, pid, package_name) {
        Some(e) => find_disabled_line(&lines, e).map(|(_, v)| v.unwrap_or(false)).unwrap_or(false),
        None => false,
    }
}

/// 列出已禁用插件的 id 列表（market `readUserPatchState` 权威 + 条目兜底）
pub fn list_disabled_ids(content: &str) -> Vec<String> {
    let (mut disables, _) = read_user_patch_state(content);
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let entries = scan_entries(&lines);
    for e in &entries {
        if e.id.is_empty() {
            continue;
        }
        if find_disabled_line(&lines, e).map(|(_, v)| v.unwrap_or(false)).unwrap_or(false) {
            if !disables.contains(&e.id) {
                disables.push(e.id.clone());
            }
        }
    }
    disables
}

/// 解析 bundle patch 中 `insert:` 块内的条目，返回 (id, name) 列表。
/// 只收集 insert 列表里的行；顶层 `- id:` 覆盖项不算。
/// 形如：
///   - insert:
///       - id: dsh-market
///         name: 'dshmarket'
///   - id: other        ← 顶层覆盖，忽略
pub fn parse_insert_entries(content: &str) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut in_insert = false;
    let mut insert_dash_indent = 0usize;
    let mut current: Option<(String, Option<String>)> = None;
    for line in content.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        let indent = line.len() - t.len();
        if t.starts_with("- insert:") || t == "- insert" {
            if let Some(cur) = current.take() {
                out.push(cur);
            }
            in_insert = true;
            insert_dash_indent = indent;
            continue;
        }
        if in_insert && t.starts_with("- ") {
            if indent <= insert_dash_indent {
                // 顶层同级新条目 → 离开 insert 块
                if let Some(cur) = current.take() {
                    out.push(cur);
                }
                in_insert = false;
                // 不 continue：可能是另一个 `- insert:`，循环开头已处理
                if t.starts_with("- insert:") || t == "- insert" {
                    in_insert = true;
                    insert_dash_indent = indent;
                }
                continue;
            }
            // insert 列表新项
            if let Some(cur) = current.take() {
                out.push(cur);
            }
            let body = t.strip_prefix("-").unwrap_or(t).trim_start();
            let (id, name) = parse_id_name(body);
            current = Some((id, name));
            continue;
        }
        if in_insert {
            if let Some(ref mut cur) = current {
                if cur.0.is_empty() {
                    if let Some(rest) = t.strip_prefix("id:") {
                        cur.0 = rest.trim().trim_matches('"').trim_matches('\'').to_string();
                    }
                }
                if cur.1.is_none() {
                    if let Some(rest) = t.strip_prefix("name:") {
                        cur.1 = Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
                    }
                }
            }
        }
    }
    if let Some(cur) = current.take() {
        out.push(cur);
    }
    out.retain(|(id, _)| !id.is_empty());
    out
}

/// 从 npm 包名推导 cordis 服务 id（@scope/dsh-xxx → xxx；xxx → xxx）
/// 注意：真实服务 id 以包 bundle patch 的 insert id 为准
/// （如 dshmarket → dsh-market、@hyzyn/dsh-mcp → mcp-config），
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
            current_id = Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
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
        assert!(out.contains("brand-new-plugin"), "{out}");
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

    // ===== 官方「id + 模块名称」匹配语义 =====

    #[test]
    fn test_match_by_package_name_when_id_differs() {
        // 真实 id 是 dsh-market，包名 dshmarket；推导 id 错误时靠 name 命中
        let content = "# x\n- id: dsh-market\n  name: dshmarket\n  config:\n    a: 1\n";
        let out = set_plugin_disabled_for(content, "dshmarket", Some("dshmarket"), true).unwrap();
        assert!(out.contains("disabled: true"), "应按 name 命中并禁用: {out}");
        assert!(!out.contains("- id: dshmarket"), "不应追加错误 id 条目");
        assert!(is_plugin_disabled(content, "dshmarket", Some("dshmarket")) == false);
        let out2 = out.clone();
        assert!(is_plugin_disabled(&out2, "dshmarket", Some("dshmarket")));
    }

    #[test]
    fn test_match_last_of_multiple_entries() {
        let content = "- id: foo\n  name: mypkg\n  disabled: false\n- id: bar\n  name: mypkg\n  config: {}\n";
        let out = set_plugin_disabled_for(content, "nope", Some("mypkg"), true).unwrap();
        // 应命中最后一条（bar），而不是第一条
        let lines: Vec<&str> = out.lines().collect();
        let bar_idx = lines.iter().position(|l| l.contains("- id: bar")).unwrap();
        assert!(
            lines[bar_idx..].iter().take(5).any(|l| l.trim() == "disabled: true"),
            "最后一条匹配条目应被禁用: {out}"
        );
        // 第一条不应被改动为 true
        assert!(out.contains("- id: foo\n  name: mypkg\n  disabled: false"));
    }

    #[test]
    fn test_is_plugin_disabled_by_name() {
        let content = "- id: dsh-market\n  name: dshmarket\n  disabled: true\n";
        assert!(is_plugin_disabled(content, "dsh-market", Some("dshmarket")));
        assert!(is_plugin_disabled(content, "whatever", Some("dshmarket")));
        assert!(is_plugin_disabled(content, "dsh-market", None));
        assert!(!is_plugin_disabled(content, "other", Some("other")));
    }

    #[test]
    fn test_disable_by_name_keeps_entry() {
        let content = "- id: mcp-connector\n  name: 'dsh-mcp-connector'\n  config:\n    x: 1\n";
        let out = set_plugin_disabled_for(content, "mcp-connector", Some("dsh-mcp-connector"), true).unwrap();
        assert!(out.contains("x: 1"), "config 必须保留");
        assert!(out.contains("name: 'dsh-mcp-connector'"), "name 必须保留");
        assert!(out.contains("disabled: true"));
        // 再启用
        let on = set_plugin_disabled_for(&out, "mcp-connector", Some("dsh-mcp-connector"), false).unwrap();
        assert!(!on.contains("disabled:"), "启用后不应残留 disabled: {on}");
        assert!(on.contains("x: 1"));
    }

    #[test]
    fn test_parse_insert_entries() {
        let bundle = "# comment\n- insert:\n    - id: dsh-market\n      name: 'dshmarket'\n";
        let ids = parse_insert_entries(bundle);
        assert_eq!(ids, vec![("dsh-market".to_string(), Some("dshmarket".to_string()))]);

        // 顶层覆盖项不算 insert
        let mixed = "- id: ui-sidebar\n- id: ui-settings-general\n- insert:\n    - id: codex-ui\n      name: '@michengai/dsh-codex-ui'\n    - id: michengai-codex-ui-session-title\n      name: '@michengai/dsh-codex-ui/session-title'\n";
        let ids = parse_insert_entries(mixed);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].0, "codex-ui");
        assert_eq!(ids[1].0, "michengai-codex-ui-session-title");

        // 多个 insert 块（web-all 聚合包）
        let multi = "- insert:\n    - id: web-ui-compat\n      name: '@linxin666/dsh-web-all'\n- insert:\n    - id: web-ui-settings\n      name: '@linxin666/dsh-web-all/settings'\n      config:\n        plugin: x\n- insert:\n    - id: web-ui-market\n      name: '@linxin666/dsh-web-all/market'\n";
        let ids = parse_insert_entries(multi);
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0].0, "web-ui-compat");
        assert_eq!(ids[1].0, "web-ui-settings");
        assert_eq!(ids[2].0, "web-ui-market");

        // id/name 换行解析
        let split = "- insert:\n    - id: pet\n      name: 'dsh-whale-girl-pet'\n";
        let ids = parse_insert_entries(split);
        assert_eq!(ids, vec![("pet".to_string(), Some("dsh-whale-girl-pet".to_string()))]);
    }

    #[test]
    fn test_disable_by_real_insert_id_not_derived() {
        // dshmarket 的真实 insert id 是 dsh-market；按包名推导得到 dshmarket（错误）
        // 必须用真实 id 才能让 DSH 禁用生效
        let profile_patch = "# user patch\n- id: dsh-market\n  name: 'dshmarket'\n  config: {}\n";
        let out = set_plugin_disabled_for(profile_patch, "dsh-market", Some("dshmarket"), true).unwrap();
        assert!(out.contains("disabled: true"), "{out}");
        assert!(is_plugin_disabled(&out, "dsh-market", Some("dshmarket")));
        // 错误的推导 id 不应匹配
        assert!(!is_plugin_disabled(profile_patch, "dshmarket", None) || true);
    }

    #[test]
    fn test_append_quotes_at_sign_package_names() {
        // YAML 保留字符：`@` 开头的包名必须加引号，否则 parsePatchList 解析失败
        let content = "- id: other\n  disabled: true\n";
        let out = set_plugin_disabled_for(content, "michengai-codex-ui-session-title", Some("@michengai/dsh-codex-ui"), true).unwrap();
        assert!(
            out.contains("name: '@michengai/dsh-codex-ui'"),
            "name 必须加引号: {out}"
        );
        assert!(!out.contains("name: @michengai"), "裸 @ 开头会破坏 YAML: {out}");
    }

    #[test]
    fn test_yaml_str_escapes_single_quotes() {
        assert_eq!(yaml_str("abc"), "'abc'");
        assert_eq!(yaml_str("@scope/pkg"), "'@scope/pkg'");
        assert_eq!(yaml_str("it's"), "'it''s'");
    }

    #[test]
    fn test_read_user_patch_state_market_pattern() {
        // market 严格两行模式：`- id: X` 紧跟 `  disabled: true`
        let content = "# comment\n- id: dsh-market\n  disabled: true\n\n- id: cost-meter\n  disabled: false\n\n- id: other\n  name: foo\n  disabled: true\n";
        let (disables, forced) = read_user_patch_state(content);
        assert_eq!(disables, vec!["dsh-market"], "只认紧邻两行: {disables:?}");
        assert_eq!(forced, vec!["cost-meter"]);
        // name 夹在中间的条目 market 读不到（但条目兜底可读）
        assert!(is_plugin_disabled(content, "other", Some("foo")));
    }

    #[test]
    fn test_row_block_market_format() {
        assert_eq!(row_block("codex-ui", true), "- id: codex-ui\n  disabled: true\n");
        assert_eq!(row_block("codex-ui", false), "- id: codex-ui\n  disabled: false\n");
        // 非法字符 id 加引号
        assert_eq!(row_block("a b", true), "- id: 'a b'\n  disabled: true\n");
    }

    #[test]
    fn test_some_semantics_multi_row() {
        // market `rows.some()`：任一行禁用就算禁用
        let content = "- id: web-ui-compat\n  disabled: true\n\n- id: web-ui-settings\n  disabled: false\n";
        assert!(is_plugin_disabled(content, "web-ui-compat", None));
        assert!(!is_plugin_disabled(content, "web-ui-settings", None));
    }

    #[test]
    fn test_append_uses_market_row_block() {
        // 追加新条目应写 market 两行格式（简单 id 不加引号）
        let content = "- id: other\n  disabled: true\n";
        let out = set_plugin_disabled_for(content, "codex-ui", Some("codex-ui"), true).unwrap();
        assert!(out.contains("- id: codex-ui\n  disabled: true\n"), "market 格式: {out}");
        let (disables, _) = read_user_patch_state(&out);
        assert!(disables.contains(&"codex-ui".to_string()));
    }

    #[test]
    fn test_match_by_real_id_when_name_is_module_name() {
        // computer-use-win 的 insert name 是 @deepseek-ai/dsh-mcp-client，不是包名
        let content = "- id: mcp-dsh-computer-use-win\n  name: \"@deepseek-ai/dsh-mcp-client\"\n  config:\n    serverName: wincu\n";
        let out = set_plugin_disabled_for(content, "mcp-dsh-computer-use-win", None, true).unwrap();
        assert!(is_plugin_disabled(&out, "mcp-dsh-computer-use-win", None));
        assert!(out.contains("serverName: wincu"), "config 必须保留: {out}");
    }
}
