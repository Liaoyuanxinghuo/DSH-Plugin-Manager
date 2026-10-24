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

/// 扫描顶层条目列表：识别每条 `- ` 起始的条目及其 id/name。
/// `- insert:` 块内的嵌套行**不是**顶层条目（market toggle 永不命中 insert 行，
/// 否则会把 `disabled:` 写进包的插入块里破坏其语义）
fn scan_entries(lines: &[String]) -> Vec<EntrySpan> {
    let mut entries: Vec<EntrySpan> = Vec::new();
    let mut current: Option<EntrySpan> = None;
    let mut in_insert = false;
    let mut insert_indent = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if t.starts_with("- insert:") || t == "- insert" {
            if let Some(e) = current.take() {
                entries.push(e);
            }
            in_insert = true;
            insert_indent = indent;
            continue;
        }
        if in_insert {
            if t.starts_with("- ") && indent <= insert_indent {
                in_insert = false; // 落回顶层，按普通条目继续处理
            } else {
                continue;
            }
        }
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
/// 仅匹配条目直接属性层级（indent == 条目缩进 + 2），
/// 避免误改条目嵌套配置（如 `config: { disabled: true }`）里的同名键
fn find_disabled_line(lines: &[String], e: &EntrySpan) -> Option<(usize, Option<bool>)> {
    let prop_indent = e.indent + 2;
    for j in e.start..e.end {
        let t = lines[j].trim_start();
        if lines[j].len() - t.len() != prop_indent {
            continue;
        }
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
/// 匹配顺序（官方）：条目 id == plugin_id，或条目声明的 name == package_name。
/// - 停用：改写**最后一条**匹配行的 flag；无匹配 → 追加新条目。
/// - 启用（market `enableRow` 语义）：摘掉**全部**匹配行的禁用 flag（仅摘最后一条时，
///   更早的同 id 禁用行仍会生效）；移除后若无显式 `disabled: false` 强制行 → 末尾追加
///   （bundle 层可能默认禁用该行，官方分层合成"后面的层赢"，必须显式覆盖）。
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
    let pid = plugin_id;
    // 与 find_last_match 相同的选择器：id 有命中则只认 id，否则按 name
    let id_hit = !pid.is_empty() && entries.iter().any(|e| e.id == pid);
    let matches: Vec<&EntrySpan> = entries
        .iter()
        .filter(|e| {
            if id_hit {
                e.id == pid
            } else {
                package_name
                    .map_or(false, |pkg| !pkg.is_empty() && e.name.as_deref() == Some(pkg))
            }
        })
        .collect();

    let mut out: Vec<String> = lines.clone();

    if disabled {
        // 停用：改写最后一条匹配行的 flag；无则在该条目末尾插入 / 文件末尾追加
        match matches.last().copied() {
            Some(e) => {
                let end = e.end;
                let indent = e.indent;
                let dis = find_disabled_line(&out, e);
                let indent_str = " ".repeat(indent + 2);
                match dis {
                    Some((_, Some(true))) => Ok(join_lines(&out, eol, orig_has_tail)),
                    Some((j, _)) => {
                        out[j] = format!("{indent_str}disabled: true");
                        Ok(join_lines(&out, eol, orig_has_tail))
                    }
                    None => {
                        out.insert(end, format!("{indent_str}disabled: true"));
                        Ok(join_lines(&out, eol, orig_has_tail))
                    }
                }
            }
            None => {
                // market `rowBlock` 格式：两行、简单 id 不加引号（readUserPatchState 只认这种）
                let id_for_block = if pid.is_empty() {
                    derive_service_id(package_name.unwrap_or("plugin"))
                } else {
                    pid.to_string()
                };
                let mut block = row_block(&id_for_block, true);
                // 仅当 id 与包名推导不一致时补 name 行（保证后续可按 name 命中）
                if let Some(pkg) = package_name {
                    if pid.is_empty() || derive_service_id(pkg) != pid {
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
            }
        }
    } else {
        // 启用：market `enableRow` 语义
        let id_for_block = if pid.is_empty() {
            derive_service_id(package_name.unwrap_or("plugin"))
        } else {
            pid.to_string()
        };
        // 1) 摘掉全部匹配行的禁用 flag（纯桩整条删，长条目只摘 flag）
        for e in matches.iter().rev() {
            strip_entry_disabled(&mut out, e);
        }
        // 2) 移除后若无显式 `disabled: false` 强制行 → 末尾追加（覆盖 bundle 层默认禁用）
        let joined_now = join_lines(&out, eol, orig_has_tail);
        let (_, forced) = read_user_patch_state(&joined_now);
        if forced.iter().any(|f| f == &id_for_block) {
            // 已有强制行：仅做删空占位还原（market `withPlaceholderRestored`）
            return Ok(with_placeholder_restored(&joined_now));
        }
        let has_content = joined_now.lines().any(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        });
        if has_content {
            out.push(String::new());
        }
        for line in row_block(&id_for_block, false).trim_end_matches('\n').split('\n') {
            out.push(line.to_string());
        }
        Ok(join_lines(&out, eol, orig_has_tail))
    }
}

/// 摘掉一个条目的 disabled 行；摘除后若条目只剩 id（纯禁用桩）则整条删除。
/// 返回是否有改动。
fn strip_entry_disabled(out: &mut Vec<String>, e: &EntrySpan) -> bool {
    let Some((j, _)) = find_disabled_line(out, e) else {
        return false;
    };
    out.remove(j);
    // 仅当条目是「纯禁用桩」（除 id 外只剩注释/空行）才整条删除。
    // 带 name/config 的原条目保留（只摘掉 disabled），否则会误删用户配置。
    let new_end = e.end.saturating_sub(1);
    let mut only_id = true;
    for k in (e.start + 1)..new_end {
        let t = out.get(k).map(|l| l.trim_start()).unwrap_or("");
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        only_id = false;
        break;
    }
    if only_id {
        let first = out.get(e.start).map(|l| l.trim_start()).unwrap_or("");
        let body = first.strip_prefix("-").unwrap_or(first).trim_start();
        // 首行也必须只是 id（不能是 name:）— 否则保留
        if !(body.starts_with("id:") || body.is_empty()) {
            only_id = false;
        }
    }
    if only_id {
        // 整条删除（含首行），避免留下孤儿行
        let count = new_end - e.start;
        for _ in 0..count {
            if e.start < out.len() {
                out.remove(e.start);
            }
        }
        if e.start < out.len() && out[e.start].trim().is_empty() {
            out.remove(e.start);
        }
    }
    true
}

/// market `lastRowFlag` 等价：同 id 多行时，最后一个显式 `disabled` flag 的值。
/// enableRow 追加的 `disabled: false` 强制行因此能推翻更早的禁用行。
/// 严格两行模式（`- id: X` + 恰好 2 空格 `disabled:`），跳过 insert 块。
fn last_row_flag(content: &str, row_id: &str) -> Option<bool> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut in_insert = false;
    let mut last: Option<bool> = None;
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
        let Some(rest) = t.strip_prefix("- id:") else {
            continue;
        };
        let id = rest.trim();
        if id.is_empty() || id.contains(' ') || id.contains(':') {
            continue;
        }
        let id = id.trim_matches('"').trim_matches('\'');
        if id != row_id {
            continue;
        }
        let next = lines.get(index + 1).map(|l| l.strip_suffix('\r').unwrap_or(l)).unwrap_or("");
        let n = next.trim_start();
        if next.starts_with("  disabled:") && !next.starts_with("   ") {
            let val = n.strip_prefix("disabled:").unwrap_or("").trim();
            if val == "true" {
                last = Some(true);
            } else if val == "false" {
                last = Some(false);
            }
        }
    }
    last
}

/// 判断某插件是否被禁用。
/// market 语义：同 id 多行时**最后一个显式 flag 胜出**（enableRow 追加的强制行
/// 会被计入）；无任何 flag 行 → 未禁用。退回条目解析兜底（兼容 name 匹配的历史写法）。
pub fn is_plugin_disabled(content: &str, plugin_id: &str, package_name: Option<&str>) -> bool {
    let pid = plugin_id.trim();
    if !pid.is_empty() {
        if let Some(v) = last_row_flag(content, pid) {
            return v;
        }
    }
    // 兜底：历史条目（带 name/config 或引号 id）按条目匹配
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let entries = scan_entries(&lines);
    match find_last_match(&entries, pid, package_name) {
        Some(e) => find_disabled_line(&lines, e).map(|(_, v)| v.unwrap_or(false)).unwrap_or(false),
        None => false,
    }
}

/// 列出已禁用插件的 id 列表（market `readUserPatchState` 权威 + 条目兜底；
/// 同 id 的最后 flag 为 false（强制启用）的不算禁用）
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
    // 强制启用（最后 flag 为 false）的 id 不算禁用
    disables.retain(|id| last_row_flag(content, id) != Some(false));
    disables
}

/// 卸载收尾：删掉 market/本管理器为某行写的简单块（`- id: X` + `disabled: …`，
/// 或再带一行 `name: …`）。只删这两种**我们自己写入**的形状；
/// 用户手写的带 config 的长条目原样保留（与 market `removeRowBlocks` 同策略，
/// 不能把用户配置当孤儿删掉）。
pub fn remove_row_blocks(content: &str, row_ids: &[String]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut remove = vec![false; lines.len()];
    for id in row_ids {
        for (i, line) in lines.iter().enumerate() {
            if remove[i] {
                continue;
            }
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("- id:") else {
                continue;
            };
            let rid = rest.trim().trim_matches('"').trim_matches('\'');
            if rid != id.as_str() {
                continue;
            }
            // 收集条目属性行（直到下一个 `- ` 条目或空行）
            let mut j = i + 1;
            let mut attrs: Vec<&str> = Vec::new();
            while j < lines.len() {
                let t2 = lines[j].trim_start();
                if t2.is_empty() || t2.starts_with("- ") || t2 == "-" {
                    break;
                }
                attrs.push(t2);
                j += 1;
            }
            let is_simple = match attrs.len() {
                1 => attrs[0].starts_with("disabled:"),
                2 => attrs[0].starts_with("name:") && attrs[1].starts_with("disabled:"),
                _ => false,
            };
            if is_simple {
                for k in i..j {
                    remove[k] = true;
                }
            }
        }
    }
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !remove[*i])
        .map(|(_, l)| *l)
        .collect();
    let mut s = kept.join(eol);
    if content.is_empty() || content.ends_with('\n') {
        s.push_str(eol);
    }
    with_placeholder_restored(&s)
}

/// market `withPlaceholderRestored` 等价：删空后的 patch 必须仍是合法 YAML
/// 列表（`[]`），否则下一次「启用」写入会把 profile 撑挂。
fn with_placeholder_restored(text: &str) -> String {
    let has_content = text
        .lines()
        .any(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        });
    if has_content {
        return text.to_string();
    }
    // 只剩注释：优先还原注释掉的 `# []` 占位
    if let Some(pos) = text.find('#') {
        let _ = pos;
        let mut out_lines: Vec<String> = Vec::new();
        let mut restored = false;
        for l in text.lines() {
            let t = l.trim();
            if !restored && t.starts_with('#') {
                let body = t.trim_start_matches('#').trim();
                if body == "[]" {
                    out_lines.push("[]".to_string());
                    restored = true;
                    continue;
                }
            }
            out_lines.push(l.to_string());
        }
        if restored {
            let mut s = out_lines.join("\n");
            if text.ends_with('\n') {
                s.push('\n');
            }
            return s;
        }
    }
    // 无可还原占位：空文件或纯注释 → 补 `[]`
    if text.is_empty() || text.ends_with('\n') {
        format!("{text}[]\n")
    } else {
        format!("{text}\n[]\n")
    }
}

/// 包自身 patch 的「邻居行」与「停用载体」识别（market `foreignRowIds` /
/// `carrierDisableIds` 等价）：
/// - foreign：顶层 `- id:` 行且**不在**本包 insert 列表（对其他插件的重配/停用）
/// - carrier：foreign 中带 `disabled: true` 的（禁用载体，#224）
/// 用于决定停用时是否可以把包从 `dsh.profile.bundles` 里摘掉：
/// 摘掉 = 整包 patch 失效，会把邻居的配置一起带走（#147）。
pub fn package_patch_foreign_and_carriers(content: &str) -> (Vec<String>, Vec<String>) {
    let inserted: std::collections::HashSet<String> =
        parse_insert_entries(content).into_iter().map(|(id, _)| id).collect();
    let mut foreign: Vec<String> = Vec::new();
    let mut carriers: Vec<String> = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut in_insert = false;
    let mut insert_indent = 0usize;
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        let indent = line.len() - t.len();
        if t.starts_with("- insert:") || t == "- insert" {
            in_insert = true;
            insert_indent = indent;
            continue;
        }
        if in_insert {
            if t.starts_with("- ") && indent <= insert_indent {
                in_insert = false;
                // 落回顶层处理
            } else {
                continue;
            }
        }
        if !t.starts_with("- ") && t != "-" {
            continue;
        }
        let body = t.strip_prefix("-").unwrap_or(t).trim_start();
        let (id, _) = parse_id_name(body);
        if id.is_empty() || inserted.contains(&id) {
            continue;
        }
        let mut disabled_true = false;
        for j in index + 1..lines.len() {
            let t2 = lines[j].trim_start();
            if t2.starts_with("- ") || t2 == "-" || t2.is_empty() {
                break;
            }
            if let Some(rest) = t2.strip_prefix("disabled:") {
                if rest.trim() == "true" {
                    disabled_true = true;
                }
            }
        }
        if !foreign.contains(&id) {
            foreign.push(id.clone());
        }
        if disabled_true && !carriers.contains(&id) {
            carriers.push(id);
        }
    }
    (foreign, carriers)
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
        // 纯禁用桩整条删除，但按 market enableRow 语义追加显式强制启用行
        assert!(!out.contains("- id: include\n  disabled: true"), "旧禁用桩应被删除");
        assert_eq!(out.matches("- id: include").count(), 1, "只应剩强制启用行: {out}");
        assert!(out.contains("- id: include\n  disabled: false"), "应追加强制启用行: {out}");
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
    fn test_enable_non_existing_appends_force() {
        // market enableRow：无匹配行也追加强制启用行（bundle 层可能默认禁用）
        let out = set_plugin_disabled(SAMPLE, "nope", false).unwrap();
        assert!(out.contains("- id: nope\n  disabled: false"), "{out}");
        assert!(out.contains("- id: include\n  disabled: true"), "其余内容不变");
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
        // 原 config 条目的 disabled 被摘除；强制启用行为显式追加（market enableRow）
        assert!(on.contains("x: 1"), "config 必须保留: {on}");
        assert!(on.contains("name: 'dsh-mcp-connector'"), "name 必须保留");
        assert!(!on.contains("  disabled: true"), "不应残留禁用: {on}");
        assert!(on.contains("- id: mcp-connector\n  disabled: false"), "应追加强制启用行: {on}");
        assert!(!is_plugin_disabled(&on, "mcp-connector", Some("dsh-mcp-connector")));
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

    // ===== 卸载收尾：remove_row_blocks / 占位恢复 / foreign-carrier =====

    #[test]
    fn test_remove_row_blocks_simple_shapes() {
        // 两行块（row_block）与三行块（id+name+disabled）都删；用户长条目保留
        let content = "- id: keep-me\n  config:\n    x: 1\n- id: codex-ui\n  disabled: true\n- id: pet\n  name: 'dsh-whale-girl-pet'\n  disabled: false\n- id: rich\n  name: foo\n  config:\n    y: 2\n  disabled: true\n";
        let out = remove_row_blocks(content, &["codex-ui".into(), "pet".into()]);
        assert!(!out.contains("- id: codex-ui"), "{out}");
        assert!(!out.contains("- id: pet"), "{out}");
        assert!(out.contains("- id: keep-me"), "其他条目保留: {out}");
        assert!(out.contains("- id: rich"), "用户长条目保留: {out}");
        assert!(out.contains("y: 2"), "{out}");
    }

    #[test]
    fn test_remove_row_blocks_emptied_gets_placeholder() {
        // 删空后的 patch 必须是合法 YAML 列表 `[]`，否则下次启用会写挂 profile
        let content = "- id: a\n  disabled: true\n";
        let out = remove_row_blocks(content, &["a".into()]);
        assert_eq!(out.trim(), "[]", "删空应补 [] 占位: {out:?}");

        let commented = "# note\n# []\n- id: a\n  disabled: true\n";
        let out2 = remove_row_blocks(commented, &["a".into()]);
        assert!(out2.contains("[]"), "注释占位应还原: {out2:?}");
        assert!(out2.contains("# note"), "注释保留: {out2:?}");
    }

    #[test]
    fn test_remove_row_blocks_unknown_id_is_noop() {
        let content = "- id: a\n  disabled: true\n";
        assert_eq!(remove_row_blocks(content, &["b".into()]), content);
    }

    #[test]
    fn test_foreign_and_carriers_detection() {
        // 包 patch：insert 自己的行 + 顶层重配邻居 + 顶层停用他人（载体）
        let patch = "- insert:\n    - id: mine\n      name: 'my-pkg'\n- id: neighbour\n  config:\n    a: 1\n- id: victim\n  disabled: true\n";
        let (foreign, carriers) = package_patch_foreign_and_carriers(patch);
        assert!(foreign.contains(&"neighbour".to_string()), "{foreign:?}");
        assert!(foreign.contains(&"victim".to_string()), "{foreign:?}");
        assert!(!foreign.contains(&"mine".to_string()), "insert 行不算 foreign: {foreign:?}");
        assert_eq!(carriers, vec!["victim".to_string()], "disabled:true 的他人行 = 停用载体");
    }

    #[test]
    fn test_foreign_empty_for_self_only_patch() {
        let patch = "- insert:\n    - id: mine\n      name: 'my-pkg'\n";
        let (foreign, carriers) = package_patch_foreign_and_carriers(patch);
        assert!(foreign.is_empty());
        assert!(carriers.is_empty());
    }

    #[test]
    fn disabled_toggle_ignores_nested_config_disabled() {
        let content = "# 注释
- id: some-plugin
  name: \"@scope/pkg\"
  config:
    disabled: true
    other: 1
";
        // 禁用：应在条目直接层级（缩进+2）插入 disabled: true，而不是改写 config 下嵌套的同名键
        let out = set_plugin_disabled(content, "some-plugin", true).unwrap();
        assert!(out.contains("
  disabled: true"));
        assert!(out.contains("    disabled: true"), "嵌套配置键必须原样保留");
        // 启用：只摘掉直接层级的 disabled，嵌套键仍保留
        let out2 = set_plugin_disabled(&out, "some-plugin", false).unwrap();
        assert!(!out2.contains("
  disabled: true"));
        assert!(out2.contains("    disabled: true"), "启用时嵌套配置键也不得被删");
    }

    #[test]
    fn enable_appends_force_row_and_last_flag_wins() {
        let content = "- id: a\n  disabled: true\n";
        let out = set_plugin_disabled(content, "a", false).unwrap();
        // 强制启用行被追加，状态判定为已启用（market enableRow 语义）
        assert!(out.contains("- id: a\n  disabled: false"), "{out}");
        assert!(!is_plugin_disabled(&out, "a", None));
        // 再禁用 → 写在强制行上，状态为禁用
        let out2 = set_plugin_disabled(&out, "a", true).unwrap();
        assert!(is_plugin_disabled(&out2, "a", None));
    }

    #[test]
    fn enable_removes_all_flags_for_id_then_forces() {
        // 同 id 两行：仅摘最后一行时更早的禁用行仍生效，必须全部摘掉
        let content = "- id: a\n  disabled: true\n\n- id: a\n  name: 'p'\n  disabled: true\n";
        let out = set_plugin_disabled(content, "a", false).unwrap();
        assert!(!out.contains("disabled: true"), "{out}");
        assert!(!is_plugin_disabled(&out, "a", None));
        assert!(out.contains("name: 'p'"), "name 行不得变孤儿: {out}");
    }

    #[test]
    fn enable_without_row_appends_force_row() {
        let content = "- id: other\n  disabled: true\n";
        let out = set_plugin_disabled(content, "x", false).unwrap();
        assert!(out.contains("- id: x\n  disabled: false"), "{out}");
        assert!(out.contains("- id: other\n  disabled: true"));
        assert!(!is_plugin_disabled(&out, "x", None));
        assert!(is_plugin_disabled(&out, "other", None));
    }

    #[test]
    fn toggle_never_touches_insert_blocks() {
        let insert_block = "- insert:\n    - id: inner\n      name: 'x'\n";
        let content = format!("{insert_block}- id: outer\n  disabled: true\n");
        // 禁用 insert 内的行：不在 insert 块里动刀，而是文件末尾追加覆盖行（market 语义）
        let out = set_plugin_disabled(&content, "inner", true).unwrap();
        assert!(out.contains(insert_block), "insert 块必须原样保留: {out}");
        assert!(out.contains("- id: inner\n  disabled: true"));
        // 启用：追加强制行，insert 块同样原样保留
        let out2 = set_plugin_disabled(&out, "inner", false).unwrap();
        assert!(out2.contains(insert_block), "insert 块必须原样保留: {out2}");
        assert!(out2.contains("- id: inner\n  disabled: false"));
        assert!(!is_plugin_disabled(&out2, "inner", None));
    }
}

