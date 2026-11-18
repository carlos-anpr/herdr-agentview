//! Fuentes rollout de Codex, tanto function_call como custom_tool_call.
use super::{
    Source,
    claude::{Content, Data, clean_prompt, timestamp},
    jsonl::{Sink, Tail},
};
use crate::{
    model::*,
    util::{clip, first_line, one_line},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub fn open(session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
        .ok_or_else(|| anyhow::anyhow!("HOME ausente"))?;
    open_in(&root, session, cwd)
}
pub fn open_in(root: &Path, session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let SessionRef::Id(id) = session else {
        anyhow::bail!("Codex requiere id")
    };
    anyhow::ensure!(!id.contains(['/', '\\']) && !id.is_empty(), "id inválido");
    fn find(dir: &Path, suffix: &str, depth: usize) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
            let path = e.path();
            if e.file_type().ok()?.is_dir() && depth > 0 {
                if let Some(p) = find(&path, suffix, depth - 1) {
                    return Some(p);
                }
            } else if e.file_name().to_string_lossy().ends_with(suffix) {
                return Some(path);
            }
        }
        None
    }
    let path = find(&root.join("sessions"), &format!("-{id}.jsonl"), 3)
        .ok_or_else(|| anyhow::anyhow!("sesión no encontrada"))?;
    Ok(Box::new(Codex {
        tail: Tail::new(path),
        data: Rollout::new(cwd),
    }))
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Row {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    payload: Payload,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    role: Option<String>,
    content: Option<Content>,
    message: Option<String>,
    input: Option<String>,
    arguments: Option<String>,
    output: Option<Content>,
    action: Option<Action>,
    model: Option<String>,
    effort: Option<String>,
    reasoning_effort: Option<String>,
    thread_settings: Option<Settings>,
    info: Option<TokenInfo>,
    rate_limits: Option<Rates>,
    model_context_window: Option<u64>,
    timestamp: Option<String>,
    last_agent_message: Option<String>,
    usage: Option<Tokens>,
    thread_token_usage: Option<Tokens>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Action {
    command: Vec<String>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Settings {
    model: Option<String>,
    reasoning_effort: Option<String>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Tokens {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct TokenInfo {
    total_token_usage: Option<Tokens>,
    last_token_usage: Option<Tokens>,
    model_context_window: Option<u64>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Rates {
    primary: Option<Rate>,
    secondary: Option<Rate>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Rate {
    used_percent: f32,
    window_minutes: u64,
    resets_at: Option<i64>,
}
struct Rollout {
    data: Data,
    seen: HashSet<String>,
    usage_seen: HashSet<String>,
    last_prompt: Option<(i64, String)>,
    groups: HashMap<String, Vec<usize>>,
    controls: HashMap<String, (String, Value)>,
}
impl Rollout {
    fn new(cwd: &Path) -> Self {
        Self {
            data: Data::new(cwd),
            seen: HashSet::new(),
            usage_seen: HashSet::new(),
            last_prompt: None,
            groups: HashMap::new(),
            controls: HashMap::new(),
        }
    }
    fn prompt(&mut self, text: &str, ts: i64) {
        let text = clean_prompt(text);
        // The event_msg and response_item may both record the same user message.
        if self
            .last_prompt
            .as_ref()
            .is_some_and(|(t, s)| *t / 1000 == ts / 1000 && s == &text)
        {
            return;
        }
        if !text.is_empty() {
            self.data.prompt(&text, ts);
            self.last_prompt = Some((ts, text));
        }
    }
    fn tokens(&mut self, total: Option<Tokens>, last: Option<Tokens>, window: Option<u64>) {
        if let Some(t) = total {
            self.data.info.tokens_in = t.input_tokens;
            self.data.info.tokens_out = t.output_tokens;
            self.data.changed = true;
        }
        if let Some(t) = last {
            self.data.info.context_used = Some(if t.total_tokens > 0 {
                t.total_tokens
            } else {
                t.input_tokens.saturating_add(t.output_tokens)
            });
            self.data.changed = true;
        }
        if let Some(w) = window {
            self.data.info.context_window = Some(w);
            self.data.changed = true;
        }
    }
    fn patch(&mut self, patch: &str, id: &str, ts: i64) -> usize {
        let mut count = 0;
        for line in patch.lines() {
            let path = [
                "*** Update File:",
                "*** Add File:",
                "*** Delete File:",
                "*** Move to:",
            ]
            .iter()
            .find_map(|p| line.trim().strip_prefix(p));
            if let Some(path) = path {
                let path = path.trim();
                self.data.queue_file(path);
                self.data.event(
                    EventKind::Edit,
                    &crate::util::rel_path(path, &self.data.cwd),
                    None,
                    ts,
                    Some(id),
                );
                count += 1;
            }
        }
        count
    }
    fn call(&mut self, name: &str, input: &str, id: &str, ts: i64) {
        if !self.seen.insert(id.into()) {
            return;
        }
        let short = name.rsplit('.').next().unwrap_or(name);
        if short == "exec" {
            let mut count = 0;
            let mut groups = Vec::new();
            for (tool, arg) in js_calls(input) {
                let before = count;
                if tool == "apply_patch" {
                    count += self.patch(&arg, id, ts);
                } else if let Ok(args) = serde_json::from_str::<Value>(&arg) {
                    self.data.tool(&tool, &args, id, ts);
                    count += 1;
                } else {
                    self.data.event(
                        EventKind::Tool,
                        &format!("{tool} · {}", first_line(&arg, TITLE_MAX)),
                        Some(&arg),
                        ts,
                        Some(id),
                    );
                    count += 1;
                }
                groups.push(count - before);
            }
            self.groups.insert(id.into(), groups);
            if count == 0 {
                self.data.event(
                    EventKind::Tool,
                    &first_line(input, TITLE_MAX),
                    Some(input),
                    ts,
                    Some(id),
                );
            }
        } else if short == "apply_patch" {
            let patch = serde_json::from_str::<String>(input)
                .ok()
                .or_else(|| {
                    serde_json::from_str::<Value>(input).ok().and_then(|v| {
                        v.get("patch")
                            .or_else(|| v.get("input"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                })
                .unwrap_or_else(|| input.to_owned());
            if self.patch(&patch, id, ts) == 0 {
                self.data
                    .event(EventKind::Tool, "apply_patch", Some(&patch), ts, Some(id));
            }
        } else {
            let args = serde_json::from_str::<Value>(input).unwrap_or(Value::Null);
            if matches!(short, "spawn_agent" | "close_agent" | "wait" | "wait_agent") {
                self.controls
                    .insert(id.into(), (short.into(), args.clone()));
            }
            self.data.tool(short, &args, id, ts);
        }
    }
}
impl Sink for Rollout {
    fn reset(&mut self) {
        let cwd = self.data.cwd.clone();
        *self = Self::new(&cwd);
        self.data.changed = true;
    }
    fn line(&mut self, line: &[u8]) {
        let Ok(row) = serde_json::from_slice::<Row>(line) else {
            return;
        };
        let ts = timestamp(row.timestamp.as_deref());
        self.data.stamp(ts);
        let p = row.payload;
        if row.kind == "session_meta"
            && let Some(start) = p.timestamp
        {
            self.data.info.started_at = Some(timestamp(Some(&start)) / 1000);
        }
        if row.kind == "turn_context" || p.kind == "thread_settings_applied" {
            let settings = p.thread_settings.unwrap_or_default();
            if let Some(m) = p.model.or(settings.model) {
                self.data.info.model = Some(m);
            }
            if let Some(e) = p
                .effort
                .or(p.reasoning_effort)
                .or(settings.reasoning_effort)
            {
                self.data.info.effort = Some(e);
            }
            self.data.changed = true;
        }
        if row.kind == "token_usage_record" {
            let usage = p.usage;
            let unique =
                p.id.as_ref()
                    .is_none_or(|id| self.usage_seen.insert(id.clone()));
            if unique {
                self.tokens(p.thread_token_usage, usage, None);
            }
            return;
        }
        if p.kind == "token_count" {
            if let Some(info) = p.info {
                self.tokens(
                    info.total_token_usage,
                    info.last_token_usage,
                    info.model_context_window,
                );
            }
            if let Some(rates) = p.rate_limits {
                self.data.info.limits = [rates.primary, rates.secondary]
                    .into_iter()
                    .flatten()
                    .map(|r| Limit {
                        name: if r.window_minutes.is_multiple_of(1440) && r.window_minutes > 0 {
                            format!("{}d", r.window_minutes / 1440)
                        } else if r.window_minutes.is_multiple_of(60) {
                            format!("{}h", r.window_minutes / 60)
                        } else {
                            format!("{}h", r.window_minutes as f64 / 60.0)
                        },
                        used_pct: r.used_percent.clamp(0.0, 100.0),
                        resets_at: r.resets_at,
                    })
                    .collect();
                self.data.changed = true;
            }
        }
        if let Some(w) = p.model_context_window {
            self.data.info.context_window = Some(w);
            self.data.changed = true;
        }
        if row.kind == "response_item" {
            match p.kind.as_str() {
                "message" => {
                    if let Some(c) = p.content {
                        if p.role.as_deref() == Some("user") {
                            self.prompt(&c.text(), ts);
                        } else if p.role.as_deref() == Some("assistant") {
                            let text = c.text();
                            if self.data.info.last_reply.as_deref()
                                != Some(clip(&text, DETAIL_MAX).as_str())
                            {
                                self.data.reply(&text, ts);
                            }
                        }
                    }
                }
                "function_call" | "custom_tool_call" => self.call(
                    p.name.as_deref().unwrap_or("Tool"),
                    p.arguments.as_deref().or(p.input.as_deref()).unwrap_or(""),
                    p.call_id.as_deref().or(p.id.as_deref()).unwrap_or(""),
                    ts,
                ),
                "local_shell_call" => {
                    let cmd = p.action.map(|a| a.command.join(" ")).unwrap_or_default();
                    self.call(
                        "exec_command",
                        &serde_json::json!({"cmd":cmd}).to_string(),
                        p.call_id.as_deref().or(p.id.as_deref()).unwrap_or(""),
                        ts,
                    );
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let parts: Vec<String> = match p.output {
                        Some(Content::Blocks(blocks)) => blocks
                            .into_iter()
                            .filter_map(|b| b.text)
                            .filter(|s| {
                                !s.starts_with("Script completed\n")
                                    && !s.starts_with("Script running with cell ID ")
                            })
                            .collect(),
                        Some(Content::Text(s)) => vec![s],
                        None => Vec::new(),
                    };
                    let text = parts.join("\n");
                    let id = p.call_id.as_deref().unwrap_or("");
                    if let Some(groups) = self.groups.remove(id).filter(|g| g.len() == parts.len())
                    {
                        let errors: Vec<_> = groups
                            .iter()
                            .zip(&parts)
                            .flat_map(|(n, s)| std::iter::repeat_n(output_error(s), *n))
                            .collect();
                        self.data.finish_states(id, &errors, ts);
                    } else {
                        self.data.finish(id, output_error(&text), ts);
                    }
                    let control = self.controls.remove(id);
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if let Some(child) = v.get("agent_id").and_then(Value::as_str) {
                            if !self.data.info.children.iter().any(|c| c.id == child) {
                                self.data.info.children.push(Child {
                                    id: child.into(),
                                    name: one_line(
                                        self.data
                                            .tasks
                                            .get(id)
                                            .map(String::as_str)
                                            .unwrap_or(child),
                                        TITLE_MAX,
                                    ),
                                    model: control
                                        .as_ref()
                                        .and_then(|(_, args)| args.get("model"))
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                    running: true,
                                    started_at: Some(ts / 1000),
                                    ..Child::default()
                                });
                            }
                            self.data.link_task(id, child);
                        }
                        if let Some(statuses) = v.get("status").and_then(Value::as_object) {
                            for (child, status) in statuses {
                                if let Some(c) =
                                    self.data.info.children.iter_mut().find(|c| &c.id == child)
                                {
                                    if let Some(reply) =
                                        status.get("completed").and_then(Value::as_str)
                                    {
                                        c.last_reply = Some(clip(reply, DETAIL_MAX));
                                        c.running = false;
                                    }
                                    if status.get("errored").is_some()
                                        || status.as_str() == Some("shutdown")
                                    {
                                        c.running = false;
                                    }
                                    c.updated_at = Some(ts / 1000);
                                    self.data.changed = true;
                                }
                            }
                        }
                        if let Some((name, args)) = control
                            && name == "close_agent"
                            && !output_error(&text)
                            && let Some(child) = args
                                .get("agent_id")
                                .or_else(|| args.get("id"))
                                .and_then(Value::as_str)
                            && let Some(c) =
                                self.data.info.children.iter_mut().find(|c| c.id == child)
                        {
                            c.running = false;
                            c.updated_at = Some(ts / 1000);
                            self.data.changed = true;
                        }
                    }
                }
                _ => (),
            }
        } else if row.kind == "event_msg" {
            match p.kind.as_str() {
                "user_message" => {
                    if let Some(text) = p.message {
                        self.prompt(&text, ts);
                    }
                }
                "agent_message" => {
                    if let Some(text) = p.message
                        && self.data.info.last_reply.as_deref()
                            != Some(clip(&text, DETAIL_MAX).as_str())
                    {
                        self.data.reply(&text, ts);
                    }
                }
                "task_complete" => {
                    if let Some(text) = p.last_agent_message
                        && self.data.info.last_reply.as_deref()
                            != Some(clip(&text, DETAIL_MAX).as_str())
                    {
                        self.data.reply(&text, ts);
                    }
                }
                "context_compacted" | "error" | "warning" | "model_changed" => self.data.event(
                    EventKind::Notice,
                    p.message.as_deref().unwrap_or(&p.kind),
                    p.message.as_deref(),
                    ts,
                    None,
                ),
                _ => (),
            }
        }
    }
}
fn output_error(text: &str) -> bool {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if v.get("isError")
            .or_else(|| v.get("is_error"))
            .and_then(Value::as_bool)
            == Some(true)
            || v.get("error")
                .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
        {
            return true;
        }
        if let Some(code) = v
            .get("exit_code")
            .or_else(|| v.get("metadata").and_then(|m| m.get("exit_code")))
            .and_then(Value::as_i64)
        {
            return code != 0;
        }
        if let Some(s) = v.get("output").and_then(Value::as_str) {
            return output_error(s);
        }
    }
    for prefix in [
        "Process exited with code ",
        "Process exit code: ",
        "Exit code: ",
        "\"exit_code\":",
    ] {
        if let Some((_, rest)) = text.split_once(prefix)
            && let Some(n) = rest
                .trim_start()
                .split(|c: char| !c.is_ascii_digit() && c != '-')
                .next()
                .and_then(|s| s.parse::<i64>().ok())
            && n != 0
        {
            return true;
        }
    }
    text.starts_with("Error:") || text.starts_with("Error ") || text.contains("isError\":true")
}

/// Small lexical scanner: ignore comments/strings and balance object literals. Never executes JS.
fn js_calls(js: &str) -> Vec<(String, String)> {
    let b = js.as_bytes();
    let mut i = 0;
    let mut calls = Vec::new();
    while i < b.len() {
        if matches!(b[i], b'"' | b'\'' | b'`') {
            i = string_end(b, i);
            continue;
        }
        if b.get(i..i + 2) == Some(b"//") {
            i += b[i..]
                .iter()
                .position(|c| *c == b'\n')
                .unwrap_or(b.len() - i);
            continue;
        }
        if b.get(i..i + 2) == Some(b"/*") {
            i += js[i + 2..].find("*/").map(|n| n + 4).unwrap_or(b.len() - i);
            continue;
        }
        if b.get(i..i + 6) != Some(b"tools.") {
            i += 1;
            continue;
        }
        i += 6;
        let start = i;
        while b
            .get(i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            i += 1;
        }
        let name = &js[start..i];
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if b.get(i) != Some(&b'(') {
            continue;
        }
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let start = i;
        let mut depth = 0;
        while i < b.len() {
            if matches!(b[i], b'"' | b'\'' | b'`') {
                i = string_end(b, i);
                continue;
            }
            match b[i] {
                b'{' | b'[' | b'(' => depth += 1,
                b'}' | b']' => depth -= 1,
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                _ => (),
            }
            i += 1;
        }
        let arg = js[start..i].trim();
        let arg = if arg.starts_with('"') {
            serde_json::from_str::<String>(arg).unwrap_or_else(|_| arg.into())
        } else if arg.len() >= 2
            && ((arg.starts_with('`') && arg.ends_with('`'))
                || (arg.starts_with('\'') && arg.ends_with('\'')))
        {
            arg[1..arg.len() - 1]
                .replace("\\n", "\n")
                .replace("\\'", "'")
        } else {
            normalize_object(arg)
        };
        calls.push((name.into(), arg));
        i += usize::from(i < b.len());
    }
    calls
}
fn string_end(b: &[u8], start: usize) -> usize {
    let quote = b[start];
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i = (i + 2).min(b.len());
        } else if b[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    i
}
/// Quote JS identifier keys and convert single quoted strings for JSON decoding.
fn normalize_object(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b'"' | b'\'' | b'`') {
            let end = string_end(b, i);
            if b[i] != b'"' && end > i + 1 && b.get(end - 1) == Some(&b[i]) {
                out.push_str(
                    &serde_json::to_string(
                        &s[i + 1..end - 1].replace("\\'", "'").replace("\\n", "\n"),
                    )
                    .unwrap_or_default(),
                );
            } else {
                out.push_str(&s[i..end]);
            }
            i = end;
        } else if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let start = i;
            while b
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                i += 1;
            }
            let mut next = i;
            while b.get(next).is_some_and(u8::is_ascii_whitespace) {
                next += 1;
            }
            if b.get(next) == Some(&b':') {
                out.push('"');
                out.push_str(&s[start..i]);
                out.push('"');
            } else {
                out.push_str(&s[start..i]);
            }
        } else {
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}
struct Codex {
    tail: Tail,
    data: Rollout,
}
impl Source for Codex {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        self.data.data.changed = false;
        self.tail.poll(&mut self.data)?;
        Ok(self.data.data.changed)
    }
    fn info(&self) -> &SessionInfo {
        &self.data.data.info
    }
}

#[cfg(test)]
mod tests {
    use super::super::claude::tests::Fixture;
    use super::*;
    use serde_json::json;
    fn row(kind: &str, payload: Value) -> Value {
        json!({"timestamp":"2026-01-01T00:00:01.250Z","type":kind,"payload":payload})
    }
    #[test]
    fn both_tool_generations_results_and_incremental_reset() {
        let f = Fixture::new("codex");
        f.append(row("response_item",json!({"type":"function_call","name":"exec_command","call_id":"old","arguments":"{\"cmd\":\"ls\"}"})));
        f.append(row("response_item",json!({"type":"function_call_output","call_id":"old","output":"Process exited with code 0"})));
        f.append(row("response_item",json!({"type":"custom_tool_call","name":"exec","call_id":"new","input":"await tools.exec_command({cmd: \"false\", workdir: \"/work\"}); await tools.apply_patch(\"*** Begin Patch\\n*** Update File: /work/a.rs\\n@@\\n-x\\n+y\\n*** End Patch\")"})));
        let mut src = open_in(&f.root, &SessionRef::Id("test".into()), Path::new("/work")).unwrap();
        src.refresh().unwrap();
        assert_eq!(src.info().events.len(), 3);
        assert_eq!(src.info().events[0].state, EventState::Ok);
        assert_eq!(src.info().events[1].kind, EventKind::Shell);
        assert_eq!(src.info().events[2].title, "a.rs");
        assert!(!src.refresh().unwrap());
        f.append(row("response_item",json!({"type":"custom_tool_call_output","call_id":"new","output":[{"type":"input_text","text":"Process exit code: 1"}]})));
        src.refresh().unwrap();
        assert_eq!(src.info().events[1].state, EventState::Error);
        assert_eq!(src.info().events[2].state, EventState::Error);
        f.append(row("response_item",json!({"type":"local_shell_call","id":"local","action":{"command":["sh","-c","sleep 5"]}})));
        f.append(row("response_item",json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>injected</environment_context>\nStop"}]})));
        src.refresh().unwrap();
        assert_eq!(src.info().last_prompt.as_deref(), Some("Stop"));
        assert_eq!(src.info().events[3].state, EventState::Error);
        std::fs::write(
            &f.path,
            format!(
                "{}\n",
                row(
                    "response_item",
                    json!({"type":"message","role":"assistant","content":"Nuevo"})
                )
            ),
        )
        .unwrap();
        src.refresh().unwrap();
        assert_eq!(src.info().events.len(), 1);
        assert!(src.info().files.is_empty());
    }
    #[test]
    fn tokens_limits_settings_and_prompt_filters() {
        let mut r = Rollout::new(Path::new("/work"));
        let mut send = |v: Value| r.line(v.to_string().as_bytes());
        send(row(
            "turn_context",
            json!({"model":"gpt-test","effort":"high"}),
        ));
        for _ in 0..2 {
            send(row(
                "event_msg",
                json!({"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":30},"last_token_usage":{"total_tokens":45},"model_context_window":200000},"rate_limits":{"primary":{"used_percent":12.5,"window_minutes":300,"resets_at":123},"secondary":{"used_percent":70,"window_minutes":10080}}}),
            ));
        }
        send(row(
            "response_item",
            json!({"type":"message","role":"developer","content":"Instructions"}),
        ));
        send(row(
            "response_item",
            json!({"type":"message","role":"user","content":"# AGENTS.md instructions\nRules"}),
        ));
        send(row(
            "response_item",
            json!({"type":"message","role":"user","content":"Hola"}),
        ));
        send(row(
            "event_msg",
            json!({"type":"user_message","message":"Hola"}),
        ));
        assert_eq!(r.data.info.model.as_deref(), Some("gpt-test"));
        assert_eq!(r.data.info.effort.as_deref(), Some("high"));
        assert_eq!((r.data.info.tokens_in, r.data.info.tokens_out), (100, 30));
        assert_eq!(r.data.info.context_used, Some(45));
        assert_eq!(r.data.info.context_window, Some(200000));
        assert_eq!(r.data.info.limits[0].name, "5h");
        assert_eq!(r.data.info.limits[1].name, "7d");
        assert_eq!(r.data.info.limits[0].resets_at, Some(123));
        assert_eq!(r.data.info.events.len(), 1);
    }
    #[test]
    fn lexical_exec_patch_plan_and_unknown() {
        let mut r = Rollout::new(Path::new("/work"));
        r.call("exec", "// tools.fake({})\nconst ignored = 'tools.fake({})'; await tools.exec_command({cmd: 'echo hola', workdir: '/work'}); tools.apply_patch(`*** Add File: x.rs\n+yes\n*** Delete File: y.rs`)", "one",0);
        assert_eq!(r.data.info.events.len(), 3);
        assert_eq!(r.data.info.events[0].title, "echo hola");
        r.data.finish("one", false, 1);
        assert_eq!(r.data.info.files.len(), 2);
        r.call("update_plan",r#"{"plan":[{"step":"Preparar","status":"completed"},{"step":"Probar","status":"in_progress"}]}"#,"plan",0);
        assert_eq!(r.data.info.todos[1].state, TodoState::Active);
        r.call("exec", "let unknown = 1;", "unknown", 0);
        assert_eq!(r.data.info.events.last().unwrap().kind, EventKind::Tool);
        assert!(output_error(r#"{"exit_code":2,"output":"oops"}"#));
        assert!(!output_error("Process exited with code 0"));
    }
    #[test]
    fn grouped_results_and_malformed_javascript_never_panic() {
        let mut r = Rollout::new(Path::new("/work"));
        r.call(
            "exec",
            r#"tools.exec_command({cmd:"true"});tools.exec_command({cmd:"false"})"#,
            "group",
            0,
        );
        r.line(row("response_item",json!({"type":"custom_tool_call_output","call_id":"group","output":[{"type":"input_text","text":"Script completed\nWall time 1s"},{"type":"input_text","text":"{\"exit_code\":0}"},{"type":"input_text","text":"{\"exit_code\":1}"}]})).to_string().as_bytes());
        assert_eq!(r.data.info.events[0].state, EventState::Ok);
        assert_eq!(r.data.info.events[1].state, EventState::Error);
        for text in [
            "tools.exec_command({cmd:'é",
            "tools.apply_patch(`",
            "tools.apply_patch('",
            "tools.exec_command({cmd: `hola\nmundo`})",
        ] {
            let _ = js_calls(text);
        }
        for i in 0..EVENTS_MAX + 1 {
            r.data.reply(&i.to_string(), 0);
        }
        assert_eq!(r.data.info.events.len(), EVENTS_MAX);
    }
    #[test]
    fn identified_subagents_link_and_finish() {
        let mut r = Rollout::new(Path::new("/work"));
        r.call(
            "spawn_agent",
            r#"{"message":"Primero","model":"small"}"#,
            "a",
            0,
        );
        r.call("spawn_agent", r#"{"message":"Segundo"}"#, "b", 0);
        r.line(row("response_item",json!({"type":"function_call_output","call_id":"a","output":"{\"agent_id\":\"child-a\"}"})).to_string().as_bytes());
        assert_eq!(r.data.info.events[0].child.as_deref(), Some("child-a"));
        assert!(r.data.info.events[1].child.is_none());
        assert_eq!(r.data.info.children[0].model.as_deref(), Some("small"));
        r.call("wait", r#"{"ids":["child-a"]}"#, "wait", 0);
        r.line(row("response_item",json!({"type":"function_call_output","call_id":"wait","output":"{\"status\":{\"child-a\":{\"completed\":\"Informe\"}}}"})).to_string().as_bytes());
        assert!(!r.data.info.children[0].running);
        assert_eq!(
            r.data.info.children[0].last_reply.as_deref(),
            Some("Informe")
        );
    }
}
