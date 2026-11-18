//! Fuente de sesión de omp: el transcript JSONL que deja en `~/.omp/agent/sessions/…`.
//!
//! Una entrada JSON por línea, con `type` como discriminante. Los `toolCall` de los mensajes
//! del asistente y los `custom/tool_execution_start` describen el mismo lanzamiento: se crea un
//! solo evento y lo cierra el `message` con `role: "toolResult"`. Si el resultado no llega y
//! después hay otro mensaje del usuario, la herramienta se da por interrumpida (`Error`).

use super::Source;
use super::jsonl::{Sink, Tail};
use crate::model::{
    Child, DETAIL_MAX, EVENTS_MAX, Event, EventKind, EventState, FileTouch, SessionInfo,
    SessionRef, TITLE_MAX, Todo, TodoState,
};
use crate::util::{clip, first_line, one_line, parse_iso, rel_path};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Abre el transcript indicado por herdr (`SessionRef::Path`).
pub fn open(session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let path = match session {
        SessionRef::Path(p) => p.clone(),
        SessionRef::Id(id) => {
            anyhow::bail!("omp necesita la ruta del transcript, llegó el id «{id}»")
        }
    };
    anyhow::ensure!(
        path.is_file(),
        "transcript omp no encontrado: {}",
        path.display()
    );
    Ok(Box::new(Omp {
        tail: Tail::new(path),
        cwd: cwd.to_path_buf(),
        st: State::default(),
    }))
}

struct Omp {
    tail: Tail,
    cwd: PathBuf,
    st: State,
}

impl Source for Omp {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        let Self { tail, cwd, st } = self;
        st.changed = false;
        let mut lines = Lines { st, cwd };
        tail.poll(&mut lines)?;
        Ok(lines.st.changed)
    }

    fn info(&self) -> &SessionInfo {
        &self.st.info
    }
}

struct Lines<'a> {
    st: &'a mut State,
    cwd: &'a Path,
}

impl Sink for Lines<'_> {
    fn reset(&mut self) {
        *self.st = State::default();
        self.st.changed = true;
    }

    fn line(&mut self, line: &[u8]) {
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            self.st.ingest(&v, self.cwd);
        }
    }
}

#[derive(Default)]
struct State {
    info: SessionInfo,
    /// Número de secuencia del siguiente evento: los eventos se recortan a `EVENTS_MAX` por
    /// delante, así que el índice dentro del vector deja de servir; el absoluto sí.
    next_abs: u64,
    abs: VecDeque<u64>,
    /// Llamadas lanzadas y todavía sin resultado: id de la llamada → evento.
    pending: HashMap<String, u64>,
    /// Ficheros tocados: ruta abreviada → índice en `info.files`.
    files: HashMap<String, usize>,
    /// Subagentes de `task` con evento: id de llamada → índice en `info.children`.
    children: HashMap<String, usize>,
    last_ts: i64,
    changed: bool,
    /// El transcript ya trae el `usage` de cada mensaje del asistente: `model_usage` es de
    /// trabajos auxiliares (títulos, widgets, memoria) y sumarlo inflaría el total.
    saw_assistant_usage: bool,
    messages: HashSet<String>,
    pending_files: HashMap<String, String>,
}

impl State {
    fn ingest(&mut self, v: &Value, cwd: &Path) {
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso)
            .unwrap_or(self.last_ts);
        if v.get("timestamp").is_some_and(|t| t.as_str().is_some()) {
            self.last_ts = ts;
            self.info.updated_at = Some(self.info.updated_at.map_or(ts, |old| old.max(ts)));
        }
        self.changed = true;
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "session" if self.info.started_at.is_none() => self.info.started_at = Some(ts),
            "title" | "title_change" => {
                if let Some(t) = v
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    self.info.title = Some(one_line(t, TITLE_MAX));
                }
            }
            "model_change" => {
                if let Some(m) = v
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                {
                    self.info.model = Some(m.to_string());
                }
            }
            "thinking_level_change" => {
                if let Some(l) = v
                    .get("thinkingLevel")
                    .and_then(Value::as_str)
                    .filter(|l| !l.is_empty())
                {
                    self.info.effort = Some(l.to_string());
                }
            }
            "message" => self.message(v, ts, cwd),
            "model_usage" => {
                // Solo si el transcript es antiguo y no hay `usage` por mensaje.
                if !self.saw_assistant_usage
                    && v.get("purpose")
                        .and_then(Value::as_str)
                        .is_none_or(|p| matches!(p, "assistant" | "main"))
                    && let Some(u) = v.get("usage")
                {
                    self.add_usage(u);
                }
            }
            "custom" => self.custom(v, ts, cwd),
            "reset_boundary" => self.notice(ts, "contexto reiniciado"),
            _ => {}
        }
    }

    fn message(&mut self, v: &Value, ts: i64, cwd: &Path) {
        let Some(m) = v.get("message") else { return };
        if let Some(id) = v.get("id").or_else(|| m.get("id")).and_then(Value::as_str)
            && !self.messages.insert(id.into())
        {
            return;
        }
        match m.get("role").and_then(Value::as_str).unwrap_or_default() {
            "user" => {
                let text = super::claude::clean_prompt(&text_blocks(m.get("content")));
                if !text.trim().is_empty() {
                    self.interrupt();
                    self.info.turn_started_at = Some(ts);
                    self.info.last_prompt = Some(first_line(&text, TITLE_MAX));
                    let title = first_line(&text, TITLE_MAX);
                    self.push(Event {
                        ts,
                        kind: EventKind::Prompt,
                        detail: detail_if_longer(&text, &title),
                        title,
                        state: EventState::Ok,
                        duration_ms: None,
                        child: None,
                    });
                }
            }
            "assistant" => {
                if let Some(blocks) = m.get("content").and_then(Value::as_array) {
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str).unwrap_or_default() {
                            "text" => {
                                let t = b.get("text").and_then(Value::as_str).unwrap_or_default();
                                if t.trim().is_empty() {
                                    continue;
                                }
                                self.info.last_reply = Some(clip(t, DETAIL_MAX));
                                let title = first_line(t, TITLE_MAX);
                                self.push(Event {
                                    ts,
                                    kind: EventKind::Reply,
                                    detail: detail_if_longer(t, &title),
                                    title,
                                    state: EventState::Ok,
                                    duration_ms: None,
                                    child: None,
                                });
                            }
                            "toolCall" => self.start_tool(b, ts, cwd),
                            _ => {}
                        }
                    }
                }
                if let Some(u) = m.get("usage") {
                    if !self.saw_assistant_usage {
                        self.info.tokens_in = 0;
                        self.info.tokens_out = 0;
                        self.info.cost_usd = None;
                    }
                    self.saw_assistant_usage = true;
                    self.add_usage(u);
                }
            }
            "toolResult" => self.tool_result(m, ts),
            _ => {}
        }
    }

    fn custom(&mut self, v: &Value, ts: i64, cwd: &Path) {
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        match v
            .get("customType")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "tool_execution_start" => {
                let id = data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if id.is_empty() || self.pending.contains_key(id) {
                    return;
                }
                // Sin `toolCall` previo (formato antiguo): el lanzamiento se describe aquí.
                let start = data
                    .get("startedAt")
                    .and_then(Value::as_str)
                    .and_then(parse_iso)
                    .unwrap_or(ts);
                let name = data
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let args = data.get("args").cloned().unwrap_or(Value::Null);
                let intent = data.get("intent").and_then(Value::as_str);
                self.start_tool_at(id, name, &args, intent, start, cwd);
            }
            "todo_hud_state" => {
                if let Some(fp) = data.get("fingerprint").and_then(Value::as_str)
                    && let Some(todos) = parse_hud(fp)
                {
                    self.info.todos = todos;
                    let title = todo_title(&self.info.todos);
                    self.push(Event {
                        ts,
                        kind: EventKind::Todo,
                        title,
                        detail: None,
                        state: EventState::Ok,
                        duration_ms: None,
                        child: None,
                    });
                }
            }
            "session_exit" => {
                let reason = data.get("reason").and_then(Value::as_str).unwrap_or("fin");
                self.notice(ts, &format!("sesión terminada · {reason}"));
            }
            other if other.contains("compact") => self.notice(ts, "contexto compactado"),
            _ => {}
        }
    }

    fn start_tool(&mut self, b: &Value, ts: i64, cwd: &Path) {
        let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
        let args = b.get("arguments").cloned().unwrap_or(Value::Null);
        let intent = args
            .get("i")
            .and_then(Value::as_str)
            .or_else(|| b.get("intent").and_then(Value::as_str));
        self.start_tool_at(id, name, &args, intent, ts, cwd);
    }

    fn start_tool_at(
        &mut self,
        id: &str,
        name: &str,
        args: &Value,
        intent: Option<&str>,
        ts: i64,
        cwd: &Path,
    ) {
        if !id.is_empty() && self.pending.contains_key(id) {
            return;
        }
        let kind = classify(name);
        let (title, detail) = describe(kind, name, args, intent, cwd);
        let child = if kind == EventKind::Task && !id.is_empty() {
            let cname = args
                .get("name")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| title.clone());
            let agent = args
                .get("tasks")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|t| t.get("agent"))
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| args.get("agent").and_then(Value::as_str).map(String::from));
            self.info.children.push(Child {
                id: id.to_string(),
                name: one_line(&cname, TITLE_MAX),
                agent_type: agent,
                running: true,
                started_at: Some(ts),
                updated_at: Some(ts),
                ..Child::default()
            });
            self.children
                .insert(id.to_string(), self.info.children.len() - 1);
            Some(id.to_string())
        } else {
            None
        };
        if kind == EventKind::Edit
            && let Some(p) = path_arg(args)
        {
            self.pending_files.insert(id.into(), rel_path(p, cwd));
        }
        let abs = self.push(Event {
            ts,
            kind,
            title,
            detail,
            state: EventState::Running,
            duration_ms: None,
            child,
        });
        if !id.is_empty() {
            self.pending.insert(id.to_string(), abs);
        }
    }

    fn tool_result(&mut self, m: &Value, ts: i64) {
        let id = m
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() {
            return;
        }
        let err = m.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let ms = m
            .get("details")
            .and_then(|d| d.get("wallTimeMs"))
            .and_then(Value::as_f64)
            .filter(|x| *x >= 0.0)
            .map(|x| x as u64);
        let text = text_blocks(m.get("content"));
        if let Some(path) = self.pending_files.remove(id)
            && !err
        {
            self.touch_file(&path, Path::new(""), ts);
        }
        if let Some(abs) = self.pending.remove(id)
            && let Some(ev) = self.event_mut(abs)
        {
            ev.state = if err {
                EventState::Error
            } else {
                EventState::Ok
            };
            ev.duration_ms =
                ms.or_else(|| Some((ts.saturating_sub(ev.ts).max(0) as u64).saturating_mul(1000)));
        }
        if let Some(&i) = self.children.get(id)
            && let Some(ch) = self.info.children.get_mut(i)
        {
            ch.running = false;
            ch.updated_at = Some(ts);
            if !text.trim().is_empty() {
                ch.last_reply = Some(clip(&text, DETAIL_MAX));
            }
        }
    }

    fn add_usage(&mut self, u: &Value) {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let input = n("input");
        let cache_read = n("cacheRead");
        let cache_write = n("cacheWrite");
        self.info.tokens_in = self
            .info
            .tokens_in
            .saturating_add(input.saturating_add(cache_read).saturating_add(cache_write));
        self.info.tokens_out = self.info.tokens_out.saturating_add(n("output"));
        self.info.context_used = Some(input.saturating_add(cache_read).saturating_add(cache_write));
        if let Some(cost) = u
            .get("cost")
            .and_then(|c| c.get("total"))
            .and_then(Value::as_f64)
        {
            *self.info.cost_usd.get_or_insert(0.0) += cost;
        }
    }

    fn touch_file(&mut self, path: &str, cwd: &Path, ts: i64) {
        let rel = rel_path(path, cwd);
        match self.files.get(&rel).copied() {
            Some(i) => {
                self.info.files[i].edits = self.info.files[i].edits.saturating_add(1);
                self.info.files[i].last_ts = ts;
            }
            None => {
                self.info.files.push(FileTouch {
                    path: rel.clone(),
                    edits: 1,
                    last_ts: ts,
                });
                self.files.insert(rel, self.info.files.len() - 1);
            }
        }
        self.info
            .files
            .sort_by_key(|f| std::cmp::Reverse(f.last_ts));
        self.files = self
            .info
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
    }

    fn interrupt(&mut self) {
        self.pending_files.clear();
        let pending: Vec<u64> = self.pending.drain().map(|(_, abs)| abs).collect();
        for abs in pending {
            if let Some(ev) = self.event_mut(abs)
                && ev.state == EventState::Running
            {
                ev.state = EventState::Error;
            }
        }
        for ch in &mut self.info.children {
            ch.running = false;
        }
    }

    fn notice(&mut self, ts: i64, text: &str) {
        self.push(Event {
            ts,
            kind: EventKind::Notice,
            title: text.to_string(),
            detail: None,
            state: EventState::Ok,
            duration_ms: None,
            child: None,
        });
    }

    fn push(&mut self, mut ev: Event) -> u64 {
        ev.title = one_line(&ev.title, TITLE_MAX);
        ev.detail = ev
            .detail
            .as_deref()
            .map(|d| clip(d, DETAIL_MAX))
            .filter(|d| !d.trim().is_empty());
        let abs = self.next_abs;
        self.next_abs += 1;
        self.info.events.push(ev);
        self.abs.push_back(abs);
        while self.info.events.len() > EVENTS_MAX {
            self.info.events.remove(0);
            self.abs.pop_front();
        }
        abs
    }

    fn event_mut(&mut self, abs: u64) -> Option<&mut Event> {
        let front = *self.abs.front()?;
        let idx = abs.checked_sub(front)? as usize;
        self.info.events.get_mut(idx)
    }
}

fn text_blocks(content: Option<&Value>) -> String {
    let Some(blocks) = content.and_then(Value::as_array) else {
        return String::new();
    };
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn detail_if_longer(text: &str, title: &str) -> Option<String> {
    let longer = text.lines().filter(|l| !l.trim().is_empty()).count() > 1
        || text.chars().count() > TITLE_MAX
        || first_line(text, usize::MAX) != title;
    longer.then(|| clip(text, DETAIL_MAX))
}

fn parse_hud(fingerprint: &str) -> Option<Vec<Todo>> {
    let groups: Value = serde_json::from_str(fingerprint).ok()?;
    let mut out = Vec::new();
    for g in groups.as_array()? {
        for t in g
            .get("tasks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(text) = t
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.trim().is_empty())
            else {
                continue;
            };
            out.push(Todo {
                text: one_line(text, TITLE_MAX),
                state: todo_state(t.get("status").and_then(Value::as_str)),
            });
        }
    }
    Some(out)
}

fn todo_state(s: Option<&str>) -> TodoState {
    match s.unwrap_or_default() {
        "completed" | "done" => TodoState::Done,
        "in_progress" | "running" | "active" | "started" => TodoState::Active,
        _ => TodoState::Pending,
    }
}

fn todo_title(todos: &[Todo]) -> String {
    if let Some(t) = todos.iter().find(|t| t.state == TodoState::Active) {
        return t.text.clone();
    }
    let done = todos.iter().filter(|t| t.state == TodoState::Done).count();
    format!("{done}/{} hechas", todos.len())
}

fn classify(name: &str) -> EventKind {
    match name {
        "bash" | "shell" => EventKind::Shell,
        "read" => EventKind::Read,
        "edit" | "write" | "patch" | "multiedit" => EventKind::Edit,
        "grep" | "glob" | "find" | "list" | "search" => EventKind::Search,
        "webfetch" | "websearch" | "web_search" | "fetch" => EventKind::Web,
        "task" | "agent" => EventKind::Task,
        "todo" | "todowrite" | "todoread" => EventKind::Todo,
        _ => EventKind::Tool,
    }
}

fn arg_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
}

fn path_arg(args: &Value) -> Option<&str> {
    arg_str(
        args,
        &["path", "filePath", "file_path", "notebook_path", "filename"],
    )
}

fn describe(
    kind: EventKind,
    name: &str,
    args: &Value,
    intent: Option<&str>,
    cwd: &Path,
) -> (String, Option<String>) {
    let intent = intent.filter(|i| !i.trim().is_empty());
    match kind {
        EventKind::Shell => {
            let cmd = arg_str(args, &["command", "cmd"]).unwrap_or_default();
            let title = intent
                .map(String::from)
                .unwrap_or_else(|| first_line(cmd, TITLE_MAX));
            let detail = (!cmd.is_empty()).then(|| clip(cmd, DETAIL_MAX));
            (title, detail)
        }
        EventKind::Read => (
            path_arg(args)
                .map(|p| rel_path(p, cwd))
                .or_else(|| intent.map(String::from))
                .unwrap_or_else(|| name.to_string()),
            None,
        ),
        EventKind::Edit => {
            let title = path_arg(args)
                .map(|p| rel_path(p, cwd))
                .or_else(|| intent.map(String::from))
                .unwrap_or_else(|| name.to_string());
            let detail = diff_detail(args);
            (title, detail)
        }
        EventKind::Search => {
            let pattern = arg_str(args, &["pattern", "query", "glob"]);
            let where_ = path_arg(args).map(|p| rel_path(p, cwd));
            let title = match (pattern, where_) {
                (Some(p), Some(w)) => format!("{p} · {w}"),
                (Some(p), None) => p.to_string(),
                (None, Some(w)) => w,
                (None, None) => intent.map(String::from).unwrap_or_else(|| name.to_string()),
            };
            (title, None)
        }
        EventKind::Web => {
            let t = arg_str(args, &["query", "url", "q"]).or_else(|| path_arg(args));
            (
                t.map(String::from)
                    .or_else(|| intent.map(String::from))
                    .unwrap_or_else(|| name.to_string()),
                None,
            )
        }
        EventKind::Task => {
            let t = args.get("name").and_then(Value::as_str).or_else(|| {
                args.get("tasks")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|t| t.get("name"))
                    .and_then(Value::as_str)
            });
            let task = args
                .get("tasks")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|t| t.get("task"))
                .and_then(Value::as_str);
            (
                t.or(intent)
                    .map(String::from)
                    .unwrap_or_else(|| "subagente".into()),
                task.map(|x| clip(x, DETAIL_MAX)),
            )
        }
        EventKind::Todo => {
            let t = arg_str(args, &["task"]);
            let title = match (t, arg_str(args, &["op"])) {
                (Some(t), _) => t.to_string(),
                (None, Some(op)) => format!("lista · {op}"),
                (None, None) => "lista de tareas".into(),
            };
            (title, None)
        }
        _ => {
            let arg = arg_str(
                args,
                &[
                    "path", "command", "query", "pattern", "url", "name", "id", "skill",
                ],
            )
            .map(|a| {
                if args.get("path").is_some() {
                    rel_path(a, cwd)
                } else {
                    a.to_string()
                }
            });
            let title = match arg {
                Some(a) => format!("{name} · {a}"),
                None => intent.map(String::from).unwrap_or_else(|| name.to_string()),
            };
            (title, None)
        }
    }
}

fn diff_detail(args: &Value) -> Option<String> {
    let old = args
        .get("old_string")
        .or_else(|| args.get("oldString"))
        .and_then(Value::as_str);
    let new = args
        .get("new_string")
        .or_else(|| args.get("newString"))
        .and_then(Value::as_str);
    match (old, new) {
        (Some(o), Some(n)) => {
            let removed = o.lines().filter(|l| !l.trim().is_empty()).count();
            let added = n.lines().filter(|l| !l.trim().is_empty()).count();
            Some(format!("+{added} −{removed}"))
        }
        (None, Some(n)) => Some(format!(
            "+{}",
            n.lines().filter(|l| !l.trim().is_empty()).count()
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("agentview-omp-{}-{name}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn append(p: &Path, v: &Value) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        writeln!(f, "{v}").unwrap();
    }

    fn open_file(p: &Path) -> Box<dyn Source> {
        open(&SessionRef::Path(p.to_path_buf()), Path::new("/w")).unwrap()
    }

    fn ts(secs: i64) -> String {
        // 2026-01-01T00:00:00Z + secs
        format!("2026-01-01T00:00:{:02}.000Z", secs)
    }

    #[test]
    fn emparejado_incremental_tokens_y_todos() {
        let p = tmp("basic");
        append(
            &p,
            &json!({"type":"session","id":"s1","timestamp":ts(0),"cwd":"/w"}),
        );
        append(&p, &json!({"type":"title","title":"Primera prueba"}));
        append(&p, &json!({"type":"model_change","model":"opencode-go/m1"}));
        append(
            &p,
            &json!({"type":"thinking_level_change","thinkingLevel":"max"}),
        );
        append(
            &p,
            &json!({"type":"message","timestamp":ts(1),"message":{"role":"user","content":[{"type":"text","text":"hola\nmundo"}]}}),
        );
        append(
            &p,
            &json!({"type":"message","timestamp":ts(2),"message":{"role":"assistant","content":[
                {"type":"text","text":"voy"},
                {"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"ls -la","i":"Listar"}}
            ],"usage":{"input":10,"output":3,"cacheRead":5,"cacheWrite":1,"cost":{"total":0.5}}}}),
        );
        append(
            &p,
            &json!({"type":"custom","customType":"tool_execution_start","timestamp":ts(2),"data":{"toolCallId":"c1","startedAt":ts(2),"toolName":"bash","intent":"Listar","args":{"command":"ls -la"}}}),
        );
        append(
            &p,
            &json!({"type":"message","timestamp":ts(3),"message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","isError":false,"content":[{"type":"text","text":"ok"}],"details":{"wallTimeMs":12.5}}}),
        );

        let mut s = open_file(&p);
        assert!(s.refresh().unwrap());
        assert!(!s.refresh().unwrap(), "sin cambios debe devolver false");
        let i = s.info();
        assert_eq!(i.title.as_deref(), Some("Primera prueba"));
        assert_eq!(i.model.as_deref(), Some("opencode-go/m1"));
        assert_eq!(i.effort.as_deref(), Some("max"));
        assert_eq!(i.last_prompt.as_deref(), Some("hola"));
        assert_eq!(i.last_reply.as_deref(), Some("voy"));
        assert_eq!((i.tokens_in, i.tokens_out), (16, 3));
        assert_eq!(i.cost_usd, Some(0.5));
        assert_eq!(i.context_used, Some(16));
        assert_eq!(i.turn_started_at, Some(parse_iso(&ts(1)).unwrap()));
        assert_eq!(i.events.len(), 3);
        assert_eq!(i.events[2].kind, EventKind::Shell);
        assert_eq!(i.events[2].title, "Listar");
        assert_eq!(i.events[2].detail.as_deref(), Some("ls -la"));
        assert_eq!(
            (i.events[2].state, i.events[2].duration_ms),
            (EventState::Ok, Some(12))
        );

        // Nuevo turno: mensaje del usuario, herramienta sin resultado (interrumpida), todos y
        // un `model_usage` auxiliar que no debe sumar tokens.
        append(
            &p,
            &json!({"type":"message","timestamp":ts(4),"message":{"role":"user","content":[{"type":"text","text":"sigue"}]}}),
        );
        append(
            &p,
            &json!({"type":"message","timestamp":ts(5),"message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"edit","arguments":{"path":"/w/a.rs","old_string":"a\nb","new_string":"a\nb\nc"}}]}}),
        );
        append(
            &p,
            &json!({"type":"custom","customType":"todo_hud_state","timestamp":ts(6),"data":{"fingerprint":serde_json::to_string(&json!([
                {"name":"Fase","tasks":[{"content":"hecha","status":"completed"},{"content":"en curso","status":"in_progress"},{"content":"pendiente","status":"pending"}]}
            ])).unwrap()}}),
        );
        append(
            &p,
            &json!({"type":"model_usage","timestamp":ts(7),"purpose":"job-caption","usage":{"input":100,"output":100,"cost":{"total":9.0}}}),
        );
        append(
            &p,
            &json!({"type":"message","timestamp":ts(8),"message":{"role":"user","content":[{"type":"text","text":"otra"}]}}),
        );
        assert!(s.refresh().unwrap());
        let i = s.info();
        assert_eq!(
            (i.tokens_in, i.tokens_out),
            (16, 3),
            "model_usage auxiliar y resultados no suman"
        );
        assert_eq!(i.cost_usd, Some(0.5));
        assert_eq!(i.todos.len(), 3);
        assert_eq!(i.todos[1].state, TodoState::Active);
        assert_eq!(i.events.last().unwrap().kind, EventKind::Prompt);
        // c2 quedó interrumpida por el mensaje del usuario siguiente.
        let interrupted = i.events.iter().find(|e| e.kind == EventKind::Edit).unwrap();
        assert_eq!(interrupted.state, EventState::Error);
        assert_eq!(interrupted.detail.as_deref(), Some("+3 −2"));
        assert!(
            i.files.is_empty(),
            "una edición interrumpida no modifica ficheros"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn resultado_error_y_tarea_con_hijo() {
        let p = tmp("task");
        append(&p, &json!({"type":"session","timestamp":ts(0)}));
        append(
            &p,
            &json!({"type":"message","timestamp":ts(1),"message":{"role":"assistant","content":[
                {"type":"toolCall","id":"t1","name":"task","arguments":{"name":"Scout","i":"Explorar","tasks":[{"agent":"scout","task":"Busca X"}]}}
            ]}}),
        );
        let mut s = open_file(&p);
        s.refresh().unwrap();
        let i = s.info();
        assert_eq!(i.children.len(), 1);
        assert!(i.children[0].running);
        assert_eq!(i.children[0].agent_type.as_deref(), Some("scout"));
        assert_eq!(i.events[0].child.as_deref(), Some("t1"));

        append(
            &p,
            &json!({"type":"message","timestamp":ts(2),"message":{"role":"toolResult","toolCallId":"t1","isError":true,"content":[{"type":"text","text":"falló"}],"details":{"wallTimeMs":7}}}),
        );
        s.refresh().unwrap();
        let i = s.info();
        assert_eq!(i.events[0].state, EventState::Error);
        assert!(!i.children[0].running);
        assert_eq!(i.children[0].last_reply.as_deref(), Some("falló"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tope_de_eventos_y_reset_al_truncar() {
        let p = tmp("cap");
        append(&p, &json!({"type":"session","timestamp":ts(0)}));
        append(
            &p,
            &json!({"type":"message","timestamp":ts(1),"message":{"role":"user","content":[{"type":"text","text":"inicio"}]}}),
        );
        for n in 0..(EVENTS_MAX + 20) {
            append(
                &p,
                &json!({"type":"message","timestamp":ts(2),"message":{"role":"assistant","content":[{"type":"toolCall","id":format!("c{n}"),"name":"read","arguments":{"path":"/w/f"}}]}}),
            );
        }
        let mut s = open_file(&p);
        s.refresh().unwrap();
        let i = s.info();
        assert_eq!(i.events.len(), EVENTS_MAX);
        assert!(
            i.events.iter().all(|e| e.kind == EventKind::Read),
            "los más antiguos se descartan"
        );
        // Un resultado de un evento ya descartado no rompe nada.
        append(
            &p,
            &json!({"type":"message","timestamp":ts(3),"message":{"role":"toolResult","toolCallId":"c0","isError":false,"content":[]}}),
        );
        s.refresh().unwrap();
        assert_eq!(s.info().events.len(), EVENTS_MAX);

        // Fichero truncado: reset y carga nueva.
        std::fs::write(
            &p,
            format!("{}\n", json!({"type":"session","timestamp":ts(5)})),
        )
        .unwrap();
        s.refresh().unwrap();
        assert_eq!(s.info().events.len(), 0);
        assert!(s.info().last_prompt.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn id_requiere_ruta() {
        assert!(open(&SessionRef::Path(tmp("missing")), Path::new("/w")).is_err());
        assert!(open(&SessionRef::Id("abc".into()), Path::new("/w")).is_err());
    }
    #[test]
    fn usage_overflow_wrappers_duplicates_and_empty_truncation() {
        let p = tmp("regressions");
        append(
            &p,
            &json!({"type":"model_usage","purpose":"job-caption","usage":{"input":999}}),
        );
        let message = json!({"type":"message","id":"unique","timestamp":ts(1),"message":{"role":"assistant","usage":{"input":u64::MAX,"cacheRead":1,"output":1},"content":[{"type":"toolCall","id":"write","name":"write","arguments":{"path":"/w/a"}}]}});
        append(&p, &message);
        append(&p, &message);
        append(
            &p,
            &json!({"type":"message","timestamp":ts(2),"message":{"role":"user","content":[{"type":"text","text":"<system-reminder>injected</system-reminder>"}]}}),
        );
        let mut s = open_file(&p);
        s.refresh().unwrap();
        assert_eq!(s.info().tokens_in, u64::MAX);
        assert_eq!(s.info().events.len(), 1);
        assert_eq!(s.info().events[0].state, EventState::Running);
        append(
            &p,
            &json!({"type":"message","timestamp":ts(3),"message":{"role":"toolResult","toolCallId":"write","content":[]}}),
        );
        s.refresh().unwrap();
        assert_eq!(s.info().files[0].path, "a");
        assert_eq!(s.info().events[0].duration_ms, Some(2000));
        std::fs::write(&p, "").unwrap();
        assert!(s.refresh().unwrap());
        assert_eq!(s.info(), &SessionInfo::default());
        std::fs::remove_file(p).unwrap();
    }
}
