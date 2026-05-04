// Extracted parser helpers for tool call parsing.

/// Split by whitespace but keep double-quoted segments as a single token (e.g. -H "Authorization: Bearer $X" -> [-H, "Authorization: Bearer $X"]).
pub(crate) fn split_whitespace_respecting_quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if rest.starts_with('"') {
            let mut end = 1usize;
            while end < rest.len() {
                let b = rest.as_bytes()[end];
                if b == b'\\' && end + 1 < rest.len() {
                    end += 2;
                    continue;
                }
                if b == b'"' {
                    out.push(rest[1..end].replace("\\\"", "\""));
                    rest = &rest[end + 1..];
                    break;
                }
                end += 1;
            }
            if end >= rest.len() {
                // Unclosed quote: slice only if rest has content after the opening quote (avoid rest[1..] when len==1)
                let quoted = if rest.len() > 1 { &rest[1..] } else { "" };
                out.push(quoted.replace("\\\"", "\""));
                rest = "";
            }
        } else {
            let next_quote = rest.find('"').unwrap_or(rest.len());
            let word_end = rest[..next_quote]
                .find(|c: char| c.is_whitespace())
                .unwrap_or(rest.len());
            let word = rest[..word_end].trim();
            if !word.is_empty() {
                out.push(word.to_string());
            }
            rest = &rest[word_end..];
        }
    }
    out
}

/// Strip `- ` / `* ` / `1. ` list prefixes so tool lines can be detected.
pub(crate) fn strip_optional_list_prefix(line: &str) -> &str {
    let mut s = line.trim_start();
    for p in ["- ", "* ", "+ ", "• "] {
        if let Some(r) = s.strip_prefix(p) {
            s = r.trim_start();
            break;
        }
    }
    if s.is_empty() {
        return s;
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        if let Some(dot) = s.find('.') {
            if dot > 0 && s[..dot].chars().all(|c| c.is_ascii_digit()) {
                return s[dot + 1..].trim_start();
            }
        }
    }
    s
}

/// Strip ATX heading hashes (`### Title` → `Title`).
pub(crate) fn strip_markdown_heading_hashes(line: &str) -> &str {
    let mut s = line.trim_start();
    let mut n = 0usize;
    while s.starts_with('#') && n < 7 {
        n += 1;
        s = &s[1..];
    }
    if n > 0 {
        s = s.trim_start();
    }
    s
}

/// Leading markdown noise before `TOOL` (table pipes, headings, lists, bold, backticks).
pub(crate) fn strip_leading_tool_line_noise(line: &str) -> &str {
    let mut s = line.trim_start();
    while s.starts_with('|') {
        s = s[1..].trim_start();
    }
    s = strip_markdown_heading_hashes(s);
    s = strip_optional_list_prefix(s);
    s = s
        .trim_start_matches(|c: char| matches!(c, '*' | '`'))
        .trim_start();
    s
}

/// True if `prefix` is only whitespace and common markdown punctuation (no letters/words — avoids matching prose before `TOOL:`).
pub(crate) fn tool_line_prefix_is_markdown_junk_only(prefix: &str) -> bool {
    prefix.trim().chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '#' | '*'
                    | '`'
                    | '|'
                    | '•'
                    | '-'
                    | '+'
                    | ':'
                    | '.'
                    | ';'
                    | ','
                    | '/'
                    | '\\'
                    | '('
                    | ')'
                    | '['
                    | ']'
            )
            || c.is_ascii_digit()
    })
}

/// First word after `TOOL:` must match a real tool when using the "inline" heuristic (avoids prose `… TOOL: …`).
const ORCH_INLINE_TOOL_FIRST_WORDS: &[&str] = &[
    "write_file",
    "read_file",
    "edit_file",
    "search_replace",
    "apply_patch",
    "list_dir",
    "grep_content",
    "web_search",
    "web_fetch",
    "web_crawl",
    "web_crawl_status",
    "run_command",
    "browser",
    "install_playwright",
    "read_skill",
    "memory_store",
    "schedule_task",
    "list_scheduled_tasks",
    "cancel_scheduled_task",
    "budget_status",
    "workspace_graph_search",
    "device_invoke",
    "ask_user",
    "generate_image",
    "pdf",
];

/// `### Step — TOOL: write_file`, table junk, or other lines where `TOOL:` is not at column 0 after strips.
pub(crate) fn inline_ascii_tool_colon_rest(line: &str) -> Option<&str> {
    let t = line.trim();
    let mut search = t;
    let mut last_ok: Option<&str> = None;
    while let Some(pos) = search.find("TOOL:") {
        let abs = t.len() - search.len() + pos;
        let prefix = &t[..abs];
        let rest = t[abs + 5..].trim_start();
        if let Some(tok) = rest.split_whitespace().next() {
            let tl = tok.to_lowercase();
            if ORCH_INLINE_TOOL_FIRST_WORDS
                .iter()
                .any(|&n| n == tl.as_str())
            {
                if tool_line_prefix_is_markdown_junk_only(prefix) {
                    last_ok = Some(rest);
                }
            }
        }
        search = &t[abs + 5..];
    }
    last_ok
}

/// If the line begins with `tool` (case-insensitive) as a keyword followed by optional `*` / `` ` `` and `:`, return the rest.
/// Handles models that emit `- Tool:`, `**TOOL:**`, table cells `| TOOL: ... |`, etc., when substring `TOOL:` is present but strict parse failed.
pub(crate) fn line_rest_after_leading_tool(line: &str) -> Option<&str> {
    line_rest_after_leading_tool_at_start(line).or_else(|| inline_ascii_tool_colon_rest(line))
}

pub(crate) fn line_rest_after_leading_tool_at_start(line: &str) -> Option<&str> {
    let s = strip_leading_tool_line_noise(line);
    if s.len() < 4 {
        return None;
    }
    if !s.get(0..4)?.eq_ignore_ascii_case("tool") {
        return None;
    }
    // Reject `tools:` / `toolkit:` — fifth char must not be ASCII letter.
    if s.as_bytes().get(4).is_some_and(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut rest = &s[4..];
    rest = rest.trim_start();
    while rest.starts_with('*') || rest.starts_with('`') {
        rest = &rest[1..];
    }
    rest = rest.strip_prefix(':')?;
    rest = rest.trim_start();
    while rest.starts_with('*') || rest.starts_with('`') {
        rest = &rest[1..];
    }
    // Drop trailing table pipe from first cell
    let rest = rest.trim_end();
    let rest = rest.strip_suffix('|').map(|x| x.trim_end()).unwrap_or(rest);
    Some(rest.trim_start())
}

/// Returns true if `tool_name` (already lower-cased) takes a multiline body argument.
pub(crate) fn tool_supports_multiline_body(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "apply_patch" | "edit_file" | "write_file" | "ask_user" | "search_replace"
    )
}

pub(crate) fn tool_name_is_safe_identifier(tool_name: &str) -> bool {
    !tool_name.is_empty()
        && tool_name.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
}

/// DeepSeek / variantes : balises style `<｜DSML｜tool_calls>…<｜DSML｜invoke name="…">…</｜DSML｜invoke>`.
/// Le découpage sur espaces ne s’applique pas au XML ; on convertit en lignes `TOOL:` avant `parse_tool_calls`.
pub(crate) fn normalize_dsml_tool_calls(response: &str) -> String {
    let lower = response.to_ascii_lowercase();
    if !lower.contains("dsml") || !lower.contains("invoke") || !response.contains('<') {
        return response.to_string();
    }
    static DSML_TOOL_CALLS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static DSML_INVOKE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static DSML_PARAM: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tc = DSML_TOOL_CALLS.get_or_init(|| {
        regex::Regex::new(
            r"(?is)<[^>]{0,160}?\bdsml[^>]{0,160}?\btool_calls[^>]*>([\s\S]*?)</[^>]{0,160}?\bdsml[^>]{0,160}?\btool_calls[^>]*>",
        )
        .expect("dsml tool_calls regex")
    });
    let inv = DSML_INVOKE.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)<[^>]{0,160}?\binvoke\b[^>]*\bname\s*=\s*["']([^"']+)["'][^>]*>([\s\S]*?)</[^>]{0,160}?\binvoke[^>]*>"#,
        )
        .expect("dsml invoke regex")
    });
    let par = DSML_PARAM.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)<[^>]{0,160}?\bparameter\b[^>]*\bname\s*=\s*["']([^"']+)["'][^>]*>\s*(.*?)\s*</[^>]{0,160}?\bparameter[^>]*>"#,
        )
        .expect("dsml parameter regex")
    });

    let mut s = tc
        .replace_all(response, |caps: &regex::Captures| {
            let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let lines = dsml_fragment_invokes_to_tool_lines(inner, inv, par);
            if lines.is_empty() {
                return caps.get(0).map(|m| m.as_str().to_string()).unwrap_or_default();
            }
            lines.join("\n")
        })
        .into_owned();

    // Invokes restants (hors bloc tool_calls, ou variante sans wrapper)
    static DSML_STANDALONE_INVOKE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let sinv = DSML_STANDALONE_INVOKE.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)<[^>]{0,160}?\bdsml[^>]{0,160}?\binvoke\b[^>]*\bname\s*=\s*["']([^"']+)["'][^>]*>([\s\S]*?)</[^>]{0,160}?\bdsml[^>]{0,160}?\binvoke[^>]*>"#,
        )
        .expect("dsml standalone invoke regex")
    });
    s = sinv
        .replace_all(&s, |caps: &regex::Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
            let body = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let params = dsml_parse_parameters(body, par);
            let args = dsml_params_to_tool_args(name, &params);
            format!("TOOL: {} {}", name, args.join(" "))
        })
        .into_owned();

    s
}

pub(crate) fn dsml_simple_entity_decode(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

pub(crate) fn dsml_truthy_param(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | ""
    )
}

pub(crate) fn dsml_parse_parameters(body: &str, par: &regex::Regex) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in par.captures_iter(body) {
        let name = c.get(1).map(|m| m.as_str()).unwrap_or("").trim().to_string();
        let val = dsml_simple_entity_decode(c.get(2).map(|m| m.as_str()).unwrap_or("").trim());
        if !name.is_empty() {
            out.push((name, val));
        }
    }
    out
}

pub(crate) fn dsml_params_to_tool_args(tool: &str, params: &[(String, String)]) -> Vec<String> {
    let tl = tool.trim().to_ascii_lowercase();
    match tl.as_str() {
        "search_files" => {
            let mut dir = ".".to_string();
            let mut pat = "*".to_string();
            let mut no_ignore = false;
            for (k, v) in params {
                let kl = k.to_ascii_lowercase();
                match kl.as_str() {
                    "dir" => {
                        if !v.is_empty() {
                            dir = v.clone();
                        }
                    }
                    "pattern" | "glob" => {
                        if !v.is_empty() {
                            pat = v.clone();
                        }
                    }
                    "--no-ignore" | "--no-gitignore" | "no_ignore" => {
                        if dsml_truthy_param(v) {
                            no_ignore = true;
                        }
                    }
                    "limit" | "offset" | "max" | "max_results" | "top_k" | "string" => {}
                    _ => {}
                }
            }
            let mut out = vec![dir, pat];
            if no_ignore {
                out.push("--no-ignore".to_string());
            }
            out
        }
        "grep_content" => {
            let mut dir = ".".to_string();
            let mut pattern = String::new();
            let mut file_glob: Option<String> = None;
            let mut no_ignore = false;
            let mut use_regex = false;
            for (k, v) in params {
                let kl = k.to_ascii_lowercase();
                match kl.as_str() {
                    "dir" => {
                        if !v.is_empty() {
                            dir = v.clone();
                        }
                    }
                    "pattern" | "query" => pattern = v.clone(),
                    "file_glob" | "glob" => {
                        if !v.is_empty() {
                            file_glob = Some(v.clone());
                        }
                    }
                    "--no-ignore" | "--no-gitignore" | "no_ignore" => {
                        if dsml_truthy_param(v) {
                            no_ignore = true;
                        }
                    }
                    "--regex" | "-r" | "regex" => {
                        if dsml_truthy_param(v) {
                            use_regex = true;
                        }
                    }
                    "limit" | "string" | "offset" => {}
                    _ => {}
                }
            }
            let mut out = vec![dir, pattern];
            if let Some(g) = file_glob {
                if !g.is_empty() {
                    out.push(g);
                }
            }
            if use_regex {
                out.push("--regex".to_string());
            }
            if no_ignore {
                out.push("--no-ignore".to_string());
            }
            out
        }
        _ => dsml_generic_params_to_args(params),
    }
}

pub(crate) fn dsml_generic_params_to_args(params: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in params {
        let kt = k.trim();
        if kt.eq_ignore_ascii_case("string") {
            continue;
        }
        if kt.starts_with("--") {
            if dsml_truthy_param(v) {
                out.push(kt.to_string());
            }
        } else if !v.is_empty() {
            out.push(v.clone());
        }
    }
    out
}

pub(crate) fn dsml_fragment_invokes_to_tool_lines(
    inner: &str,
    inv: &regex::Regex,
    par: &regex::Regex,
) -> Vec<String> {
    let mut lines = Vec::new();
    for c in inv.captures_iter(inner) {
        let name = c.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let body = c.get(2).map(|m| m.as_str()).unwrap_or("");
        let params = dsml_parse_parameters(body, par);
        let args = dsml_params_to_tool_args(name, &params);
        lines.push(format!("TOOL: {} {}", name, args.join(" ")));
    }
    lines
}

/// Variante LongCat / pseudo-XML (`<longcat_tool_call>read_file …</longcat_tool_call>`) utilisée par certains
/// modèles (ex. via OpenRouter). Sans normalisation, aucune ligne `TOOL:` n’est détectée et l’orchestrateur
/// termine sans exécution d’outils — ou boucle sur des `read_file` mal dédupliqués.
pub(crate) fn normalize_longcat_tool_calls(response: &str) -> String {
    let lower = response.to_ascii_lowercase();
    if !lower.contains("longcat_tool_call") {
        return response.to_string();
    }
    static LONGCAT_BLOCK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LONGCAT_KV_PAIRED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LONGCAT_KV_SHORTHAND: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let block_re = LONGCAT_BLOCK.get_or_init(|| {
        regex::Regex::new(
            r"(?is)<longcat_tool_call[^>]*>\s*([\s\S]*?)\s*</longcat_tool_call\s*>",
        )
        .expect("longcat block regex")
    });
    // Full pairing: `<longcat_arg_key>path</longcat_arg_key><longcat_arg_value>…`
    let kv_paired = LONGCAT_KV_PAIRED.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)<longcat_arg_key>\s*([^<]+?)\s*</longcat_arg_key>\s*<longcat_arg_value>\s*([\s\S]*?)\s*</longcat_arg_value>"#,
        )
        .expect("longcat arg kv paired regex")
    });
    // Shorthand (OpenRouter / owl-alpha): `<longcat_arg_key>path <longcat_arg_value>…` sans `</longcat_arg_key>`,
    // et souvent sans `</longcat_arg_value>` avant `</longcat_tool_call>`.
    let kv_shorthand = LONGCAT_KV_SHORTHAND.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)<longcat_arg_key>\s*(\S+)\s*<longcat_arg_value>\s*(.*)\s*\z"#,
        )
        .expect("longcat arg kv shorthand regex")
    });

    block_re
        .replace_all(response, |caps: &regex::Captures| {
            let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
            longcat_inner_to_tool_line(inner, kv_paired, kv_shorthand).unwrap_or_else(|| {
                caps.get(0).map(|m| m.as_str().to_string()).unwrap_or_default()
            })
        })
        .into_owned()
}

fn longcat_collect_kv_pairs(
    inner: &str,
    kv_paired: &regex::Regex,
    kv_shorthand: &regex::Regex,
) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    for c in kv_paired.captures_iter(inner) {
        let k = c
            .get(1)
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty());
        let v = c
            .get(2)
            .map(|m| dsml_simple_entity_decode(m.as_str().trim()));
        if let (Some(k), Some(v)) = (k, v) {
            params.push((k, v));
        }
    }
    if params.is_empty() {
        for c in kv_shorthand.captures_iter(inner) {
            let k = c
                .get(1)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty());
            let v_raw = c.get(2).map(|m| m.as_str().trim());
            let v = v_raw.map(|s| {
                let t = if let Some(i) = s.find("</longcat_arg_value>") {
                    s[..i].trim()
                } else {
                    s
                };
                dsml_simple_entity_decode(t)
            });
            if let (Some(k), Some(v)) = (k, v) {
                params.push((k, v));
            }
        }
    }
    params
}

fn longcat_inner_to_tool_line(
    inner: &str,
    kv_paired: &regex::Regex,
    kv_shorthand: &regex::Regex,
) -> Option<String> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    // Tool name is the first token when present before any tag (usual LongCat shape).
    let (tool_name, rest_for_kv) = if inner.starts_with('<') {
        return None;
    } else {
        let first_end = inner
            .find(|c: char| c.is_whitespace() || c == '<')
            .unwrap_or(inner.len());
        let tool = inner[..first_end].trim();
        if tool.is_empty() || !tool_name_is_safe_identifier(&tool.to_lowercase()) {
            return None;
        }
        let rest = inner[first_end..].trim_start();
        (tool, rest)
    };

    let mut params = longcat_collect_kv_pairs(rest_for_kv, kv_paired, kv_shorthand);
    if params.is_empty() {
        params = longcat_collect_kv_pairs(inner, kv_paired, kv_shorthand);
    }
    if params.is_empty() {
        return None;
    }

    let args = dsml_params_to_tool_args(tool_name, &params);
    // Leading/trailing newlines so `TOOL:` is on its own line even when LongCat is glued to prose.
    Some(format!(
        "\nTOOL: {} {}\n",
        tool_name,
        args.join(" ")
    ))
}

/// Découpe une ligne `… prose … TOOL: … TOOL: …` en segments (sans regex look-around : non supporté par le moteur `regex`).
fn split_inline_tool_segments(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut s = line;
    loop {
        let pos_space = s.find(" TOOL:");
        let pos_tab = s.find("\tTOOL:");
        let pos = match (pos_space, pos_tab) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        match pos {
            Some(p) => {
                let head = s[..p].trim_end();
                if !head.is_empty() {
                    out.push(head.to_string());
                }
                s = s.get(p + 1..).unwrap_or("");
            }
            None => {
                let t = s.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                break;
            }
        }
    }
    out
}

/// Modèles (p.ex. Kimi) qui mettent la sortie outil sur la **même ligne** que la prose :
/// `…scaffold. TOOL: search_files … TOOL: read_file …` — `parse_tool_calls_strict` ne voit
/// qu’une seule « ligne » qui ne commence pas par `TOOL:`, donc zéro exécution d’outils.
/// Découpe en **une ligne par `TOOL:`** (hors blocs de code ```).
pub(crate) fn normalize_inline_adjacent_tool_calls(response: &str) -> String {
    if !response.contains("TOOL:") {
        return response.to_string();
    }
    let mut in_fence = false;
    let mut out_lines: Vec<String> = Vec::new();
    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out_lines.push(line.to_string());
            continue;
        }
        if in_fence {
            out_lines.push(line.to_string());
            continue;
        }
        for part in split_inline_tool_segments(line) {
            out_lines.push(part);
        }
    }
    out_lines.join("\n")
}

/// Retire les blocs `<think>…</think>` (modèles « reasoning ») pour que
/// le parseur d’outils et l’UI ne voient pas la chaîne de pensée comme du corps de réponse.
pub(crate) fn strip_redacted_reasoning_blocks(response: &str) -> String {
    static REDACTED_BLOCK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = REDACTED_BLOCK.get_or_init(|| {
        regex::Regex::new(r"(?is)<think>.*?</think>")
            .expect("redacted_thinking block regex")
    });
    let mut s = re.replace_all(response, "").to_string();
    if s.contains("<think>") {
        static OPEN_ONLY: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let open = OPEN_ONLY.get_or_init(|| {
            regex::Regex::new(r"(?is)<think>").expect("redacted open regex")
        });
        s = open.replace_all(&s, "").to_string();
    }
    // Fermetures orphelines après découpe ou flux incomplet.
    s.replace("</think>", "")
}

/// Certains modèles enveloppent les appels outils en XML (`<tool_call>read_file …</tool_call>`)
/// ou enchaînent avec `<tool_call>…<tool_call>…` sans fermeture. Convertir en lignes `TOOL:` pour `parse_tool_calls`.
pub(crate) fn normalize_xml_tool_call_wrappers(response: &str) -> String {
    static CLOSE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static OPEN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    let lower = response.to_ascii_lowercase();
    if !lower.contains("<tool_call") {
        return response.to_string();
    }
    let close_re = CLOSE_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)</tool_call\s*>").expect("hard-coded close tool_call regex must compile")
    });
    let s = close_re.replace_all(response, "\n").to_string();
    let open_re = OPEN_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)<tool_call(?:\s[^>]*)?>\s*")
            .expect("hard-coded open tool_call regex must compile")
    });
    open_re.replace_all(&s, "\nTOOL: ").to_string()
}

/// Rewrite lines so strict `TOOL:` prefix parsing succeeds (see `parse_tool_calls`).
///
/// Lines that are inside the body of a multiline tool (`write_file`, `edit_file`,
/// `apply_patch`, `ask_user`) are **not** normalized — only a real tool header
/// (canonical `TOOL: …` or a sloppy markdown-style prefix recognized by
/// `line_rest_after_leading_tool`, e.g. `- Tool: …` or `**TOOL:** …`) can
/// terminate a body.  Lines that merely *mention* a tool mid-sentence
/// (e.g. `### Step — TOOL: read_file`) remain body content because
/// `line_rest_after_leading_tool` requires the keyword to appear at the effective
/// start of the line after stripping markdown noise.
pub(crate) fn normalize_response_tool_prefixes(response: &str) -> String {
    let mut in_fence = false;
    let mut in_multiline_body = false;
    let mut out_lines = Vec::new();

    for raw in response.lines() {
        let trimmed = raw.trim();

        // Track fenced code blocks (``` or ```lang) — never normalize inside them.
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out_lines.push(raw.to_string());
            continue;
        }

        if in_fence {
            out_lines.push(raw.to_string());
            continue;
        }

        // Inside a multiline body, exit body mode only when the line is a real
        // tool header — either the canonical "TOOL: …" form or a sloppy
        // markdown-style prefix that `line_rest_after_leading_tool` recognises
        // (e.g. "- Tool: …", "**TOOL:** …").  Lines where TOOL: is embedded
        // after non-noise text (e.g. "### Step — TOOL: read_file") are kept as
        // body content because `line_rest_after_leading_tool` requires the
        // keyword at the effective start of the line.
        if in_multiline_body {
            if raw.trim_start().starts_with("TOOL:") || line_rest_after_leading_tool(raw).is_some()
            {
                // Real tool header (canonical or sloppy) — exit body mode and fall through.
                in_multiline_body = false;
            } else {
                out_lines.push(raw.to_string());
                continue;
            }
        }

        if let Some(rest) = line_rest_after_leading_tool(raw) {
            let normalized = format!("TOOL: {}", rest);
            // If this tool supports a multiline body, subsequent lines are body content.
            if let Some(name) = rest.split_whitespace().next() {
                if tool_supports_multiline_body(&name.to_lowercase()) {
                    in_multiline_body = true;
                }
            }
            out_lines.push(normalized);
        } else {
            // Already canonical TOOL: line — still need to track body-mode entry.
            if let Some(rest) = trimmed.strip_prefix("TOOL:") {
                if let Some(name) = rest.trim().split_whitespace().next() {
                    if tool_supports_multiline_body(&name.to_lowercase()) {
                        in_multiline_body = true;
                    }
                }
            }
            out_lines.push(raw.to_string());
        }
    }

    out_lines.join("\n")
}

/// Parse tool calls from LLM response: lines "TOOL: tool_name arg1 arg2 ...".
pub(crate) fn parse_tool_calls(response: &str) -> Vec<(String, Vec<String>)> {
    let stripped = strip_redacted_reasoning_blocks(response);
    let dsml_norm = normalize_dsml_tool_calls(&stripped);
    let longcat_norm = normalize_longcat_tool_calls(&dsml_norm);
    let xml_norm = normalize_xml_tool_call_wrappers(&longcat_norm);
    let inline_norm = normalize_inline_adjacent_tool_calls(&xml_norm);
    let normalized = normalize_response_tool_prefixes(&inline_norm);
    parse_tool_calls_strict(&normalized)
}

/// Parse already-normalized tool lines (internal).
pub(crate) fn parse_tool_calls_strict(response: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let lines: Vec<&str> = response.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("TOOL:") {
            let rest = rest.trim();
            // Split by whitespace, respecting double-quoted args (so run_command -H "Bearer $VAR" works)
            let parts: Vec<String> = split_whitespace_respecting_quotes(rest);
            if let Some((name, fixed_args)) = parts.split_first() {
                let tool_name_lc = name.to_lowercase();
                if !tool_name_is_safe_identifier(&tool_name_lc) {
                    i += 1;
                    continue;
                }
                let mut args = fixed_args.to_vec();
                // Only certain tools support a multi-line body argument.
                let supports_body = tool_supports_multiline_body(&tool_name_lc);

                if supports_body {
                    // Collect subsequent non-TOOL: lines as a raw multi-line body (for
                    // tools like apply_patch / edit_file that need preserved whitespace).
                    i += 1;
                    let body_start = i;
                    while i < lines.len() && !lines[i].trim_start().starts_with("TOOL:") {
                        i += 1;
                    }
                    // Trim trailing blank lines from the body
                    let mut body_end = i;
                    while body_end > body_start && lines[body_end - 1].trim().is_empty() {
                        body_end -= 1;
                    }
                    if body_end > body_start {
                        let body = lines[body_start..body_end].join("\n");
                        if tool_name_lc == "search_replace" {
                            // search_replace: path on first line; old/new often span lines — merge head tokens + body.
                            if args.len() <= 1 {
                                args.push(body);
                            } else {
                                let head = args[1..].join(" ");
                                args.truncate(1);
                                let merged = if head.is_empty() {
                                    body
                                } else if body.is_empty() {
                                    head
                                } else {
                                    format!("{}\n{}", head, body)
                                };
                                args.push(merged);
                            }
                        } else {
                            args.push(body);
                        }
                    }
                    out.push((name.clone(), args));
                    continue;
                } else {
                    // For other tools, only use the header line arguments and
                    // do not consume following lines as a body.
                    out.push((name.clone(), args));
                    i += 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

