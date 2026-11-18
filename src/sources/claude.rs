//! Lectura incremental, sin modificar el transcript de Claude Code.
use super::{
    Source,
    jsonl::{Sink, Tail},
};
use crate::{
    model::*,
    util::{clip, first_line, one_line, parse_iso, rel_path},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub fn open(session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
        .ok_or_else(|| anyhow::anyhow!("HOME ausente"))?;
    open_in(&root, session, cwd)
}
pub fn open_in(root: &Path, session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let SessionRef::Id(id) = session else {
        anyhow::bail!("Claude requiere id")
    };
    anyhow::ensure!(!id.contains(['/', '\\']) && !id.is_empty(), "id inválido");
    let slug: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let projects = root.join("projects");
    let mut path = projects.join(slug).join(format!("{id}.jsonl"));
    if !path.is_file() {
        path = std::fs::read_dir(projects)?
            .filter_map(Result::ok)
            .map(|d| d.path().join(format!("{id}.jsonl")))
            .find(|p| p.is_file())
            .ok_or_else(|| anyhow::anyhow!("sesión no encontrada"))?;
    }
    let children = path.with_extension("").join("subagents");
    Ok(Box::new(Claude {
        tail: Tail::new(path),
        data: Data::new(cwd),
        children,
        readers: HashMap::new(),
    }))
}

/// Estado compartido por las dos fuentes; sólo materializa argumentos de herramientas.
pub(super) struct Data {
    pub info: SessionInfo,
    pub cwd: PathBuf,
    pending: HashMap<String, Vec<(usize, i64)>>,
    pending_files: HashMap<usize, String>,
    pub completed: HashSet<String>,
    pub tasks: HashMap<String, String>,
    seen: HashSet<String>,
    base: usize,
    task_positions: HashMap<String, usize>,
    reset_count: u64,
    todo_ids: HashMap<String, usize>,
    todo_calls: HashMap<String, usize>,
    pub changed: bool,
}
impl Data {
    pub fn new(cwd: &Path) -> Self {
        Self {
            info: SessionInfo::default(),
            cwd: cwd.into(),
            pending: HashMap::new(),
            pending_files: HashMap::new(),
            completed: HashSet::new(),
            tasks: HashMap::new(),
            seen: HashSet::new(),
            base: 0,
            task_positions: HashMap::new(),
            reset_count: 0,
            todo_ids: HashMap::new(),
            todo_calls: HashMap::new(),
            changed: false,
        }
    }
    pub fn stamp(&mut self, ts: i64) {
        if ts != 0 {
            self.info.started_at.get_or_insert(ts / 1000);
            if self.info.updated_at != Some(ts / 1000) {
                self.changed = true;
            }
            self.info.updated_at = Some(ts / 1000);
        }
    }
    pub fn event(
        &mut self,
        kind: EventKind,
        text: &str,
        detail: Option<&str>,
        ts: i64,
        id: Option<&str>,
    ) {
        let idx = self.base + self.info.events.len();
        if kind == EventKind::Task
            && let Some(id) = id
        {
            self.task_positions.insert(id.into(), idx);
        }
        self.info.events.push(Event {
            ts: ts / 1000,
            kind,
            title: one_line(text, TITLE_MAX),
            detail: detail.map(|s| clip(s, DETAIL_MAX)),
            state: if id.is_some() {
                EventState::Running
            } else {
                EventState::Ok
            },
            duration_ms: None,
            child: None,
        });
        if let Some(id) = id {
            self.pending.entry(id.into()).or_default().push((idx, ts));
        }
        if self.info.events.len() > EVENTS_MAX {
            self.info.events.remove(0);
            self.base += 1;
        }
        self.changed = true;
    }
    pub fn finish(&mut self, id: &str, error: bool, ts: i64) {
        self.finish_states(id, &[error], ts);
    }
    pub fn finish_states(&mut self, id: &str, errors: &[bool], ts: i64) {
        if let Some(items) = self.pending.remove(id) {
            for (n, (idx, start)) in items.into_iter().enumerate() {
                let error = errors
                    .get(n)
                    .copied()
                    .unwrap_or_else(|| errors.iter().any(|e| *e));
                if let Some(path) = self.pending_files.remove(&idx)
                    && !error
                {
                    self.file(&path, ts);
                }
                if let Some(e) = idx
                    .checked_sub(self.base)
                    .and_then(|i| self.info.events.get_mut(i))
                {
                    e.state = if error {
                        EventState::Error
                    } else {
                        EventState::Ok
                    };
                    e.duration_ms = Some(ts.saturating_sub(start).max(0) as u64);
                }
            }
            self.changed = true;
        }
        self.completed.insert(id.into());
    }
    pub fn link_task(&mut self, tool: &str, child: &str) {
        if let Some(idx) = self.task_positions.get(tool)
            && let Some(e) = idx
                .checked_sub(self.base)
                .and_then(|i| self.info.events.get_mut(i))
            && e.child.as_deref() != Some(child)
        {
            e.child = Some(child.into());
            self.changed = true;
        }
    }
    pub fn prompt(&mut self, text: &str, ts: i64) {
        let text = clean_prompt(text);
        if text.is_empty() {
            return;
        }
        let ids: Vec<_> = self.pending.keys().cloned().collect();
        for id in ids {
            self.finish(&id, true, ts);
        }
        self.info.last_prompt = Some(one_line(&text, TITLE_MAX));
        self.info.turn_started_at = Some(ts / 1000);
        self.event(
            EventKind::Prompt,
            &first_line(&text, TITLE_MAX),
            Some(&text),
            ts,
            None,
        );
    }
    pub fn reply(&mut self, text: &str, ts: i64) {
        if text.trim().is_empty() {
            return;
        }
        self.info.last_reply = Some(clip(text, DETAIL_MAX));
        self.event(
            EventKind::Reply,
            &first_line(text, TITLE_MAX),
            Some(text),
            ts,
            None,
        );
    }
    pub fn queue_file(&mut self, path: &str) {
        self.pending_files
            .insert(self.base + self.info.events.len(), path.into());
    }
    fn file(&mut self, path: &str, ts: i64) {
        let path = rel_path(path, &self.cwd);
        let edits = self
            .info
            .files
            .iter()
            .position(|f| f.path == path)
            .map(|i| self.info.files.remove(i).edits)
            .unwrap_or(0);
        self.info.files.push(FileTouch {
            path,
            edits: edits.saturating_add(1),
            last_ts: ts / 1000,
        });
    }
    pub fn todos(&mut self, v: &Value) {
        if let Some(list) = v.as_array() {
            self.info.todos = list
                .iter()
                .map(|t| Todo {
                    text: clip(
                        t.get("content")
                            .or_else(|| t.get("step"))
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        DETAIL_MAX,
                    ),
                    state: match t.get("status").and_then(Value::as_str) {
                        Some("completed") => TodoState::Done,
                        Some("in_progress") => TodoState::Active,
                        _ => TodoState::Pending,
                    },
                })
                .collect();
        }
    }
    pub fn tool(&mut self, name: &str, args: &Value, id: &str, ts: i64) {
        let get = |keys: &[&str]| -> String {
            keys.iter()
                .find_map(|k| {
                    args.get(k).and_then(|v| {
                        v.as_str().map(str::to_owned).or_else(|| {
                            v.as_array().map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                        })
                    })
                })
                .unwrap_or_default()
        };
        let (kind, title, detail) = match name.rsplit('.').next().unwrap_or(name) {
            "Bash" | "exec_command" | "shell_command" | "shell" => {
                let cmd = get(&["command", "cmd"]);
                let mut title = get(&["description"]).replace('\n', " ");
                if title.trim().is_empty() {
                    title = cmd_title(&cmd);
                }
                (EventKind::Shell, title, cmd)
            }
            "Read" => (
                EventKind::Read,
                rel_path(&get(&["file_path", "path"]), &self.cwd),
                String::new(),
            ),
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                let p = get(&["file_path", "notebook_path", "path"]);
                self.queue_file(&p);
                (EventKind::Edit, rel_path(&p, &self.cwd), String::new())
            }
            "Grep" | "Glob" => {
                let (pattern, path) = (get(&["pattern"]), get(&["path"]));
                let title = if path.is_empty() { pattern } else { format!("{pattern}  en {}", rel_path(&path, &self.cwd)) };
                (EventKind::Search, title, String::new())
            }
            "WebFetch" | "WebSearch" => (EventKind::Web, get(&["query", "url"]), String::new()),
            "Task" | "Agent" | "spawn_agent" => {
                self.tasks
                    .insert(id.into(), get(&["description", "message", "prompt"]));
                (
                    EventKind::Task,
                    get(&["description", "message", "prompt"]),
                    String::new(),
                )
            }
            "TodoWrite" | "update_plan" | "TaskCreate" | "TaskUpdate" => {
                if let Some(v) = args.get("todos").or_else(|| args.get("plan")) {
                    self.todos(v);
                }
                if name == "TaskCreate" {
                    self.todo_calls.insert(id.into(), self.info.todos.len());
                    self.info.todos.push(Todo {
                        text: get(&["subject"]),
                        state: TodoState::Pending,
                    });
                }
                if name == "TaskUpdate" {
                    let key = get(&["taskId", "task_id"]);
                    if let Some(index) = self.todo_ids.get(&key).copied()
                        && let Some(todo) = self.info.todos.get_mut(index)
                    {
                        if let Some(subject) = args.get("subject").and_then(Value::as_str) {
                            todo.text = clip(subject, DETAIL_MAX);
                        }
                        if let Some(status) = args.get("status").and_then(Value::as_str) {
                            todo.state = match status {
                                "completed" | "deleted" => TodoState::Done,
                                "in_progress" => TodoState::Active,
                                _ => TodoState::Pending,
                            };
                        }
                    }
                }
                let title = self
                    .info
                    .todos
                    .iter()
                    .find(|t| t.state == TodoState::Active)
                    .map(|t| t.text.clone())
                    .unwrap_or_else(|| {
                        format!(
                            "{}/{} hechas",
                            self.info
                                .todos
                                .iter()
                                .filter(|t| t.state == TodoState::Done)
                                .count(),
                            self.info.todos.len()
                        )
                    });
                (EventKind::Todo, title, String::new())
            }
            _ => {
                let tool = name
                    .strip_prefix("mcp__")
                    .unwrap_or(name)
                    .replace("__", " · ");
                let mut arg = get(&[
                    "query",
                    "description",
                    "path",
                    "url",
                    "command",
                    "message",
                    "sql",
                    "text",
                    "input",
                    "pattern",
                ]);
                if arg.is_empty() {
                    arg = args
                        .as_object()
                        .and_then(|a| a.values().find_map(Value::as_str))
                        .unwrap_or("")
                        .into();
                }
                (
                    EventKind::Tool,
                    if arg.is_empty() {
                        tool
                    } else {
                        format!("{tool} · {arg}")
                    },
                    String::new(),
                )
            }
        };
        self.event(
            kind,
            if title.is_empty() { &detail } else { &title },
            (!detail.is_empty()).then_some(detail.as_str()),
            ts,
            Some(id),
        );
    }
}
/// Título de un comando de shell: su primera línea; si tiene más (heredocs, scripts), se marca con `…`.
pub(super) fn cmd_title(cmd: &str) -> String {
    let mut lines = cmd.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("");
    if lines.next().is_some() { format!("{first} …") } else { first.to_string() }
}

pub(super) fn clean_prompt(text: &str) -> String {
    let mut s = text.to_owned();
    for tag in [
        "system-reminder",
        "local-command-caveat",
        "local-command-stdout",
        "local-command-stderr",
        "command-name",
        "command-message",
        "command-args",
        "environment_context",
        "environment",
        "INSTRUCTIONS",
        "permissions",
        "collaboration_mode",
        "user_instructions",
        "developer_instructions",
        "skills_instructions",
        "turn_aborted",
    ] {
        let start = format!("<{tag}");
        while let Some(a) = s.find(&start) {
            let end = format!("</{tag}>");
            let b = s[a..]
                .find(&end)
                .map(|b| a + b + end.len())
                .unwrap_or(s.len());
            s.replace_range(a..b, "");
        }
    }
    while let Some(start) = s.find("<local-command-") {
        let rest = &s[start + 1..];
        let tag = rest
            .split(|c: char| c == '>' || c.is_whitespace())
            .next()
            .unwrap_or("");
        let close = format!("</{tag}>");
        let end = s[start..]
            .find(&close)
            .map(|end| start + end + close.len())
            .unwrap_or(s.len());
        s.replace_range(start..end, "");
    }
    if s.trim_start().starts_with("# AGENTS.md instructions") {
        return String::new();
    }
    s.trim().to_owned()
}
pub(super) fn timestamp(s: Option<&str>) -> i64 {
    s.and_then(|s| {
        parse_iso(s).map(|secs| {
            let frac = s
                .get(19..)
                .and_then(|s| s.strip_prefix('.'))
                .map(|s| {
                    s.chars()
                        .take_while(char::is_ascii_digit)
                        .take(3)
                        .collect::<String>()
                })
                .unwrap_or_default();
            secs * 1000 + format!("{frac:0<3}").parse::<i64>().unwrap_or(0)
        })
    })
    .unwrap_or(0)
}
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Block {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub input: Option<Value>,
    pub tool_use_id: Option<String>,
    pub content: Option<Content>,
    pub is_error: bool,
}
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Content {
    Text(String),
    Blocks(Vec<Block>),
}
impl Content {
    pub fn text(&self) -> String {
        match self {
            Self::Text(t) => t.clone(),
            Self::Blocks(b) => b
                .iter()
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    content: Option<Content>,
    usage: Option<Usage>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    message: Option<Message>,
    #[serde(rename = "isMeta")]
    is_meta: bool,
    effort: Option<String>,
    summary: Option<String>,
    #[serde(rename = "aiTitle")]
    title: Option<String>,
    subtype: Option<String>,
    content: Option<String>,
    #[serde(rename = "totalCostUSD")]
    cost: Option<f64>,
    params: Option<Params>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Params {
    model: Option<String>,
    output_config: Option<OutputConfig>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct OutputConfig {
    effort: Option<String>,
}
impl Sink for Data {
    fn reset(&mut self) {
        let cwd = self.cwd.clone();
        let resets = self.reset_count + 1;
        *self = Self::new(&cwd);
        self.reset_count = resets;
        self.changed = true;
    }
    fn line(&mut self, line: &[u8]) {
        let Ok(e) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let ts = timestamp(e.timestamp.as_deref());
        self.stamp(ts);
        if let Some(t) = e.title.or(e.summary) {
            self.info.title = Some(one_line(&t, TITLE_MAX));
            self.changed = true;
        }
        if let Some(effort) = e.effort {
            self.info.effort = Some(effort);
            self.changed = true;
        }
        if let Some(cost) = e.cost {
            self.info.cost_usd = Some(cost);
            self.changed = true;
        }
        if let Some(p) = e.params {
            if let Some(m) = p.model {
                self.info.model = Some(m);
            }
            if let Some(c) = p.output_config
                && let Some(effort) = c.effort
            {
                self.info.effort = Some(effort);
            }
            self.changed = true;
        }
        if e.kind == "system" && e.subtype.as_deref() != Some("local_command") {
            self.event(
                EventKind::Notice,
                e.content
                    .as_deref()
                    .or(e.subtype.as_deref())
                    .unwrap_or("Sistema"),
                e.content.as_deref(),
                ts,
                None,
            );
        }
        let Some(m) = e.message else { return };
        if e.kind == "assistant" {
            if let Some(model) = m.model {
                self.info.model = Some(model);
                self.changed = true;
            }
            if let Some(u) = m.usage
                && m.id.is_none_or(|id| self.seen.insert(id))
            {
                let input = u
                    .input_tokens
                    .saturating_add(u.cache_read_input_tokens)
                    .saturating_add(u.cache_creation_input_tokens);
                self.info.tokens_in = self.info.tokens_in.saturating_add(input);
                self.info.tokens_out = self.info.tokens_out.saturating_add(u.output_tokens);
                self.info.context_used = Some(input);
                self.changed = true;
            }
        }
        let Some(c) = m.content else { return };
        if let Content::Blocks(blocks) = &c {
            for b in blocks {
                match b.kind.as_str() {
                    "tool_use" if e.kind == "assistant" => self.tool(
                        b.name.as_deref().unwrap_or("Tool"),
                        b.input.as_ref().unwrap_or(&Value::Null),
                        b.id.as_deref().unwrap_or(""),
                        ts,
                    ),
                    "tool_result" => {
                        let id = b.tool_use_id.as_deref().unwrap_or("");
                        if let Some(index) = self.todo_calls.remove(id)
                            && let Some(text) = b.content.as_ref().map(Content::text)
                        {
                            let task = serde_json::from_str::<Value>(&text)
                                .ok()
                                .and_then(|v| {
                                    v.get("task")
                                        .and_then(|v| v.get("id"))
                                        .or_else(|| v.get("id"))
                                        .map(|v| {
                                            v.as_str()
                                                .map(str::to_owned)
                                                .unwrap_or_else(|| v.to_string())
                                        })
                                })
                                .or_else(|| {
                                    text.split_once("Task #").map(|(_, s)| {
                                        s.split_whitespace().next().unwrap_or("").to_owned()
                                    })
                                });
                            if let Some(task) = task {
                                self.todo_ids.insert(task, index);
                            }
                        }
                        self.finish(id, b.is_error, ts)
                    }
                    _ => (),
                }
            }
        }
        if e.kind == "user" && !e.is_meta {
            self.prompt(&c.text(), ts);
        }
        if e.kind == "assistant" {
            self.reply(&c.text(), ts);
        }
    }
}
struct Claude {
    tail: Tail,
    data: Data,
    children: PathBuf,
    readers: HashMap<String, (Tail, Data)>,
}
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Meta {
    agent_type: Option<String>,
    description: Option<String>,
    tool_use_id: Option<String>,
    model: Option<String>,
    finished: Option<bool>,
}
impl Source for Claude {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        self.data.changed = false;
        let resets = self.data.reset_count;
        self.tail.poll(&mut self.data)?;
        if self.data.reset_count != resets {
            self.readers.clear();
        }
        if let Ok(entries) = std::fs::read_dir(&self.children) {
            let mut paths: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .collect();
            paths.sort();
            for path in paths {
                let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_prefix("agent-"))
                else {
                    continue;
                };
                let (tail, data) = self
                    .readers
                    .entry(id.into())
                    .or_insert_with(|| (Tail::new(&path), Data::new(&self.data.cwd)));
                tail.poll(data)?;
                let meta: Meta = std::fs::File::open(path.with_extension("meta.json"))
                    .ok()
                    .and_then(|f| serde_json::from_reader(f).ok())
                    .unwrap_or_default();
                let child = Child {
                    id: id.into(),
                    name: one_line(
                        meta.description
                            .as_deref()
                            .or_else(|| {
                                meta.tool_use_id
                                    .as_ref()
                                    .and_then(|t| self.data.tasks.get(t).map(String::as_str))
                            })
                            .unwrap_or(id),
                        TITLE_MAX,
                    ),
                    agent_type: meta.agent_type,
                    model: meta.model.or_else(|| data.info.model.clone()),
                    running: !meta.finished.unwrap_or_else(|| {
                        meta.tool_use_id
                            .as_ref()
                            .is_some_and(|t| self.data.completed.contains(t))
                    }),
                    started_at: data.info.started_at,
                    updated_at: data.info.updated_at,
                    tokens_in: data.info.tokens_in,
                    tokens_out: data.info.tokens_out,
                    events: data
                        .info
                        .events
                        .iter()
                        .rev()
                        .take(CHILD_EVENTS_MAX)
                        .cloned()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect(),
                    last_reply: data.info.last_reply.clone(),
                };
                if let Some(tool) = meta.tool_use_id {
                    self.data.link_task(&tool, id);
                }
                if let Some(old) = self.data.info.children.iter_mut().find(|c| c.id == id) {
                    if *old != child {
                        *old = child;
                        self.data.changed = true;
                    }
                } else {
                    self.data.info.children.push(child);
                    self.data.changed = true;
                }
            }
            self.data.info.children.sort_by_key(|c| c.started_at);
        }
        Ok(self.data.changed)
    }
    fn info(&self) -> &SessionInfo {
        &self.data.info
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    pub(crate) struct Fixture {
        pub root: PathBuf,
        pub path: PathBuf,
    }
    impl Fixture {
        pub fn new(family: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "agentview-{family}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let path = root.join(if family == "claude" {
                "projects/elsewhere/test.jsonl"
            } else {
                "sessions/2026/11/16/rollout-date-test.jsonl"
            });
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            Self { root, path }
        }
        pub fn append(&self, row: Value) {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.path)
                .unwrap();
            writeln!(f, "{row}").unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn call(id: &str, name: &str, input: Value) -> Value {
        json!({"type":"assistant","timestamp":"2026-01-01T00:00:00.250Z","message":{"content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
    }
    fn result(id: &str, error: bool) -> Value {
        json!({"type":"user","timestamp":"2026-01-01T00:00:01.750Z","message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":error,"content":"output"}]}})
    }
    #[test]
    fn tools_incremental_interrupt_reset_and_files() {
        let f = Fixture::new("claude");
        f.append(call("ok", "Bash", json!({"command":"echo a\necho b"})));
        f.append(call("bad", "Edit", json!({"file_path":"/work/a.rs"})));
        let mut src = open_in(&f.root, &SessionRef::Id("test".into()), Path::new("/work")).unwrap();
        assert!(src.refresh().unwrap());
        assert!(!src.refresh().unwrap());
        assert_eq!(src.info().events[0].state, EventState::Running);
        f.append(result("ok", false));
        f.append(result("bad", true));
        assert!(src.refresh().unwrap());
        assert_eq!(src.info().events[0].state, EventState::Ok);
        assert_eq!(src.info().events[0].duration_ms, Some(1500));
        assert_eq!(src.info().events[1].state, EventState::Error);
        assert!(src.info().files.is_empty());
        f.append(call("write", "Write", json!({"file_path":"/work/a.rs"})));
        f.append(result("write", false));
        src.refresh().unwrap();
        assert_eq!(src.info().files[0].path, "a.rs");
        f.append(call("lost", "Read", json!({"file_path":"/work/a.rs"})));
        f.append(json!({"type":"user","timestamp":"2026-01-01T00:00:03Z","message":{"content":"<system-reminder>injected</system-reminder>\nContinúa"}}));
        src.refresh().unwrap();
        assert_eq!(src.info().last_prompt.as_deref(), Some("Continúa"));
        assert_eq!(src.info().events[3].state, EventState::Error);
        std::fs::write(
            &f.path,
            "{\"type\":\"user\",\"message\":{\"content\":\"Nuevo\"}}\n",
        )
        .unwrap();
        src.refresh().unwrap();
        assert_eq!(src.info().events.len(), 1);
        assert!(src.info().files.is_empty());
        assert_eq!(src.info().last_prompt.as_deref(), Some("Nuevo"));
    }
    #[test]
    fn usage_deduplicated_and_bounded_history() {
        let mut data = Data::new(Path::new("/work"));
        for _ in 0..3 {
            data.line(json!({"type":"assistant","message":{"id":"one","model":"test-model","usage":{"input_tokens":5,"cache_read_input_tokens":7,"cache_creation_input_tokens":11,"output_tokens":13},"content":[{"type":"thinking","thinking":"secret"}]}}).to_string().as_bytes());
        }
        assert_eq!(
            (
                data.info.tokens_in,
                data.info.tokens_out,
                data.info.context_used
            ),
            (23, 13, Some(23))
        );
        assert!(data.info.events.is_empty());
        data.tool("Bash", &json!({"command":"sleep"}), "old", 0);
        for i in 0..EVENTS_MAX + 5 {
            data.reply(&format!("reply {i}"), i as i64);
        }
        data.finish("old", false, 1000);
        assert_eq!(data.info.events.len(), EVENTS_MAX);
        assert_eq!(data.info.tokens_in, 23);
        data.line(b"{bad json");
        data.line(json!({"type":"user","message":{"content":"<local-command-stdout>x</local-command-stdout>"}}).to_string().as_bytes());
        assert!(data.info.last_prompt.is_none());
    }
    #[test]
    fn subagent_meta_links_completed_parent_and_incremental_child() {
        let f = Fixture::new("claude");
        f.append(call("task1", "Agent", json!({"description":"Investigar"})));
        f.append(result("task1", false));
        let mut src = open_in(&f.root, &SessionRef::Id("test".into()), Path::new("/work")).unwrap();
        src.refresh().unwrap();
        let dir = f.path.with_extension("").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent-abc.meta.json"), json!({"agentType":"Explore","description":"Buscar","toolUseId":"task1","model":"small","finished":true}).to_string()).unwrap();
        std::fs::write(dir.join("agent-abc.jsonl"), format!("{}\n",json!({"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"id":"child","usage":{"input_tokens":8},"content":[{"type":"text","text":"Informe"}]}}))).unwrap();
        assert!(src.refresh().unwrap());
        let child = &src.info().children[0];
        assert_eq!(child.id, "abc");
        assert!(!child.running);
        assert_eq!(child.tokens_in, 8);
        assert_eq!(child.last_reply.as_deref(), Some("Informe"));
        assert_eq!(src.info().events[0].child.as_deref(), Some("abc"));
        assert!(!src.refresh().unwrap());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("agent-abc.jsonl"))
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"assistant","message":{"content":"Ampliación"}})
        )
        .unwrap();
        assert!(src.refresh().unwrap());
        assert_eq!(
            src.info().children[0].last_reply.as_deref(),
            Some("Ampliación")
        );
    }
    #[test]
    fn todos_and_unicode_caps() {
        let mut data = Data::new(Path::new("/work"));
        data.tool("TodoWrite", &json!({"todos":[{"content":"Hacer","status":"in_progress"},{"content":"Hecho","status":"completed"}]}), "todo", 0);
        assert_eq!(data.info.todos[0].state, TodoState::Active);
        assert_eq!(data.info.events[0].title, "Hacer");
        data.reply(&"é\n".repeat(5000), 0);
        assert!(data.info.events.last().unwrap().title.chars().count() <= TITLE_MAX);
        assert!(data.info.last_reply.as_ref().unwrap().chars().count() <= DETAIL_MAX);
        assert!(!data.info.events.last().unwrap().title.contains('\n'));
    }
    #[test]
    fn large_ignored_payload() {
        let mut data = Data::new(Path::new("/work"));
        let line = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"image\",\"source\":{{\"data\":\"{}\"}}}}]}}}}",
            "A".repeat(3_000_000)
        );
        data.line(line.as_bytes());
        assert!(data.info.events.is_empty());
    }
    #[test]
    fn task_create_update_and_extreme_usage() {
        let mut d = Data::new(Path::new("/work"));
        d.line(
            call("create", "TaskCreate", json!({"subject":"Preparar"}))
                .to_string()
                .as_bytes(),
        );
        d.line(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"create","content":"Task #42 created successfully: Preparar"}]}}).to_string().as_bytes());
        d.line(
            call(
                "update",
                "TaskUpdate",
                json!({"taskId":"42","status":"in_progress"}),
            )
            .to_string()
            .as_bytes(),
        );
        assert_eq!(d.info.todos[0].state, TodoState::Active);
        d.line(json!({"type":"assistant","message":{"usage":{"input_tokens":u64::MAX,"cache_read_input_tokens":1,"output_tokens":u64::MAX}}}).to_string().as_bytes());
        d.line(
            json!({"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":1}}})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(d.info.tokens_in, u64::MAX);
        assert_eq!(d.info.tokens_out, u64::MAX);
    }
    #[test]
    #[ignore = "medición de rendimiento: cargo test --release benchmark_50mb -- --ignored --nocapture"]
    fn benchmark_50mb() {
        for family in ["claude", "codex"] {
            let f = Fixture::new(family);
            let mut out = std::io::BufWriter::new(std::fs::File::create(&f.path).unwrap());
            let command = "x".repeat(25_000);
            for i in 0..1000 {
                let id = format!("call-{i}");
                if family == "claude" {
                    writeln!(out, "{}", call(&id, "Bash", json!({"command":command}))).unwrap();
                    writeln!(out, "{}", json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":command}]}})).unwrap();
                } else {
                    writeln!(out,"{}",json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":id,"input":format!("tools.exec_command({{cmd:{}}})",serde_json::to_string(&command).unwrap())}})).unwrap();
                    writeln!(out,"{}",json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":id,"output":command}})).unwrap();
                }
            }
            out.flush().unwrap();
            let session = SessionRef::Id("test".into());
            let mut source = if family == "claude" {
                open_in(&f.root, &session, Path::new("/work")).unwrap()
            } else {
                super::super::codex::open_in(&f.root, &session, Path::new("/work")).unwrap()
            };
            let t = std::time::Instant::now();
            source.refresh().unwrap();
            let load = t.elapsed();
            let t = std::time::Instant::now();
            for _ in 0..100 {
                assert!(!source.refresh().unwrap());
            }
            let idle = t.elapsed() / 100;
            println!(
                "{family}: {} bytes; carga {load:?}; refresco medio {idle:?}",
                std::fs::metadata(&f.path).unwrap().len()
            );
            assert!(load.as_secs_f64() < 1.5);
            assert!(idle.as_secs_f64() < 0.001);
            assert_eq!(source.info().events.len(), EVENTS_MAX);
        }
    }
    #[test]
    fn strips_unknown_local_wrappers_and_preserves_followup() {
        assert_eq!(
            clean_prompt("<local-command-extra>noise</local-command-extra> Sigue"),
            "Sigue"
        );
        assert_eq!(
            clean_prompt("<turn_aborted>cancelado</turn_aborted> Continúa"),
            "Continúa"
        );
    }
}
