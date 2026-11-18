//! Fuente de sesión de OpenCode: su base SQLite (`opencode.db`), en solo lectura.
//!
//! OpenCode ha tenido dos generaciones de tablas y una sesión puede estar en cualquiera:
//! - antigua: `session` + `message` + `part` (el bloque `part.data` trae tipo, herramienta y
//!   estado; el resultado de la herramienta se escribe en la misma fila).
//! - nueva: `session_v2` + `session_message` (una fila por mensaje; los bloques de contenido
//!   —texto y herramientas— van en `data.content`).
//!
//! La base está en uso (WAL) y pesa gigabytes: se abre con `SQLITE_OPEN_READ_ONLY`, sin crear
//! nada, con `busy_timeout` corto, y todas las consultas van por índice (`session_id`,
//! `parent_id`, clave primaria). Los historiales se paginan y los bloques se leen por mensaje.
//! El refresco sin cambios consulta una fila
//! (`time_updated` de la sesión) y `PRAGMA data_version`, dentro de una transacción de lectura.

use super::Source;
use crate::model::{
    CHILD_EVENTS_MAX, Child, DETAIL_MAX, EVENTS_MAX, Event, EventKind, EventState, FileTouch,
    SessionInfo, SessionRef, TITLE_MAX, Todo, TodoState,
};
use crate::util::{clip, first_line, ms_to_s, one_line, rel_path};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Abre la sesión en la base por defecto (`$XDG_DATA_HOME/opencode/opencode.db`).
pub fn open(session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let db = db_path()?;
    open_at(&db, session, cwd)
}

/// Como `open`, con la base explícita (tests).
pub fn open_at(db: &Path, session: &SessionRef, cwd: &Path) -> anyhow::Result<Box<dyn Source>> {
    let id = match session {
        SessionRef::Id(id) => id.clone(),
        SessionRef::Path(p) => anyhow::bail!(
            "opencode necesita el id de sesión, llegó la ruta {}",
            p.display()
        ),
    };
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("no se puede abrir {} en solo lectura", db.display()))?;
    conn.busy_timeout(Duration::from_millis(200))?;
    let generation = if exists(&conn, "select 1 from session_v2 where id=?1", &id)? {
        Gen::New
    } else if exists(&conn, "select 1 from session where id=?1", &id)? {
        Gen::Old
    } else {
        return Err(anyhow!(
            "sesión opencode {id} no encontrada en {}",
            db.display()
        ));
    };
    Ok(Box::new(Opencode {
        conn,
        id,
        cwd: cwd.to_path_buf(),
        generation,
        st: State::default(),
        loaded: false,
        data_version: -1,
    }))
}

fn db_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("opencode").join("opencode.db"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME sin definir"))?;
    Ok(PathBuf::from(home).join(".local/share/opencode/opencode.db"))
}

fn exists(conn: &Connection, sql: &str, id: &str) -> Result<bool> {
    let table = sql.split_whitespace().nth(3).unwrap_or("");
    if conn
        .query_row(
            "select 1 from sqlite_schema where type='table' and name=?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        return Ok(false);
    }
    Ok(conn
        .query_row(sql, params![id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Gen {
    Old,
    New,
}

struct Opencode {
    conn: Connection,
    id: String,
    cwd: PathBuf,
    generation: Gen,
    st: State,
    loaded: bool,
    data_version: i64,
}

impl Source for Opencode {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        // All pages see the same WAL snapshot; this is a read-only transaction.
        self.conn.execute_batch("BEGIN DEFERRED")?;
        let result = self.refresh_inner();
        let end = self.conn.execute_batch("ROLLBACK");
        result.and_then(|changed| {
            end?;
            Ok(changed)
        })
    }

    fn info(&self) -> &SessionInfo {
        &self.st.info
    }
}

// ───────────────────────── estado ─────────────────────────

#[derive(Default)]
struct State {
    info: SessionInfo,
    next_abs: u64,
    abs: VecDeque<u64>,
    /// Llamadas de herramienta visto: id de llamada → evento.
    calls: HashMap<String, u64>,
    /// Bloques de texto ya emitidos: clave estable → evento, para actualizarlos en sitio.
    texts: HashMap<String, u64>,
    reply_key: Option<String>,
    files: HashMap<String, usize>,
    edited: HashSet<String>,
    prompt_key: Option<String>,
    turn_seq: i64,
    context_ts: i64,
    notices: HashSet<String>,
    interrupted: HashSet<String>,
    session_model: bool,
    model_ts: i64,
    last_seen_ms: i64,
    /// Nueva generación: última `seq` leída. Antigua: instantes (ms) ya procesados.
    cursor_seq: i64,
    cursor_part_ms: i64,
    cursor_msg_ms: i64,
    /// Nueva generación: mensajes del asistente a medias (`time.completed` ausente).
    pending_msgs: Vec<String>,
    /// Antigua: `message.id` → rol, para saber si un `part` de texto es del usuario.
    roles: HashMap<String, String>,
}

impl State {
    fn apply(&mut self, row: &SessionRow) {
        if let Some(t) = row.title.as_deref().filter(|t| !t.trim().is_empty()) {
            self.info.title = Some(one_line(t, TITLE_MAX));
        }
        self.session_model = false;
        if let Some(m) = row.model.as_deref() {
            let (model, effort) = parse_model(m);
            if model.is_some() {
                self.session_model = true;
                self.info.model = model;
                self.info.effort = effort;
            }
        }
        self.info.started_at = Some(ms_to_s(row.created_ms));
        self.info.updated_at = Some(ms_to_s(row.updated_ms));
        self.info.tokens_in = row.tokens_in;
        self.info.tokens_out = row.tokens_out;
        self.info.cost_usd = Some(row.cost);
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

    fn interrupt(&mut self) {
        let pending: Vec<_> = self
            .calls
            .iter()
            .map(|(id, abs)| (id.clone(), *abs))
            .collect();
        for (id, abs) in pending {
            if let Some(ev) = self.event_mut(abs)
                && ev.state == EventState::Running
            {
                ev.state = EventState::Error;
                self.interrupted.insert(id);
            }
        }
    }

    fn text_event(&mut self, key: &str, kind: EventKind, text: &str, ts: i64) -> Option<u64> {
        if text.trim().is_empty() {
            return None;
        }
        let ev = text_event_value(kind, text, ts);
        if let Some(&abs) = self.texts.get(key) {
            if let Some(old) = self.event_mut(abs) {
                *old = ev;
            }
            if self.prompt_key.as_deref() == Some(key) {
                self.info.last_prompt = Some(first_line(text, TITLE_MAX));
            }
            if self.reply_key.as_deref() == Some(key) {
                self.info.last_reply = Some(clip(text, DETAIL_MAX));
            }
            return Some(abs);
        }
        let abs = self.push(ev);
        self.texts.insert(key.to_string(), abs);
        if kind == EventKind::Prompt {
            self.prompt_key = Some(key.into());
            self.info.last_prompt = Some(first_line(text, TITLE_MAX));
            self.info.turn_started_at = Some(ts);
        } else if kind == EventKind::Reply {
            self.info.last_reply = Some(clip(text, DETAIL_MAX));
            self.reply_key = Some(key.to_string());
        }
        Some(abs)
    }

    fn tool_event(&mut self, t: &Tool, ts: i64, cwd: &Path) {
        let mut ev = tool_event_value(t, ts, cwd);
        if ev.state == EventState::Running && self.interrupted.contains(&t.call_id) {
            ev.state = EventState::Error;
        }
        if ev.kind == EventKind::Todo && !self.info.todos.is_empty() {
            ev.title = todo_title(&self.info.todos);
        }
        if ev.kind == EventKind::Edit
            && ev.state == EventState::Ok
            && self.edited.insert(t.call_id.clone())
            && let Some(p) = t.path.as_deref()
        {
            self.touch_file(p, cwd, ts);
        }
        if !t.call_id.is_empty()
            && let Some(&abs) = self.calls.get(&t.call_id)
        {
            if let Some(old) = self.event_mut(abs) {
                *old = ev;
            }
            return;
        }
        let abs = self.push(ev);
        if !t.call_id.is_empty() {
            self.calls.insert(t.call_id.clone(), abs);
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
}

// ───────────────────────── filas ─────────────────────────

struct SessionRow {
    title: Option<String>,
    model: Option<String>,
    cost: f64,
    tokens_in: u64,
    tokens_out: u64,
    created_ms: i64,
    updated_ms: i64,
}

fn session_row(conn: &Connection, generation: Gen, id: &str) -> Result<Option<SessionRow>> {
    let table = match generation {
        Gen::New => "session_v2",
        Gen::Old => "session",
    };
    let sql = format!(
        "select title, model, cost, tokens_input, tokens_output, tokens_reasoning,
                tokens_cache_read, tokens_cache_write, time_created, time_updated
         from {table} where id=?1"
    );
    Ok(conn
        .query_row(&sql, params![id], |r| {
            let tokens_in = token_sum(r, &[3, 6, 7])?;
            let tokens_out = token_sum(r, &[4, 5])?;
            Ok(SessionRow {
                title: r.get(0)?,
                model: r.get(1)?,
                cost: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                tokens_in,
                tokens_out,
                created_ms: r.get(8)?,
                updated_ms: r.get(9)?,
            })
        })
        .optional()?)
}

fn token_sum(row: &rusqlite::Row<'_>, columns: &[usize]) -> rusqlite::Result<u64> {
    columns.iter().try_fold(0u64, |sum, &index| {
        Ok(sum.saturating_add(row.get::<_, Option<i64>>(index)?.unwrap_or(0).max(0) as u64))
    })
}

fn parse_model(raw: &str) -> (Option<String>, Option<String>) {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(model)) => (Some(model), None),
        Ok(v) => (
            s(v.get("id")),
            s(v.get("variant")).filter(|x| x != "default"),
        ),
        Err(_) => (Some(raw.to_string()), None),
    }
}

#[derive(Default, Clone)]
struct Msg {
    id: String,
    ty: String,
    seq: i64,
    ts_ms: i64,
    updated_ms: i64,
    text: Option<String>,
    completed: Option<i64>,
    model: Option<String>,
    variant: Option<String>,
    tokens_in: Option<i64>,
    tokens_cache: i64,
    err: Option<String>,
    outcome: Option<String>,
    role: Option<String>,
}

fn read_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Msg> {
    Ok(Msg {
        id: r.get(0)?,
        ty: r.get(1)?,
        seq: r.get(2)?,
        ts_ms: r.get(3)?,
        text: r.get(4)?,
        completed: r.get(5)?,
        model: r.get(6)?,
        variant: r.get(7)?,
        tokens_in: r.get(8)?,
        tokens_cache: r.get(10)?,
        err: r.get(11)?,
        outcome: r.get(12)?,
        role: None,
        updated_ms: 0,
    })
}

#[derive(Default, Clone)]
struct Block {
    msg_id: String,
    idx: i64,
    kind: String,
    text: Option<String>,
    tool: Option<String>,
    call_id: Option<String>,
    status: Option<String>,
    path: Option<String>,
    command: Option<String>,
    pattern: Option<String>,
    query: Option<String>,
    url: Option<String>,
    description: Option<String>,
    prompt: Option<String>,
    child_sid: Option<String>,
    t0: Option<i64>,
    t1: Option<i64>,
    title: Option<String>,
}

#[derive(Default, Clone)]
struct Part {
    id: String,
    msg_id: String,
    ts_ms: i64,
    updated_ms: i64,
    kind: String,
    text: Option<String>,
    tool: Option<String>,
    call_id: Option<String>,
    status: Option<String>,
    path: Option<String>,
    command: Option<String>,
    pattern: Option<String>,
    query: Option<String>,
    url: Option<String>,
    description: Option<String>,
    prompt: Option<String>,
    child_sid: Option<String>,
    t0: Option<i64>,
    t1: Option<i64>,
    title: Option<String>,
}

#[derive(Default)]
struct Tool {
    name: String,
    call_id: String,
    status: String,
    path: Option<String>,
    command: Option<String>,
    pattern: Option<String>,
    query: Option<String>,
    url: Option<String>,
    description: Option<String>,
    prompt: Option<String>,
    child_sid: Option<String>,
    t0: Option<i64>,
    t1: Option<i64>,
    title: Option<String>,
}

fn tool_of_block(b: &Block) -> Tool {
    Tool {
        name: b.tool.clone().unwrap_or_else(|| "tool".into()),
        call_id: b
            .call_id
            .clone()
            .unwrap_or_else(|| format!("{}:{}", b.msg_id, b.idx)),
        status: b.status.clone().unwrap_or_default(),
        path: b.path.clone(),
        command: b.command.clone(),
        pattern: b.pattern.clone(),
        query: b.query.clone(),
        url: b.url.clone(),
        description: b.description.clone(),
        prompt: b.prompt.clone(),
        child_sid: b.child_sid.clone(),
        t0: b.t0,
        t1: b.t1,
        title: b.title.clone(),
    }
}

fn tool_of_part(p: &Part) -> Tool {
    Tool {
        name: p.tool.clone().unwrap_or_else(|| "tool".into()),
        call_id: p.call_id.clone().unwrap_or_else(|| p.id.clone()),
        status: p.status.clone().unwrap_or_default(),
        path: p.path.clone(),
        command: p.command.clone(),
        pattern: p.pattern.clone(),
        query: p.query.clone(),
        url: p.url.clone(),
        description: p.description.clone(),
        prompt: p.prompt.clone(),
        child_sid: p.child_sid.clone(),
        t0: p.t0,
        t1: p.t1,
        title: p.title.clone(),
    }
}

// ───────────────────────── carga: generación nueva ─────────────────────────

const NEW_MSGS_ASC: &str = "select id, type, seq, time_created, substr(json_extract(data,'$.text'),1,4000),
    json_extract(data,'$.time.completed'), json_extract(data,'$.model.id'), json_extract(data,'$.model.variant'),
    json_extract(data,'$.tokens.input'), json_extract(data,'$.tokens.output'),
    coalesce(json_extract(data,'$.tokens.cache.read'),0)+coalesce(json_extract(data,'$.tokens.cache.write'),0),
    json_extract(data,'$.error.message'), json_extract(data,'$.outcome')
  from session_message where session_id=?1 and seq>?2 order by seq limit 600";

impl Opencode {
    fn refresh_inner(&mut self) -> anyhow::Result<bool> {
        let version: i64 = self
            .conn
            .query_row("pragma data_version", [], |r| r.get(0))?;
        let Some(row) = session_row(&self.conn, self.generation, &self.id)? else {
            let changed = self.st.info != SessionInfo::default();
            self.st = State::default();
            self.loaded = false;
            return Ok(changed);
        };
        let changed = row.updated_ms != self.st.last_seen_ms;
        if self.loaded && !changed && self.data_version == version {
            return Ok(false);
        }
        let first = !self.loaded;
        let before = self.st.info.clone();
        self.st.apply(&row);
        self.load_todos()?;
        match self.generation {
            Gen::New => self.load_new(first)?,
            Gen::Old => self.load_old(first)?,
        }
        self.load_children()?;
        self.st.last_seen_ms = row.updated_ms;
        self.data_version = version;
        self.loaded = true;
        Ok(first || changed || before != self.st.info)
    }

    fn load_new(&mut self, first: bool) -> Result<()> {
        if first {
            self.st.cursor_seq = i64::MIN;
        }
        self.recheck_pending()?;
        loop {
            let msgs = self.new_messages()?;
            let full = msgs.len() == 600;
            for m in &msgs {
                let blocks = if m.ty == "assistant" {
                    self.blocks_of(&m.id)?
                } else {
                    Vec::new()
                };
                self.new_message(m, &blocks.iter().collect::<Vec<_>>());
                if m.ty == "assistant"
                    && m.completed.is_none()
                    && !self.st.pending_msgs.contains(&m.id)
                {
                    self.st.pending_msgs.push(m.id.clone());
                }
                self.st.cursor_seq = self.st.cursor_seq.max(m.seq);
            }
            if !full {
                break;
            }
        }
        Ok(())
    }

    fn new_messages(&self) -> Result<Vec<Msg>> {
        let sql = NEW_MSGS_ASC;
        let since = self.st.cursor_seq;
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![self.id, since], read_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn new_message(&mut self, m: &Msg, blocks: &[&Block]) {
        let ts = ms_to_s(m.ts_ms);
        match m.ty.as_str() {
            "user" => {
                if let Some(text) = m.text.as_deref() {
                    let text = super::claude::clean_prompt(text);
                    if !text.is_empty() {
                        self.st.interrupt();
                        self.st.turn_seq = m.seq;
                        self.st
                            .text_event(&format!("p:{}", m.id), EventKind::Prompt, &text, ts);
                    }
                }
            }
            "assistant" => {
                if !self.st.session_model && m.ts_ms >= self.st.model_ts && m.model.is_some() {
                    self.st.info.model = m.model.clone();
                    self.st.info.effort = m.variant.clone().filter(|v| v != "default");
                    self.st.model_ts = m.ts_ms;
                }
                if let Some(v) = m.tokens_in
                    && m.ts_ms >= self.st.context_ts
                {
                    self.st.context_ts = m.ts_ms;
                    let used = (v.max(0) as u64).saturating_add(m.tokens_cache.max(0) as u64);
                    self.st.info.context_used = Some(used);
                }
                if let Some(err) = m.err.as_deref()
                    && self.st.notices.insert(m.id.clone())
                {
                    self.st.push(Event {
                        ts,
                        kind: EventKind::Notice,
                        title: format!("error del modelo · {err}"),
                        detail: None,
                        state: EventState::Error,
                        duration_ms: None,
                        child: None,
                    });
                }
                for b in blocks {
                    match b.kind.as_str() {
                        "text" => {
                            if let Some(text) = b.text.as_deref() {
                                self.st.text_event(
                                    &format!("r:{}:{}", m.id, b.idx),
                                    EventKind::Reply,
                                    text,
                                    ts,
                                );
                            }
                        }
                        "tool" => {
                            let mut t = tool_of_block(b);
                            if m.seq < self.st.turn_seq
                                && matches!(t.status.as_str(), "running" | "pending" | "started")
                            {
                                t.status = "error".into();
                            }
                            self.st.tool_event(&t, ts, &self.cwd);
                        }
                        _ => {}
                    }
                }
            }
            "compaction" => {
                self.st.push(Event {
                    ts,
                    kind: EventKind::Notice,
                    title: "contexto compactado".into(),
                    detail: None,
                    state: EventState::Ok,
                    duration_ms: None,
                    child: None,
                });
            }
            "idle" if m.outcome.as_deref() == Some("failed") => {
                if let Some(err) = m.err.as_deref()
                    && self.st.notices.insert(m.id.clone())
                {
                    self.st.push(Event {
                        ts,
                        kind: EventKind::Notice,
                        title: format!("error del modelo · {err}"),
                        detail: None,
                        state: EventState::Error,
                        duration_ms: None,
                        child: None,
                    });
                }
            }
            _ => {}
        }
    }

    /// Un mensaje a medias se escribió antes de terminar: cuando llega su versión final se
    /// vuelve a leer, y sus bloques se actualizan en sitio (misma clave / mismo id de llamada).
    fn recheck_pending(&mut self) -> Result<()> {
        let pending = self.st.pending_msgs.clone();
        let mut still = Vec::new();
        for id in pending {
            let sql = NEW_MSGS_ASC.replace(
                "where session_id=?1 and seq>?2 order by seq limit 600",
                "where id=?1",
            );
            let msg = self
                .conn
                .query_row(&sql, params![id], read_message)
                .optional()?;
            let Some(msg) = msg else {
                continue;
            };
            if msg.completed.is_none() {
                still.push(id);
            }
            let blocks = self.blocks_of(&msg.id)?;
            self.new_message(&msg, &blocks.iter().collect::<Vec<_>>());
        }
        self.st.pending_msgs = still;
        Ok(())
    }

    fn blocks_of(&self, msg_id: &str) -> Result<Vec<Block>> {
        let mut st = self.conn.prepare(
            "select sm.id, sm.seq, sm.time_created, je.key,
                json_extract(je.value,'$.type'), substr(json_extract(je.value,'$.text'),1,4000),
                json_extract(je.value,'$.name'), json_extract(je.value,'$.id'),
                json_extract(je.value,'$.state.status'), json_extract(je.value,'$.state.input.path'),
                json_extract(je.value,'$.state.input.filePath'), json_extract(je.value,'$.state.input.command'),
                json_extract(je.value,'$.state.input.pattern'), json_extract(je.value,'$.state.input.query'),
                json_extract(je.value,'$.state.input.url'), json_extract(je.value,'$.state.input.description'),
                json_extract(je.value,'$.state.input.prompt'), json_extract(je.value,'$.state.metadata.sessionId'),
                json_extract(je.value,'$.state.time.start'), json_extract(je.value,'$.state.time.end')
              from session_message sm, json_each(sm.data,'$.content') je where sm.id=?1 order by je.key",
        )?;
        let rows = st.query_map(params![msg_id], |r| {
            Ok(Block {
                msg_id: r.get(0)?,
                idx: r.get(3)?,
                kind: r.get(4)?,
                text: r.get(5)?,
                tool: r.get(6)?,
                call_id: r.get(7)?,
                status: r.get(8)?,
                path: r.get::<_, Option<String>>(9)?.or(r.get(10)?),
                command: r.get(11)?,
                pattern: r.get(12)?,
                query: r.get(13)?,
                url: r.get(14)?,
                description: r.get(15)?,
                prompt: r.get(16)?,
                child_sid: r.get(17)?,
                t0: r.get(18)?,
                t1: r.get(19)?,
                title: None,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ───────────────────── carga: generación antigua ─────────────────────

    fn load_old(&mut self, first: bool) -> Result<()> {
        let since = if first {
            i64::MIN
        } else {
            self.st.cursor_msg_ms
        };
        let mut offset = 0;
        loop {
            let msgs = self.old_messages(since, offset)?;
            let full = msgs.len() == 600;
            for m in msgs {
                self.st
                    .roles
                    .insert(m.id.clone(), m.role.clone().unwrap_or_default());
                if let Some(v) = m.tokens_in
                    && m.ts_ms >= self.st.context_ts
                {
                    self.st.context_ts = m.ts_ms;
                    self.st.info.context_used =
                        Some((v.max(0) as u64).saturating_add(m.tokens_cache.max(0) as u64));
                }
                self.st.cursor_msg_ms = self.st.cursor_msg_ms.max(m.updated_ms);
            }
            if !full {
                break;
            }
            offset += 600;
        }
        let since = if first {
            i64::MIN
        } else {
            self.st.cursor_part_ms
        };
        let mut offset = 0;
        let mut parts = Vec::new();
        loop {
            let page = self.old_parts(since, offset)?;
            let full = page.len() == 1200;
            parts.extend(page);
            if !full {
                break;
            }
            offset += 1200;
        }
        parts.sort_by(|a, b| (a.ts_ms, &a.id).cmp(&(b.ts_ms, &b.id)));
        for p in parts {
            let ts = p.t0.map(ms_to_s).unwrap_or_else(|| ms_to_s(p.ts_ms));
            match p.kind.as_str() {
                "text" => {
                    if let Some(text) = p.text.as_deref() {
                        if !self.st.roles.contains_key(&p.msg_id) {
                            let role: Option<String> = self
                                .conn
                                .query_row(
                                    "select json_extract(data,'$.role') from message where id=?1",
                                    [&p.msg_id],
                                    |r| r.get(0),
                                )
                                .optional()?
                                .flatten();
                            if let Some(role) = role {
                                self.st.roles.insert(p.msg_id.clone(), role);
                            }
                        }
                        let role = self
                            .st
                            .roles
                            .get(&p.msg_id)
                            .map(String::as_str)
                            .unwrap_or_default();
                        let key = format!("p:{}", p.id);
                        match role {
                            "user" => {
                                let text = super::claude::clean_prompt(text);
                                if !text.is_empty() {
                                    if !self.st.texts.contains_key(&key) {
                                        self.st.interrupt();
                                    }
                                    self.st.text_event(&key, EventKind::Prompt, &text, ts);
                                }
                            }
                            "assistant" => {
                                self.st.text_event(&key, EventKind::Reply, text, ts);
                            }
                            _ => {}
                        }
                    }
                }
                "tool" => {
                    self.st.tool_event(&tool_of_part(&p), ts, &self.cwd);
                }
                _ => {}
            }
            self.st.cursor_part_ms = self.st.cursor_part_ms.max(p.updated_ms);
        }
        Ok(())
    }

    fn old_messages(&self, since: i64, offset: usize) -> Result<Vec<Msg>> {
        let sql = "select id, json_extract(data,'$.role'), time_created,
                json_extract(data,'$.tokens.input'),
                coalesce(json_extract(data,'$.tokens.cache.read'),0)+coalesce(json_extract(data,'$.tokens.cache.write'),0),
                json_extract(data,'$.time.completed'), time_updated
             from message where session_id=?1 and time_updated>=?2 order by time_updated,id limit 600 offset ?3";
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![self.id, since, offset as i64], |r| {
            Ok(Msg {
                id: r.get(0)?,
                role: r.get(1)?,
                ts_ms: r.get(2)?,
                tokens_in: r.get(3)?,
                tokens_cache: r.get(4)?,
                completed: r.get(5)?,
                updated_ms: r.get(6)?,
                ..Msg::default()
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn old_parts(&self, since: i64, offset: usize) -> Result<Vec<Part>> {
        let sql = "select p.id, p.message_id, p.time_created,
                json_extract(p.data,'$.type'), substr(json_extract(p.data,'$.text'),1,4000),
                json_extract(p.data,'$.tool'), json_extract(p.data,'$.callID'),
                json_extract(p.data,'$.state.status'), json_extract(p.data,'$.state.input.path'),
                json_extract(p.data,'$.state.input.filePath'), json_extract(p.data,'$.state.input.command'),
                json_extract(p.data,'$.state.input.pattern'), json_extract(p.data,'$.state.input.query'),
                json_extract(p.data,'$.state.input.url'), json_extract(p.data,'$.state.input.description'),
                json_extract(p.data,'$.state.input.prompt'), json_extract(p.data,'$.state.metadata.sessionId'),
                json_extract(p.data,'$.state.time.start'), json_extract(p.data,'$.state.time.end'),
                json_extract(p.data,'$.state.title'), p.time_updated
             from part p where p.session_id=?1 and p.time_updated>=?2 order by p.time_updated,p.id limit 1200 offset ?3";
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![self.id, since, offset as i64], |r| {
            Ok(Part {
                id: r.get(0)?,
                msg_id: r.get(1)?,
                ts_ms: r.get(2)?,
                kind: r.get(3)?,
                text: r.get(4)?,
                tool: r.get(5)?,
                call_id: r.get(6)?,
                status: r.get(7)?,
                path: r.get::<_, Option<String>>(8)?.or(r.get(9)?),
                command: r.get(10)?,
                pattern: r.get(11)?,
                query: r.get(12)?,
                url: r.get(13)?,
                description: r.get(14)?,
                prompt: r.get(15)?,
                child_sid: r.get(16)?,
                t0: r.get(17)?,
                t1: r.get(18)?,
                title: r.get(19)?,
                updated_ms: r.get(20)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ───────────────────── todos e hijos ─────────────────────

    fn load_todos(&mut self) -> Result<()> {
        let mut st = self.conn.prepare("select content, status from todo where session_id=?1 order by position limit 200 offset ?2")?;
        let mut todos = Vec::new();
        let mut offset = 0;
        loop {
            let page = st
                .query_map(params![self.id, offset], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let full = page.len() == 200;
            for (text, status) in page {
                if !text.trim().is_empty() {
                    todos.push(Todo {
                        text: one_line(&text, TITLE_MAX),
                        state: todo_state(&status),
                    });
                }
            }
            if !full {
                break;
            }
            offset += 200;
        }
        self.st.info.todos = todos;
        Ok(())
    }

    fn load_children(&mut self) -> Result<()> {
        let table = match self.generation {
            Gen::New => "session_v2",
            Gen::Old => "session",
        };
        let sql = format!(
            "select id, title, model, cost, tokens_input, tokens_output, tokens_reasoning,
                    tokens_cache_read, tokens_cache_write, time_created, time_updated, agent
             from {table} where parent_id=?1 order by time_created,id limit 50 offset ?2"
        );
        let mut st = self.conn.prepare(&sql)?;
        let mut all = Vec::new();
        let mut offset = 0;
        loop {
            let rows = st.query_map(params![self.id, offset], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    SessionRow {
                        title: r.get(1)?,
                        model: r.get(2)?,
                        cost: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                        tokens_in: token_sum(r, &[4, 7, 8])?,
                        tokens_out: token_sum(r, &[5, 6])?,
                        created_ms: r.get(9)?,
                        updated_ms: r.get(10)?,
                    },
                    r.get::<_, Option<String>>(11)?,
                ))
            })?;
            let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            let full = rows.len() == 50;
            all.extend(rows);
            if !full {
                break;
            }
            offset += 50;
        }
        let mut out = Vec::new();
        for (id, row, agent) in all {
            let child = self.build_child(&id, &row, agent.as_deref());
            out.push(child);
        }
        self.st.info.children = out;
        Ok(())
    }

    fn build_child(&self, id: &str, row: &SessionRow, agent: Option<&str>) -> Child {
        match self.generation {
            Gen::New => self.child_new(id, row, agent),
            Gen::Old => self.child_old(id, row, agent),
        }
    }

    fn child_new(&self, id: &str, row: &SessionRow, agent: Option<&str>) -> Child {
        let mut events = Vec::new();
        let mut last_reply = None;
        let mut model = None;
        let mut msgs = Vec::new();
        if let Ok(mut st) = self.conn.prepare(
            "select id, type, seq, time_created, substr(json_extract(data,'$.text'),1,4000), json_extract(data,'$.model.id'), json_extract(data,'$.time.completed')
             from session_message where session_id=?1 order by seq desc limit 120",
        ) && let Ok(rows) = st.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        }) {
            msgs = rows.filter_map(Result::ok).collect();
        }
        msgs.reverse();
        let running = msgs
            .iter()
            .rev()
            .find(|m| matches!(m.1.as_str(), "user" | "assistant" | "idle"))
            .is_some_and(|m| m.1 == "user" || (m.1 == "assistant" && m.6.is_none()));
        for (mid, ty, _seq, ts_ms, text, mdl, _completed) in &msgs {
            if mdl.is_some() {
                model = mdl.clone();
            }
            let ts = ms_to_s(*ts_ms);
            match ty.as_str() {
                "user" => {
                    if let Some(t) = text.as_deref() {
                        push_capped(&mut events, text_event_value(EventKind::Prompt, t, ts));
                    }
                }
                "assistant" => {
                    let blocks = self.blocks_of(mid).unwrap_or_default();
                    for b in &blocks {
                        match b.kind.as_str() {
                            "text" => {
                                if let Some(t) = b.text.as_deref() {
                                    last_reply = Some(clip(t, DETAIL_MAX));
                                    push_capped(
                                        &mut events,
                                        text_event_value(EventKind::Reply, t, ts),
                                    );
                                }
                            }
                            "tool" => push_capped(
                                &mut events,
                                tool_event_value(&tool_of_block(b), ts, &self.cwd),
                            ),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Child {
            id: id.to_string(),
            name: row
                .title
                .as_deref()
                .map(|t| one_line(t, TITLE_MAX))
                .unwrap_or_else(|| "subagente".into()),
            agent_type: agent.map(String::from),
            model,
            running,
            started_at: Some(ms_to_s(row.created_ms)),
            updated_at: Some(ms_to_s(row.updated_ms)),
            tokens_in: row.tokens_in,
            tokens_out: row.tokens_out,
            events,
            last_reply,
        }
    }

    fn child_old(&self, id: &str, row: &SessionRow, agent: Option<&str>) -> Child {
        let mut events = Vec::new();
        let mut last_reply = None;
        let mut roles: HashMap<String, String> = HashMap::new();
        let mut last: Option<(String, Option<i64>)> = None;
        if let Ok(mut st) = self.conn.prepare(
            "select id, json_extract(data,'$.role'), json_extract(data,'$.time.completed')
             from message where session_id=?1 order by time_created desc limit 120",
        ) && let Ok(rows) = st.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        }) {
            for row in rows.filter_map(Result::ok) {
                roles.insert(row.0.clone(), row.1.clone().unwrap_or_default());
                last.get_or_insert((row.1.unwrap_or_default(), row.2));
            }
        }
        let running = match last {
            Some((role, completed)) => {
                role == "user" || (role == "assistant" && completed.is_none())
            }
            None => false,
        };
        let mut parts = Vec::new();
        if let Ok(mut st) = self.conn.prepare(
            "select p.id, p.message_id, p.time_created,
                json_extract(p.data,'$.type'), substr(json_extract(p.data,'$.text'),1,4000),
                json_extract(p.data,'$.tool'), json_extract(p.data,'$.callID'),
                json_extract(p.data,'$.state.status'), json_extract(p.data,'$.state.input.path'),
                json_extract(p.data,'$.state.input.filePath'), json_extract(p.data,'$.state.input.command'),
                json_extract(p.data,'$.state.input.pattern'), json_extract(p.data,'$.state.input.query'),
                json_extract(p.data,'$.state.input.url'), json_extract(p.data,'$.state.input.description'),
                json_extract(p.data,'$.state.input.prompt'), json_extract(p.data,'$.state.metadata.sessionId'),
                json_extract(p.data,'$.state.time.start'), json_extract(p.data,'$.state.time.end'),
                json_extract(p.data,'$.state.title')
             from part p where p.session_id=?1 order by p.time_updated desc limit 400",
        ) && let Ok(rows) = st.query_map(params![id], |r| {
            Ok(Part {
                id: r.get(0)?,
                msg_id: r.get(1)?,
                ts_ms: r.get(2)?,
                kind: r.get(3)?,
                text: r.get(4)?,
                tool: r.get(5)?,
                call_id: r.get(6)?,
                status: r.get(7)?,
                path: r.get::<_, Option<String>>(8)?.or(r.get(9)?),
                command: r.get(10)?,
                pattern: r.get(11)?,
                query: r.get(12)?,
                url: r.get(13)?,
                description: r.get(14)?,
                prompt: r.get(15)?,
                child_sid: r.get(16)?,
                t0: r.get(17)?,
                t1: r.get(18)?,
                title: r.get(19)?,
                updated_ms: 0,
            })
        }) {
            parts = rows.filter_map(Result::ok).collect();
        }
        parts.sort_by(|a, b| (a.ts_ms, &a.id).cmp(&(b.ts_ms, &b.id)));
        for p in &parts {
            let ts = p.t0.map(ms_to_s).unwrap_or_else(|| ms_to_s(p.ts_ms));
            match p.kind.as_str() {
                "text" => {
                    if let Some(t) = p.text.as_deref().filter(|t| !t.trim().is_empty()) {
                        match roles.get(&p.msg_id).map(String::as_str).unwrap_or_default() {
                            "user" => {
                                push_capped(&mut events, text_event_value(EventKind::Prompt, t, ts))
                            }
                            "assistant" => {
                                last_reply = Some(clip(t, DETAIL_MAX));
                                push_capped(&mut events, text_event_value(EventKind::Reply, t, ts));
                            }
                            _ => {}
                        }
                    }
                }
                "tool" => push_capped(
                    &mut events,
                    tool_event_value(&tool_of_part(p), ts, &self.cwd),
                ),
                _ => {}
            }
        }
        Child {
            id: id.to_string(),
            name: row
                .title
                .as_deref()
                .map(|t| one_line(t, TITLE_MAX))
                .unwrap_or_else(|| "subagente".into()),
            agent_type: agent.map(String::from),
            model: row.model.as_deref().and_then(|m| parse_model(m).0),
            running,
            started_at: Some(ms_to_s(row.created_ms)),
            updated_at: Some(ms_to_s(row.updated_ms)),
            tokens_in: row.tokens_in,
            tokens_out: row.tokens_out,
            events,
            last_reply,
        }
    }
}

// ───────────────────────── vocabulario de eventos ─────────────────────────

fn todo_title(todos: &[Todo]) -> String {
    if let Some(t) = todos.iter().find(|t| t.state == TodoState::Active) {
        return t.text.clone();
    }
    let done = todos.iter().filter(|t| t.state == TodoState::Done).count();
    format!("{done}/{} hechas", todos.len())
}

fn todo_state(status: &str) -> TodoState {
    match status {
        "completed" | "done" => TodoState::Done,
        "in_progress" | "running" | "active" | "started" => TodoState::Active,
        _ => TodoState::Pending,
    }
}

fn classify(name: &str) -> EventKind {
    match name {
        "bash" | "shell" | "execute" => EventKind::Shell,
        "read" => EventKind::Read,
        "edit" | "write" | "patch" | "multiedit" => EventKind::Edit,
        "grep" | "glob" | "list" | "find" | "search" => EventKind::Search,
        "webfetch" | "websearch" | "fetch" | "web_search" => EventKind::Web,
        "task" | "agent" => EventKind::Task,
        "todowrite" | "todoread" | "todo" => EventKind::Todo,
        _ => EventKind::Tool,
    }
}

fn describe(kind: EventKind, t: &Tool, cwd: &Path) -> (String, Option<String>) {
    let name = t.name.as_str();
    match kind {
        EventKind::Shell => {
            let cmd = t.command.as_deref().unwrap_or_default();
            let title = t
                .description
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| first_line(cmd, TITLE_MAX));
            (title, (!cmd.is_empty()).then(|| clip(cmd, DETAIL_MAX)))
        }
        EventKind::Read => (
            t.path
                .as_deref()
                .map(|p| rel_path(p, cwd))
                .or_else(|| t.title.clone())
                .unwrap_or_else(|| name.to_string()),
            None,
        ),
        EventKind::Edit => (
            t.path
                .as_deref()
                .map(|p| rel_path(p, cwd))
                .or_else(|| t.title.clone())
                .unwrap_or_else(|| name.to_string()),
            None,
        ),
        EventKind::Search => {
            let where_ = t.path.as_deref().map(|p| rel_path(p, cwd));
            let title = match (t.pattern.as_deref(), where_) {
                (Some(p), Some(w)) => format!("{p} · {w}"),
                (Some(p), None) => p.to_string(),
                (None, Some(w)) => w,
                (None, None) => t.title.clone().unwrap_or_else(|| name.to_string()),
            };
            (title, None)
        }
        EventKind::Web => (
            t.query
                .clone()
                .or_else(|| t.url.clone())
                .or_else(|| t.path.clone())
                .unwrap_or_else(|| name.to_string()),
            None,
        ),
        EventKind::Task => (
            t.description
                .clone()
                .or_else(|| t.title.clone())
                .unwrap_or_else(|| "subagente".into()),
            t.prompt.clone().map(|p| clip(&p, DETAIL_MAX)),
        ),
        EventKind::Todo => (
            t.title.clone().unwrap_or_else(|| "lista de tareas".into()),
            None,
        ),
        _ => {
            let arg = t
                .path
                .as_deref()
                .or(t.query.as_deref())
                .or(t.pattern.as_deref())
                .or(t.title.as_deref());
            let title = match arg {
                Some(a) if t.path.is_some() => format!("{name} · {}", rel_path(a, cwd)),
                Some(a) => format!("{name} · {a}"),
                None => name.to_string(),
            };
            (title, None)
        }
    }
}

fn push_capped(events: &mut Vec<Event>, ev: Event) {
    events.push(ev);
    while events.len() > CHILD_EVENTS_MAX {
        events.remove(0);
    }
}

fn text_event_value(kind: EventKind, text: &str, ts: i64) -> Event {
    let title = first_line(text, TITLE_MAX);
    let detail = (text.lines().filter(|l| !l.trim().is_empty()).count() > 1
        || text.chars().count() > TITLE_MAX
        || first_line(text, usize::MAX) != title)
        .then(|| clip(text, DETAIL_MAX));
    Event {
        ts,
        kind,
        title,
        detail,
        state: EventState::Ok,
        duration_ms: None,
        child: None,
    }
}

fn tool_event_value(t: &Tool, ts: i64, cwd: &Path) -> Event {
    let kind = classify(&t.name);
    let (title, detail) = describe(kind, t, cwd);
    let state = match t.status.as_str() {
        "error" | "failed" => EventState::Error,
        "running" | "pending" | "started" => EventState::Running,
        _ => EventState::Ok,
    };
    let duration_ms = match (t.t0, t.t1) {
        (Some(a), Some(b)) if b >= a => Some(b.saturating_sub(a) as u64),
        _ => None,
    };
    Event {
        ts,
        kind,
        title: one_line(&title, TITLE_MAX),
        detail: detail.map(|d| clip(&d, DETAIL_MAX)),
        state,
        duration_ms,
        child: if kind == EventKind::Task {
            t.child_sid.clone()
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection as Db;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "agentview-opencode-{}-{name}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn new_db(p: &Path) {
        let c = Db::open(p).unwrap();
        c.execute_batch(
            "create table session_v2 (id text primary key, parent_id text, title text, model text,
                cost real default 0, tokens_input integer default 0, tokens_output integer default 0,
                tokens_reasoning integer default 0, tokens_cache_read integer default 0,
                tokens_cache_write integer default 0, time_created integer, time_updated integer, agent text);
             create table session_message (id text primary key, session_id text, type text, seq integer,
                time_created integer, time_updated integer, data text);
             create table todo (session_id text, content text, status text, priority text, position integer,
                time_created integer, time_updated integer, primary key(session_id, position));",
        )
        .unwrap();
    }

    fn old_db(p: &Path) {
        let c = Db::open(p).unwrap();
        c.execute_batch(
            "create table session (id text primary key, parent_id text, title text, model text,
                cost real default 0, tokens_input integer default 0, tokens_output integer default 0,
                tokens_reasoning integer default 0, tokens_cache_read integer default 0,
                tokens_cache_write integer default 0, time_created integer, time_updated integer, agent text);
             create table message (id text primary key, session_id text, time_created integer, time_updated integer, data text);
             create table part (id text primary key, message_id text, session_id text, time_created integer, time_updated integer, data text);
             create table todo (session_id text, content text, status text, priority text, position integer,
                time_created integer, time_updated integer, primary key(session_id, position));
",
        )
        .unwrap();
    }

    fn indexes(p: &Path) {
        Db::open(p).unwrap().execute_batch("create index session_message_session_seq_idx on session_message(session_id,seq);create index session_message_session_type_seq_idx on session_message(session_id,type,seq);create index session_v2_parent_idx on session_v2(parent_id);").unwrap();
    }

    fn put_session(p: &Path, table: &str, id: &str, title: &str, model: &str, updated: i64) {
        let c = Db::open(p).unwrap();
        c.execute(
            &format!("insert into {table} (id,title,model,cost,tokens_input,tokens_output,tokens_reasoning,tokens_cache_read,tokens_cache_write,time_created,time_updated) values (?1,?2,?3,1.5,100,20,5,7,3,1000,?4)"),
            params![id, title, model, updated],
        )
        .unwrap();
    }

    fn put_msg(p: &Path, id: &str, session: &str, ty: &str, seq: i64, ts: i64, data: &str) {
        let c = Db::open(p).unwrap();
        c.execute(
            "insert into session_message (id,session_id,type,seq,time_created,time_updated,data) values (?1,?2,?3,?4,?5,?5,?6)",
            params![id, session, ty, seq, ts, data],
        )
        .unwrap();
    }

    fn sref(id: &str) -> SessionRef {
        SessionRef::Id(id.to_string())
    }

    fn open_db(p: &Path, id: &str) -> Box<dyn Source> {
        open_at(p, &sref(id), Path::new("/w")).unwrap()
    }

    #[test]
    fn nueva_emparejado_incremental_todos_e_hijos() {
        let p = tmp("new");
        new_db(&p);
        put_session(
            &p,
            "session_v2",
            "ses_a",
            "Título",
            r#"{"id":"m1","providerID":"x","variant":"max"}"#,
            1000,
        );
        put_msg(
            &p,
            "m1",
            "ses_a",
            "user",
            1,
            1000,
            r#"{"time":{"created":1000},"text":"hola\nmundo","files":[],"agents":[]}"#,
        );
        put_msg(
            &p,
            "m2",
            "ses_a",
            "assistant",
            2,
            2000,
            r#"{"time":{"created":2000,"completed":2500},"model":{"id":"m1","variant":"max"},"tokens":{"input":10,"output":2,"cache":{"read":5,"write":1}},"content":[{"type":"text","text":"voy"},{"type":"tool","id":"c1","name":"shell","state":{"status":"completed","input":{"command":"ls -la"},"metadata":{"exit":0}}},{"type":"tool","id":"c2","name":"edit","state":{"status":"error","input":{"path":"/w/a.rs","oldString":"a","newString":"b"},"error":{"type":"tool.execution","message":"nope"}}}]}"#,
        );
        put_msg(
            &p,
            "m3",
            "ses_a",
            "user",
            3,
            3000,
            r#"{"time":{"created":3000},"text":"sigue","files":[],"agents":[]}"#,
        );
        let c = Db::open(&p).unwrap();
        c.execute(
            "insert into todo (session_id,content,status,priority,position,time_created,time_updated) values ('ses_a','en curso','in_progress','high',0,1,1),('ses_a','hecha','completed','high',1,1,1)",
            [],
        )
        .unwrap();
        // Hijo en curso: solo mensaje del usuario, sin `idle`.
        c.execute(
            "insert into session_v2 (id,parent_id,title,model,cost,tokens_input,tokens_output,time_created,time_updated) values ('ses_child','ses_a','Tarea hija','{\"id\":\"m1\"}',0,1,1,1000,4000)",
            [],
        )
        .unwrap();
        put_msg(
            &p,
            "mc1",
            "ses_child",
            "user",
            0,
            4000,
            r#"{"time":{"created":4000},"text":"haz esto"}"#,
        );
        drop(c);

        let mut s = open_db(&p, "ses_a");
        assert!(s.refresh().unwrap());
        assert!(!s.refresh().unwrap(), "sin cambios: una fila");
        let i = s.info();
        assert_eq!(i.title.as_deref(), Some("Título"));
        assert_eq!(
            (i.model.as_deref(), i.effort.as_deref()),
            (Some("m1"), Some("max"))
        );
        assert_eq!(i.last_prompt.as_deref(), Some("sigue"));
        assert_eq!(i.last_reply.as_deref(), Some("voy"));
        assert_eq!((i.tokens_in, i.tokens_out), (110, 25));
        assert_eq!(i.cost_usd, Some(1.5));
        assert_eq!(i.context_used, Some(16));
        assert_eq!(i.todos.len(), 2);
        assert_eq!(i.todos[0].state, TodoState::Active);
        let shell = i
            .events
            .iter()
            .find(|e| e.kind == EventKind::Shell)
            .unwrap();
        assert_eq!(
            (shell.title.as_str(), shell.detail.as_deref()),
            ("ls -la", Some("ls -la"))
        );
        assert_eq!(shell.state, EventState::Ok);
        let edit = i.events.iter().find(|e| e.kind == EventKind::Edit).unwrap();
        assert_eq!(edit.state, EventState::Error);
        assert_eq!(edit.title, "a.rs");
        assert!(i.files.is_empty());
        assert_eq!(i.children.len(), 1);
        assert_eq!(i.children[0].id, "ses_child");
        assert_eq!(i.children[0].name, "Tarea hija");
        assert!(i.children[0].running);
        assert_eq!(i.children[0].events[0].kind, EventKind::Prompt);

        // Nuevo turno: una herramienta a medias y luego otra fila con el resultado.
        put_msg(
            &p,
            "m4",
            "ses_a",
            "assistant",
            4,
            5000,
            r#"{"time":{"created":5000},"content":[{"type":"tool","id":"c3","name":"read","state":{"status":"running","input":{"path":"/w/b.rs"}}}]}"#,
        );
        let c = Db::open(&p).unwrap();
        c.execute(
            "update session_v2 set time_updated=5000 where id='ses_a'",
            [],
        )
        .unwrap();
        drop(c);
        assert!(s.refresh().unwrap());
        let i = s.info();
        assert_eq!(i.events.last().unwrap().state, EventState::Running);
        assert_eq!(i.events.last().unwrap().title, "b.rs");

        let c = Db::open(&p).unwrap();
        c.execute(
            "update session_message set time_updated=6000, data=?1 where id='m4'",
            params![r#"{"time":{"created":5000,"completed":6000},"content":[{"type":"tool","id":"c3","name":"read","state":{"status":"completed","input":{"path":"/w/b.rs"},"content":[{"type":"text","text":"contenido"}]}}]}"#],
        )
        .unwrap();
        c.execute(
            "update session_v2 set time_updated=6000 where id='ses_a'",
            [],
        )
        .unwrap();
        drop(c);
        assert!(s.refresh().unwrap());
        let i = s.info();
        let ev = i.events.iter().find(|e| e.kind == EventKind::Read).unwrap();
        assert_eq!(ev.state, EventState::Ok);
        assert_eq!(
            i.events
                .iter()
                .filter(|e| e.kind == EventKind::Read)
                .count(),
            1,
            "sin duplicar"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn antigua_emparejado_y_humano() {
        let p = tmp("old");
        old_db(&p);
        put_session(
            &p,
            "session",
            "ses_o",
            "Vieja",
            r#"{"id":"m9","variant":"high"}"#,
            5000,
        );
        let c = Db::open(&p).unwrap();
        c.execute(
            "insert into message (id,session_id,time_created,time_updated,data) values
             ('u1','ses_o',1000,1000,'{\"role\":\"user\",\"time\":{\"created\":1000}}'),
             ('a1','ses_o',2000,2000,'{\"role\":\"assistant\",\"time\":{\"created\":2000}}')",
            [],
        )
        .unwrap();
        c.execute(
            "insert into part (id,message_id,session_id,time_created,time_updated,data) values
             ('t1','u1','ses_o',1100,1100,'{\"type\":\"text\",\"text\":\"hazlo\"}'),
             ('t2','a1','ses_o',2100,2100,'{\"type\":\"text\",\"text\":\"hecho\"}'),
             ('x1','a1','ses_o',2200,2200,'{\"type\":\"tool\",\"callID\":\"b1\",\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"echo ok\",\"description\":\"Probar\"},\"time\":{\"start\":2200,\"end\":2250}}}'),
             ('x2','a1','ses_o',2300,2300,'{\"type\":\"tool\",\"callID\":\"b2\",\"tool\":\"write\",\"state\":{\"status\":\"error\",\"input\":{\"filePath\":\"/w/c.rs\",\"content\":\"x\"},\"time\":{\"start\":2300,\"end\":2301}}}'),
             ('x3','a1','ses_o',2400,2400,'{\"type\":\"tool\",\"callID\":\"b3\",\"tool\":\"task\",\"state\":{\"status\":\"completed\",\"input\":{\"description\":\"Explorar\",\"prompt\":\"mira\"},\"metadata\":{\"sessionId\":\"ses_sub\"}}}')",
            [],
        )
        .unwrap();
        drop(c);

        let mut s = open_db(&p, "ses_o");
        assert!(s.refresh().unwrap());
        assert!(!s.refresh().unwrap());
        let i = s.info();
        assert_eq!(i.last_prompt.as_deref(), Some("hazlo"));
        assert_eq!(i.last_reply.as_deref(), Some("hecho"));
        assert_eq!(i.events.len(), 5);
        let task = i.events.iter().find(|e| e.kind == EventKind::Task).unwrap();
        assert_eq!(task.child.as_deref(), Some("ses_sub"));
        assert_eq!(
            (task.title.as_str(), task.detail.as_deref()),
            ("Explorar", Some("mira"))
        );
        let bash = i
            .events
            .iter()
            .find(|e| e.kind == EventKind::Shell)
            .unwrap();
        assert_eq!(
            (bash.title.as_str(), bash.duration_ms),
            ("Probar", Some(50))
        );
        assert_eq!(bash.state, EventState::Ok);
        let w = i.events.iter().find(|e| e.kind == EventKind::Edit).unwrap();
        assert_eq!((w.title.as_str(), w.state), ("c.rs", EventState::Error));
        assert!(i.files.is_empty());

        // Incremental: mensaje nuevo y su `part` de texto del usuario.
        let c = Db::open(&p).unwrap();
        c.execute(
            "insert into message (id,session_id,time_created,time_updated,data) values
             ('u2','ses_o',3000,3000,'{\"role\":\"user\",\"time\":{\"created\":3000}}')",
            [],
        )
        .unwrap();
        c.execute(
            "insert into part (id,message_id,session_id,time_created,time_updated,data) values
             ('t3','u2','ses_o',3100,3100,'{\"type\":\"text\",\"text\":\"sigue\"}')",
            [],
        )
        .unwrap();
        c.execute("update session set time_updated=3100 where id='ses_o'", [])
            .unwrap();
        drop(c);
        assert!(s.refresh().unwrap());
        assert_eq!(s.info().last_prompt.as_deref(), Some("sigue"));
        assert!(!s.refresh().unwrap());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tope_de_eventos() {
        let p = tmp("cap");
        new_db(&p);
        put_session(&p, "session_v2", "ses_c", "Cap", "{}", 1000);
        let c = Db::open(&p).unwrap();
        for n in 0..(EVENTS_MAX + 20) {
            put_msg(
                &p,
                &format!("m{n}"),
                "ses_c",
                "assistant",
                n as i64,
                1000 + n as i64,
                &format!(
                    r#"{{"time":{{"created":{}}},"content":[{{"type":"tool","id":"c{n}","name":"read","state":{{"status":"completed","input":{{"path":"/w/f"}}}}}}]}}"#,
                    1000 + n
                ),
            );
        }
        drop(c);
        let mut s = open_db(&p, "ses_c");
        s.refresh().unwrap();
        let i = s.info();
        assert_eq!(i.events.len(), EVENTS_MAX);
        assert!(i.events.iter().all(|e| e.kind == EventKind::Read));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn ruta_no_vale_y_sesion_inexistente() {
        let p = tmp("err");
        new_db(&p);
        assert!(open_at(&p, &SessionRef::Path(PathBuf::from("/x")), Path::new("/w")).is_err());
        assert!(open_at(&p, &sref("nope"), Path::new("/w")).is_err());
        let _ = std::fs::remove_file(&p);
    }
    #[test]
    fn new_pages_zero_sequence_and_multiple_text_blocks() {
        let p = tmp("pages");
        new_db(&p);
        indexes(&p);
        put_session(&p, "session_v2", "s", "title", "{}", 1);
        put_msg(&p, "u", "s", "user", 0, 1, r#"{"text":"first prompt"}"#);
        let mut c = Db::open(&p).unwrap();
        let tx = c.transaction().unwrap();
        for n in 1..=650 {
            let data = serde_json::json!({"time":{"completed":1},"content":[{"type":"tool","id":format!("edit-{n}"),"name":"write","state":{"status":"completed","input":{"path":format!("/w/f-{n}")}}},{"type":"text","text":format!("first-{n}")},{"type":"text","text":format!("last-{n}")}]});
            tx.execute(
                "insert into session_message values (?1,'s','assistant',?2,?2,?2,?3)",
                params![format!("m-{n}"), n, data.to_string()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        drop(c);
        let before = std::fs::read(&p).unwrap();
        let mut src = open_db(&p, "s");
        assert!(src.refresh().unwrap());
        assert_eq!(src.info().files.len(), 650);
        assert_eq!(src.info().events.len(), EVENTS_MAX);
        assert_eq!(src.info().last_prompt.as_deref(), Some("first prompt"));
        assert_eq!(src.info().last_reply.as_deref(), Some("last-650"));
        assert!(!src.refresh().unwrap());
        assert_eq!(before, std::fs::read(&p).unwrap());
        let c = Db::open(&p).unwrap();
        let mut stmt = c
            .prepare(&format!("explain query plan {NEW_MSGS_ASC}"))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params!["s", 0], |r| r.get(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            plan.iter()
                .any(|s| s.contains("SEARCH session_message USING INDEX")),
            "{plan:?}"
        );
        drop(stmt);
        drop(c);
        drop(src);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn streaming_without_parent_timestamp_updates_and_child_completion() {
        let p = tmp("stream");
        new_db(&p);
        put_session(&p, "session_v2", "s", "title", "{}", 1);
        put_msg(
            &p,
            "a",
            "s",
            "assistant",
            1,
            1000,
            r#"{"tokens":{"input":2},"content":[{"type":"tool","id":"edit","name":"write","state":{"status":"running","input":{"path":"/w/a"}}}]}"#,
        );
        let mut src = open_db(&p, "s");
        src.refresh().unwrap();
        assert_eq!(src.info().events[0].state, EventState::Running);
        assert!(src.info().files.is_empty());
        let c = Db::open(&p).unwrap();
        c.execute("update session_message set data=?1 where id='a'",[r#"{"model":{"id":"stream-model"},"tokens":{"input":9},"content":[{"type":"tool","id":"edit","name":"write","state":{"status":"completed","input":{"path":"/w/a"}}},{"type":"text","text":"streaming"}]}"#]).unwrap();
        assert!(src.refresh().unwrap());
        assert_eq!(src.info().events[0].state, EventState::Ok);
        assert_eq!(src.info().files.len(), 1);
        assert_eq!(src.info().context_used, Some(9));
        assert_eq!(src.info().last_reply.as_deref(), Some("streaming"));
        assert_eq!(src.info().model.as_deref(), Some("stream-model"));
        c.execute("insert into session_v2(id,parent_id,title,model,time_created,time_updated) values('child','s','child','{}',1,1)",[]).unwrap();
        put_msg(&p, "cu", "child", "user", 0, 1, r#"{"text":"task"}"#);
        assert!(src.refresh().unwrap());
        assert!(src.info().children[0].running);
        put_msg(
            &p,
            "ca",
            "child",
            "assistant",
            1,
            2,
            r#"{"time":{"completed":3},"content":[{"type":"text","text":"done"}]}"#,
        );
        put_msg(&p, "cs", "child", "system", 2, 3, "{}");
        assert!(src.refresh().unwrap());
        assert!(!src.info().children[0].running);
        assert_eq!(src.info().children[0].last_reply.as_deref(), Some("done"));
        assert!(!src.refresh().unwrap());
        drop(src);
        drop(c);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn old_only_database_paginates_parts_and_rechecks_timestamp_ties() {
        let p = tmp("old-pages");
        old_db(&p);
        put_session(&p, "session", "s", "old", "{}", 1000);
        let mut c = Db::open(&p).unwrap();
        c.execute_batch("create index part_session_idx on part(session_id);create index message_session_time_created_id_idx on message(session_id,time_created,id);").unwrap();
        c.execute(
            "insert into message values('a','s',1,1000,?1)",
            [r#"{"role":"assistant"}"#],
        )
        .unwrap();
        let tx = c.transaction().unwrap();
        for n in 0..1210 {
            let data = serde_json::json!({"type":"tool","tool":"write","callID":format!("c-{n}"),"state":{"status":"completed","input":{"path":format!("/w/f-{n}")}}});
            tx.execute(
                "insert into part values(?1,'a','s',?2,1000,?3)",
                params![format!("p-{n:04}"), n, data.to_string()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let mut src = open_db(&p, "s");
        src.refresh().unwrap();
        assert_eq!(src.info().files.len(), 1210);
        assert_eq!(src.info().events.len(), EVENTS_MAX);
        c.execute("insert into part values('p-new','a','s',2000,1000,?1)",[r#"{"type":"tool","tool":"write","callID":"new","state":{"status":"running","input":{"path":"/w/late"}}}"#]).unwrap();
        assert!(src.refresh().unwrap());
        assert_eq!(src.info().events.last().unwrap().state, EventState::Running);
        c.execute("update part set data=?1 where id='p-new'",[r#"{"type":"tool","tool":"write","callID":"new","state":{"status":"completed","input":{"path":"/w/late"}}}"#]).unwrap();
        assert!(src.refresh().unwrap());
        assert_eq!(src.info().files.len(), 1211);
        assert_eq!(src.info().events.last().unwrap().state, EventState::Ok);
        assert!(!src.refresh().unwrap());
        drop(src);
        drop(c);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn more_than_3000_blocks_and_extreme_tokens() {
        let p = tmp("blocks");
        new_db(&p);
        put_session(&p, "session_v2", "s", "title", "{}", 1);
        let blocks: Vec<_> = (0..3005)
            .map(|n| serde_json::json!({"type":"text","text":format!("block-{n}")}))
            .collect();
        put_msg(
            &p,
            "a",
            "s",
            "assistant",
            0,
            1,
            &serde_json::json!({"time":{"completed":2},"content":blocks}).to_string(),
        );
        let c = Db::open(&p).unwrap();
        c.execute("update session_v2 set tokens_input=?1,tokens_cache_read=?1,tokens_cache_write=?1 where id='s'",[i64::MAX]).unwrap();
        c.execute("insert into session_v2(id,parent_id,title,model,time_created,time_updated) values('child','s','child','{}',1,1)",[]).unwrap();
        put_msg(
            &p,
            "child-a",
            "child",
            "assistant",
            0,
            1,
            &serde_json::json!({"time":{"completed":2},"content":blocks}).to_string(),
        );
        let mut src = open_db(&p, "s");
        src.refresh().unwrap();
        assert_eq!(
            src.info().children[0].last_reply.as_deref(),
            Some("block-3004")
        );
        assert_eq!(src.info().children[0].events.len(), CHILD_EVENTS_MAX);
        assert_eq!(src.info().tokens_in, u64::MAX);
        assert_eq!(src.info().events.len(), EVENTS_MAX);
        assert_eq!(src.info().last_reply.as_deref(), Some("block-3004"));
        drop(src);
        drop(c);
        std::fs::remove_file(p).unwrap();
    }
    #[test]
    fn interrupted_tools_do_not_restart_when_replayed() {
        let mut state = State::default();
        let tool = Tool {
            name: "read".into(),
            call_id: "call".into(),
            status: "running".into(),
            path: Some("a".into()),
            ..Tool::default()
        };
        state.tool_event(&tool, 1, Path::new("/w"));
        state.interrupt();
        state.tool_event(&tool, 1, Path::new("/w"));
        assert_eq!(state.info.events[0].state, EventState::Error);
    }
    #[test]
    fn all_current_todos_and_children_survive_query_page_limits() {
        let p = tmp("lists");
        new_db(&p);
        put_session(&p, "session_v2", "s", "title", "{}", 1);
        let mut c = Db::open(&p).unwrap();
        let tx = c.transaction().unwrap();
        for n in 0..205 {
            tx.execute(
                "insert into todo(session_id,content,status,position) values('s',?1,'pending',?2)",
                params![format!("todo-{n}"), n],
            )
            .unwrap();
        }
        for n in 0..55 {
            tx.execute("insert into session_v2(id,parent_id,title,model,time_created,time_updated) values(?1,'s','child','{}',1,1)",[format!("child-{n}")]).unwrap();
        }
        tx.commit().unwrap();
        let mut src = open_db(&p, "s");
        src.refresh().unwrap();
        assert_eq!(src.info().todos.len(), 205);
        assert_eq!(src.info().children.len(), 55);
        drop(src);
        drop(c);
        std::fs::remove_file(p).unwrap();
    }
}
