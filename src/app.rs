//! Estado de la aplicación, entrada del usuario y acciones sobre los agentes.

use crate::config::Config;
use crate::herdr::Herdr;
use crate::model::*;
use crate::util::now;
use crate::{ansi, keys, sources};
use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Minutos que un subagente terminado sigue a la vista en la lista.
const CHILD_RECENT_SECS: i64 = 15 * 60;
/// Subagentes terminados que se enseñan como mucho por agente (salvo con el historial abierto).
const CHILD_RECENT_MAX: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Solo el espacio de trabajo desde el que se abrió la vista.
    Workspace,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// Lista de agentes a la izquierda y el agente elegido a la derecha.
    Detail,
    /// Mosaico con la pantalla en vivo de cada agente.
    Wall,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Rail,
    Stage,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum StageTab {
    Activity,
    Screen,
    Files,
}

/// Identidad estable de lo seleccionado: un panel o un subagente de un panel.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub pane: String,
    pub child: Option<String>,
}

/// Fila de la lista de agentes.
#[derive(Clone, Debug)]
pub enum Item {
    Group { label: String, count: usize },
    Agent(usize),
    Child { pane: usize, child: usize, last: bool },
}

pub struct Input {
    pub title: String,
    pub targets: Vec<String>,
    pub text: String,
    /// Posición del cursor en caracteres.
    pub cursor: usize,
}

pub struct Confirm {
    pub title: String,
    pub body: String,
    pub danger: bool,
    pub action: Action,
}

#[derive(Clone, Debug)]
pub enum Action {
    Interrupt(Vec<String>),
    Close(String),
}

pub struct NewAgent {
    pub sel: usize,
    /// Panel en el que se lanza (ya en la shell) o del que se parte para dividir.
    pub pane: String,
    /// Lanzar en ese mismo panel en vez de dividirlo.
    pub in_place: bool,
}

pub enum Mode {
    Normal,
    Input(Input),
    Confirm(Confirm),
    /// Todo lo que se teclea va al panel seleccionado.
    Direct,
    Search,
    New(NewAgent),
    Help,
    Explain(Explanation),
}

pub struct Explanation {
    pub id: u64,
    pub fingerprint: u64,
    pub title: String,
    pub model: String,
    pub effort: String,
    pub text: String,
    pub error: Option<String>,
    pub loading: bool,
    pub scroll: usize,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Row(usize),
    Tab(StageTab),
    Event(usize),
    Btn(Btn),
    Pill(Status),
    Card(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    Scope,
    Wall,
    Help,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Ok,
    Warn,
    Err,
}

pub enum Msg {
    Snapshot(Result<Snapshot, String>),
    Session { pane: String, info: Option<Arc<SessionInfo>> },
    SessionLoaded { want: Want, info: Option<Arc<SessionInfo>> },
    Screen { pane: String, lines: Arc<Vec<Line<'static>>> },
    Toast(ToastKind, String),
    Quit,
    Explanation { id: u64, text: Result<String,String>, done: bool },
}

#[derive(Clone)]
pub struct Want {
    pane: String,
    kind: Kind,
    session: SessionRef,
    cwd: PathBuf,
}

/// Lo que la interfaz necesita ahora mismo de los hilos de fondo.
#[derive(Clone, Default)]
struct Wants {
    /// Sesiones a mantener al día; la primera es la seleccionada.
    sessions: Vec<Want>,
    /// Paneles cuya pantalla hay que leer.
    screens: Vec<String>,
    /// Refresco rápido de pantalla (control directo).
    fast: bool,
}

enum Job {
    Prompt { panes: Vec<(String, String)>, text: String },
    Interrupt { panes: Vec<(String, String, Vec<String>)> },
    Close { pane: String, name: String },
    Launch { pane: String, name: String, command: String },
    Spawn { from: String, cwd: String, kind: String, name: String, args: Vec<String> },
    Goto { pane: String },
    Direct { pane: String, input: keys::Input },
}

pub struct App {
    pub cfg: Config,
    pub rx: Receiver<Msg>,
    jobs: Sender<Job>,
    wants: Arc<Mutex<Wants>>,
    stop: Arc<AtomicBool>,
    ai_tx: Sender<Msg>,
    ai_live: bool,
    ai_sequence: u64,
    ai_cache: HashMap<u64,String>,

    /// Espacio y panel desde los que se abrió la vista.
    pub origin_ws: Option<String>,
    pub origin_pane: Option<String>,
    own_pane: Option<String>,

    /// Todos los paneles de herdr, en su orden.
    pub panes: Vec<Pane>,
    pub sessions: HashMap<String, Arc<SessionInfo>>,
    /// Paneles cuya fuente de sesión ya se ha intentado abrir (haya datos o no).
    pub probed: HashSet<String>,
    pub screens: HashMap<String, Arc<Vec<Line<'static>>>>,
    pub loaded: bool,
    pub error: Option<String>,

    pub scope: Scope,
    pub view: View,
    pub focus: Focus,
    pub mode: Mode,
    pub filter: String,
    pub items: Vec<Item>,
    pub sel: usize,
    sel_key: Option<Key>,
    pub rail_top: usize,
    pub marks: HashSet<String>,
    /// Paneles con un cambio de estado que el usuario aún no ha mirado.
    pub fresh: HashSet<String>,
    pub all_children: bool,

    tab_user: Option<StageTab>,
    /// Evento seleccionado en la línea de tiempo.
    pub ev_sel: usize,
    /// La línea de tiempo sigue al último evento.
    pub follow: bool,
    pub ev_top: usize,
    pub screen_up: usize,
    pub files_top: usize,
    pub detail_top: usize,
    pub zoom: bool,

    pub toast: Option<(String, ToastKind, Instant)>,
    pub tick: u64,
    pub quit: bool,
    last_status: HashMap<String, Status>,
    last_click: Option<(Instant, Key)>,

    // Geometría del último repintado, para el ratón.
    pub hits: Vec<(Rect, Hit)>,
    pub rail_area: Rect,
    pub stage_area: Rect,
    pub wall_cols: usize,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

impl App {
    pub fn new(cfg: Config) -> App {
        Self::build(cfg, true)
    }

    /// Sin hilos de fondo ni conexión con herdr: los datos se inyectan con `on_msg` (modo demo).
    pub fn offline(cfg: Config) -> App {
        Self::build(cfg, false)
    }

    fn build(cfg: Config, live: bool) -> App {
        let herdr = Herdr::from_env();
        let (tx, rx) = mpsc::channel();
        let (jobs, jobs_rx) = mpsc::channel();
        let wants = Arc::new(Mutex::new(Wants::default()));

        let ctx: serde_json::Value =
            env("HERDR_PLUGIN_CONTEXT_JSON").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let from_ctx = |k: &str| ctx[k].as_str().filter(|s| !s.is_empty()).map(String::from);
        let own_pane = env("HERDR_PANE_ID");
        let origin_pane = from_ctx("focused_pane_id");
        let origin_ws = from_ctx("workspace_id")
            .or_else(|| env("HERDR_ACTIVE_WORKSPACE_ID"))
            .or_else(|| origin_pane.as_ref().and_then(|p| p.split(':').next().map(String::from)))
            .or_else(|| env("HERDR_WORKSPACE_ID"));

        let ai_tx = tx.clone();
        let stop = Arc::new(AtomicBool::new(false));
        if live {
            spawn_poller(herdr.clone(), tx.clone(), wants.clone(), cfg.refresh_ms.max(200), stop.clone());
            spawn_sessions(tx.clone(), wants.clone(), stop.clone());
            spawn_jobs(herdr, tx, jobs_rx, stop.clone());
        }

        App {
            scope: if cfg.scope == "all" { Scope::All } else { Scope::Workspace },
            cfg,
            rx,
            jobs,
            wants,
            stop,
            ai_tx,
            ai_live: live,
            ai_sequence: 0,
            ai_cache: HashMap::new(),
            origin_ws,
            origin_pane,
            own_pane,
            panes: vec![],
            sessions: HashMap::new(),
            probed: HashSet::new(),
            screens: HashMap::new(),
            loaded: false,
            error: None,
            view: View::Detail,
            focus: Focus::Rail,
            mode: Mode::Normal,
            filter: String::new(),
            items: vec![],
            sel: 0,
            sel_key: None,
            rail_top: 0,
            marks: HashSet::new(),
            fresh: HashSet::new(),
            all_children: false,
            tab_user: None,
            ev_sel: 0,
            follow: true,
            ev_top: 0,
            screen_up: 0,
            files_top: 0,
            detail_top: 0,
            zoom: false,
            toast: None,
            tick: 0,
            quit: false,
            last_status: HashMap::new(),
            last_click: None,
            hits: vec![],
            rail_area: Rect::default(),
            stage_area: Rect::default(),
            wall_cols: 1,
        }
    }

    // ───────────────────────────── consultas ─────────────────────────────

    /// Nombre con el que se enseña un panel.
    pub fn name(p: &Pane) -> String {
        match &p.label {
            Some(l) => l.clone(),
            None => crate::theme::kind_name(p.kind, p.agent_raw.as_deref()).to_lowercase(),
        }
    }

    pub fn sel_pane_idx(&self) -> Option<usize> {
        match self.items.get(self.sel)? {
            Item::Agent(i) => Some(*i),
            Item::Child { pane, .. } => Some(*pane),
            Item::Group { .. } => None,
        }
    }

    pub fn sel_pane(&self) -> Option<&Pane> {
        self.panes.get(self.sel_pane_idx()?)
    }

    pub fn info(&self, pane_id: &str) -> Option<&SessionInfo> {
        self.sessions.get(pane_id).map(Arc::as_ref)
    }

    pub fn sel_info(&self) -> Option<&SessionInfo> {
        self.info(&self.sel_pane()?.pane_id)
    }

    pub fn sel_child(&self) -> Option<&Child> {
        match self.items.get(self.sel)? {
            Item::Child { pane, child, .. } => self.info(&self.panes.get(*pane)?.pane_id)?.children.get(*child),
            _ => None,
        }
    }

    /// Eventos que enseña la línea de tiempo: los del subagente si hay uno seleccionado.
    pub fn stage_events(&self) -> &[Event] {
        match self.sel_child() {
            Some(c) => &c.events,
            None => self.sel_info().map(|i| i.events.as_slice()).unwrap_or(&[]),
        }
    }

    pub fn has_source(k: Kind) -> bool {
        matches!(k, Kind::Claude | Kind::Codex | Kind::OpenCode | Kind::Omp)
    }

    /// Pestaña que se enseña: la que eligió el usuario o, si no eligió, la más útil para el estado.
    pub fn tab(&self) -> StageTab {
        if self.sel_child().is_some() {
            return StageTab::Activity;
        }
        let Some(p) = self.sel_pane() else { return StageTab::Activity };
        let no_data = !Self::has_source(p.kind)
            || p.session.is_none()
            || (self.probed.contains(&p.pane_id) && self.info(&p.pane_id).is_none());
        if no_data || matches!(self.mode, Mode::Direct) {
            return StageTab::Screen;
        }
        match self.tab_user {
            Some(t) => t,
            None if p.status == Status::Blocked => StageTab::Screen,
            None => StageTab::Activity,
        }
    }

    /// El agente dice que trabaja pero su transcript lleva demasiado sin moverse.
    pub fn stalled(&self, p: &Pane) -> Option<i64> {
        if p.status != Status::Working {
            return None;
        }
        let quiet = now().saturating_sub(self.info(&p.pane_id)?.updated_at?);
        (quiet > i64::try_from(self.cfg.stalled_minutes).unwrap_or(i64::MAX).saturating_mul(60)).then_some(quiet)
    }

    pub fn in_scope(&self, p: &Pane) -> bool {
        match (self.scope, &self.origin_ws) {
            (Scope::Workspace, Some(ws)) => &p.workspace_id == ws,
            _ => true,
        }
    }

    /// Paneles que cuentan como agentes dentro del ámbito (para los contadores de la cabecera).
    pub fn scoped_agents(&self) -> impl Iterator<Item = &Pane> {
        self.panes.iter().filter(|p| p.kind != Kind::Shell && self.in_scope(p))
    }

    pub fn scope_label(&self) -> String {
        match self.scope {
            Scope::All => "Todos los espacios".into(),
            Scope::Workspace => self
                .panes
                .iter()
                .find(|p| Some(&p.workspace_id) == self.origin_ws.as_ref())
                .map(|p| p.workspace_label.clone())
                .unwrap_or_else(|| "Este espacio".into()),
        }
    }

    /// Posiciones en `items` de las tarjetas del mosaico (agentes, sin subagentes ni shells).
    pub fn wall_items(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, Item::Agent(i) if self.panes[*i].kind != Kind::Shell))
            .map(|(n, _)| n)
            .collect()
    }

    // ───────────────────────────── mensajes de fondo ─────────────────────────────

    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Snapshot(Ok(s)) => {
                self.error = None;
                self.apply_snapshot(s);
            }
            Msg::Snapshot(Err(e)) => self.error = Some(e),
            Msg::SessionLoaded { want, info } => {
                if self.panes.iter().any(|p| p.pane_id == want.pane && p.kind == want.kind && p.session.as_ref() == Some(&want.session) && p.cwd == want.cwd) { self.on_msg(Msg::Session { pane: want.pane, info }); }
            }
            Msg::Session { pane, info } => {
                if !self.panes.iter().any(|p| p.pane_id == pane) { return; }
                let key = self.sel_key.clone();
                let selected = (!self.follow && self.sel_key.as_ref().is_some_and(|k| k.pane == pane)).then(|| self.stage_events().get(self.ev_sel).cloned()).flatten();
                self.probed.insert(pane.clone());
                match info {
                    Some(i) => {
                        self.sessions.insert(pane, i);
                    }
                    None => {
                        self.sessions.remove(&pane);
                    }
                }
                self.rebuild();
                if let Some(old) = selected && self.sel_key == key && !self.follow {
                    self.ev_sel = self.stage_events().iter().position(|e| e.ts == old.ts && e.kind == old.kind && e.title == old.title && e.child == old.child).unwrap_or(0);
                }
            }
            Msg::Screen { pane, lines } => {
                if self.panes.iter().any(|p| p.pane_id == pane) { self.screens.insert(pane, lines); }
            }
            Msg::Toast(k, t) => self.say(k, t),
            Msg::Quit => self.quit = true,
            Msg::Explanation {id,text,done} => {
                if let Mode::Explain(ex) = &mut self.mode && ex.id == id {
                    ex.loading = !done;
                    match text {
                        Ok(text) => { ex.text = text.clone(); if done { if self.ai_cache.len()>=32 {self.ai_cache.clear();} self.ai_cache.insert(ex.fingerprint,text); } }
                        Err(error) => ex.error = Some(error),
                    }
                }
            }
        }
    }

    fn apply_snapshot(&mut self, s: Snapshot) {
        let first = !self.loaded;
        self.loaded = true;
        if self.origin_pane.is_none() {
            self.origin_pane = s.focused_pane_id.clone().filter(|p| Some(p) != self.own_pane.as_ref());
        }
        if self.origin_ws.is_none() {
            self.origin_ws = s.focused_workspace_id.clone();
        }
        for p in &s.panes {
            if self.panes.iter().find(|old| old.pane_id == p.pane_id).is_some_and(|old| old.session != p.session || old.kind != p.kind || old.cwd != p.cwd) {
                self.sessions.remove(&p.pane_id); self.probed.remove(&p.pane_id); self.screens.remove(&p.pane_id);
                if self.sel_key.as_ref().is_some_and(|k| k.pane == p.pane_id) && matches!(self.mode, Mode::Direct) { self.mode = Mode::Normal; }
            }
        }
        self.panes = s.panes.into_iter().filter(|p| Some(&p.pane_id) != self.own_pane.as_ref()).collect();

        // Cambios de estado mientras la vista está abierta: aviso y marca de «nuevo».
        let mut news = None;
        let watching = self.sel_key.as_ref().map(|k| k.pane.clone());
        for p in &self.panes {
            let prev = self.last_status.insert(p.pane_id.clone(), p.status);
            if first || p.kind == Kind::Shell || !self.in_scope(p) {
                continue;
            }
            let name = Self::name(p);
            match (prev, p.status) {
                (Some(Status::Working), Status::Done | Status::Idle) => {
                    news = Some((ToastKind::Ok, format!("✓ {name} ha terminado")));
                    if watching.as_ref() != Some(&p.pane_id) {
                        self.fresh.insert(p.pane_id.clone());
                    }
                }
                (Some(a), Status::Blocked) if a != Status::Blocked => {
                    news = Some((ToastKind::Warn, format!("▲ {name} espera respuesta")));
                    if watching.as_ref() != Some(&p.pane_id) {
                        self.fresh.insert(p.pane_id.clone());
                    }
                }
                _ => {}
            }
        }
        let alive: HashSet<&str> = self.panes.iter().map(|p| p.pane_id.as_str()).collect();
        self.last_status.retain(|k, _| alive.contains(k.as_str()));
        self.marks.retain(|k| alive.contains(k.as_str()));
        self.fresh.retain(|k| alive.contains(k.as_str()));
        self.sessions.retain(|k, _| alive.contains(k.as_str()));
        self.probed.retain(|k| alive.contains(k.as_str()));
        self.screens.retain(|k, _| alive.contains(k.as_str()));
        if let Some((k, t)) = news {
            self.say(k, t);
        }
        self.rebuild();
        if first {
            self.select_initial();
        }
    }

    fn visible_children(&self, pane_id: &str) -> Vec<usize> {
        let Some(info) = self.info(pane_id) else { return vec![] };
        if self.all_children {
            return (0..info.children.len()).collect();
        }
        let t = now();
        let mut done_left = CHILD_RECENT_MAX;
        let mut out: Vec<usize> = (0..info.children.len())
            .rev()
            .filter(|&i| {
                let c = &info.children[i];
                if c.running {
                    return true;
                }
                let recent = c.updated_at.is_some_and(|u| t.saturating_sub(u) < CHILD_RECENT_SECS);
                if recent && done_left > 0 {
                    done_left -= 1;
                    return true;
                }
                false
            })
            .collect();
        out.reverse();
        out
    }

    fn matches_filter(&self, p: &Pane) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let q = self.filter.to_lowercase();
        let hay = format!(
            "{} {} {} {} {}",
            Self::name(p),
            p.title,
            crate::theme::kind_name(p.kind, p.agent_raw.as_deref()),
            p.workspace_label,
            self.info(&p.pane_id).and_then(|i| i.last_prompt.clone()).unwrap_or_default()
        )
        .to_lowercase();
        q.split_whitespace().all(|w| hay.contains(w))
    }

    /// Reconstruye la lista de filas conservando la selección.
    pub fn rebuild(&mut self) {
        let keep = self.sel_key.clone();
        let mut items = Vec::new();
        let vis: Vec<usize> = (0..self.panes.len())
            .filter(|&i| {
                let p = &self.panes[i];
                // Una shell solo interesa si tiene nombre (un agente que se cerró) o sesión que reanudar.
                let shown = p.kind != Kind::Shell || p.label.is_some() || p.session.is_some();
                shown && self.in_scope(p) && self.matches_filter(p)
            })
            .collect();
        let grouped = self.scope == Scope::All || self.origin_ws.is_none();
        let mut cur_ws: Option<&str> = None;
        for &i in &vis {
            let p = &self.panes[i];
            if grouped && cur_ws != Some(p.workspace_id.as_str()) {
                cur_ws = Some(p.workspace_id.as_str());
                let count = vis.iter().filter(|&&j| self.panes[j].workspace_id == p.workspace_id).count();
                items.push(Item::Group { label: p.workspace_label.clone(), count });
            }
            items.push(Item::Agent(i));
            let kids = self.visible_children(&p.pane_id);
            let n = kids.len();
            for (k, child) in kids.into_iter().enumerate() {
                items.push(Item::Child { pane: i, child, last: k + 1 == n });
            }
        }
        self.items = items;
        // Recuperar la selección: la misma fila; si era un subagente que ya no se ve, su padre.
        let find = |key: &Key, items: &[Item]| -> Option<usize> {
            items.iter().position(|it| self.key_of(it).as_ref() == Some(key))
        };
        let pos = keep.as_ref().and_then(|k| {
            find(k, &self.items).or_else(|| find(&Key { pane: k.pane.clone(), child: None }, &self.items))
        });
        match pos {
            Some(p) => self.sel = p,
            None => {
                self.sel = self.sel.min(self.items.len().saturating_sub(1));
                self.snap_to_row(1);
            }
        }
        if self.view == View::Wall && !self.wall_items().contains(&self.sel) && let Some(&first) = self.wall_items().first() { self.sel = first; }
        let new_key = self.items.get(self.sel).and_then(|it| self.key_of(it));
        if new_key != keep {
            self.on_selection_changed(new_key);
        }
        self.clamp_events();
        self.push_wants();
    }

    fn key_of(&self, it: &Item) -> Option<Key> {
        match it {
            Item::Agent(i) => Some(Key { pane: self.panes.get(*i)?.pane_id.clone(), child: None }),
            Item::Child { pane, child, .. } => {
                let p = self.panes.get(*pane)?;
                Some(Key { pane: p.pane_id.clone(), child: Some(self.info(&p.pane_id)?.children.get(*child)?.id.clone()) })
            }
            Item::Group { .. } => None,
        }
    }

    fn selectable(&self, i: usize) -> bool {
        matches!(self.items.get(i), Some(Item::Agent(_) | Item::Child { .. }))
    }

    /// Si la fila seleccionada es una cabecera de grupo, pasa a la fila útil más cercana.
    fn snap_to_row(&mut self, dir: isize) {
        if self.items.is_empty() || self.selectable(self.sel) {
            return;
        }
        let n = self.items.len() as isize;
        for step in 1..=n {
            for d in [dir, -dir] {
                let j = self.sel as isize + d * step;
                if j >= 0 && j < n && self.selectable(j as usize) {
                    self.sel = j as usize;
                    return;
                }
            }
        }
    }

    fn on_selection_changed(&mut self, key: Option<Key>) {
        if self.sel_key.as_ref().map(|k| &k.pane) != key.as_ref().map(|k| &k.pane) && matches!(self.mode, Mode::Direct) { self.mode = Mode::Normal; }
        self.sel_key = key;
        self.tab_user = None;
        self.follow = true;
        self.ev_sel = 0;
        self.ev_top = 0;
        self.screen_up = 0;
        self.files_top = 0;
        self.detail_top = 0;
        self.zoom = false;
        if let Some(p) = self.sel_pane().map(|p| p.pane_id.clone()) {
            self.fresh.remove(&p);
        }
    }

    fn clamp_events(&mut self) {
        let n = self.stage_events().len();
        if self.follow || self.ev_sel >= n {
            self.ev_sel = n.saturating_sub(1);
        }
    }

    fn select(&mut self, i: usize) {
        if !self.selectable(i) || i == self.sel {
            return;
        }
        self.sel = i;
        let key = self.key_of(&self.items[i]);
        self.on_selection_changed(key);
        self.clamp_events();
        self.push_wants();
    }

    fn move_sel(&mut self, d: isize) {
        let n = self.items.len() as isize;
        let mut j = self.sel as isize + d;
        while j >= 0 && j < n {
            if self.selectable(j as usize) {
                self.select(j as usize);
                return;
            }
            j += d.signum();
        }
    }

    /// Al abrir: lo que pide atención antes que nada; si no, lo que está trabajando.
    fn select_initial(&mut self) {
        let rank = |p: &Pane| match p.status {
            Status::Blocked => 0,
            Status::Working if Some(&p.pane_id) != self.origin_pane.as_ref() => 1,
            Status::Done => 2,
            _ if Some(&p.pane_id) == self.origin_pane.as_ref() => 3,
            Status::Working => 4,
            _ => 5,
        };
        let best = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(n, it)| match it {
                Item::Agent(i) if self.panes[*i].kind != Kind::Shell => Some((rank(&self.panes[*i]), n)),
                _ => None,
            })
            .min();
        if let Some((_, n)) = best {
            self.sel = n;
            let key = self.key_of(&self.items[n]);
            self.on_selection_changed(key);
            self.clamp_events();
            self.push_wants();
        }
    }

    /// Publica qué sesiones y pantallas necesitan los hilos de fondo.
    fn push_wants(&self) {
        let want = |i: usize| -> Option<Want> {
            let p = &self.panes[i];
            if !Self::has_source(p.kind) {
                return None;
            }
            Some(Want { pane: p.pane_id.clone(), kind: p.kind, session: p.session.clone()?, cwd: p.cwd.clone() })
        };
        let sel = self.sel_pane_idx();
        let mut sessions: Vec<Want> = sel.and_then(want).into_iter().collect();
        for it in &self.items {
            if let Item::Agent(i) = it
                && Some(*i) != sel
                && let Some(w) = want(*i)
            {
                sessions.push(w);
            }
        }
        let screens = match self.view {
            View::Wall => self.wall_items().iter().filter_map(|&n| self.key_of(&self.items[n])).map(|k| k.pane).collect(),
            View::Detail if self.tab() == StageTab::Screen => self.sel_pane().map(|p| vec![p.pane_id.clone()]).unwrap_or_default(),
            View::Detail => vec![],
        };
        if let Ok(mut w) = self.wants.lock() {
            *w = Wants { sessions, screens, fast: matches!(self.mode, Mode::Direct) };
        }
    }

    pub fn say(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast = Some((text.into(), kind, Instant::now()));
    }

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if let Some((_, k, t)) = &self.toast {
            let ttl = if *k == ToastKind::Err { 6 } else { 4 };
            if t.elapsed() > Duration::from_secs(ttl) {
                self.toast = None;
            }
        }
    }

    // ───────────────────────────── acciones ─────────────────────────────

    fn job(&self, j: Job) {
        let _ = self.jobs.send(j);
    }

    fn agent_cfg(&self, p: &Pane) -> crate::config::AgentCfg {
        let key = p.agent_raw.clone().or_else(|| p.session_agent.clone()).unwrap_or_default();
        self.cfg.agents.get(&key).cloned().unwrap_or_default()
    }

    fn interrupt_keys(&self, p: &Pane) -> Vec<String> {
        let k = self.agent_cfg(p).interrupt;
        if k.is_empty() { vec!["escape".into()] } else { k }
    }

    fn goto(&mut self) {
        if let Some(p) = self.sel_pane() {
            self.job(Job::Goto { pane: p.pane_id.clone() });
        }
    }

    /// Paneles a los que va un mensaje: los marcados o, si no hay marcas, el seleccionado.
    fn message_targets(&self) -> Vec<&Pane> {
        if self.marks.is_empty() {
            self.sel_pane().into_iter().collect()
        } else {
            self.panes.iter().filter(|p| self.marks.contains(&p.pane_id)).collect()
        }
    }

    fn start_message(&mut self) {
        let targets = self.message_targets();
        let Some(first) = targets.first() else { return };
        if targets.iter().any(|p| p.kind == Kind::Shell) {
            return self.say(ToastKind::Warn, "En ese panel no hay ningún agente: usa r para lanzarlo");
        }
        let title = match targets.len() {
            1 => format!("Mensaje para {}", Self::name(first)),
            n => format!("Mensaje para {n} agentes"),
        };
        // Un subagente interno no se puede escribir directamente: el mensaje va a su agente padre.
        let text = self.sel_child().filter(|_| self.marks.is_empty()).map(|c| format!("Sobre el subagente «{}»: ", c.name)).unwrap_or_default();
        let cursor = text.chars().count();
        let targets = targets.iter().map(|p| p.pane_id.clone()).collect();
        self.mode = Mode::Input(Input { title, targets, text, cursor });
    }

    fn send_message(&mut self, targets: &[String], text: String) {
        let panes: Vec<(String, String)> =
            self.panes.iter().filter(|p| targets.contains(&p.pane_id)).map(|p| (p.pane_id.clone(), Self::name(p))).collect();
        if !panes.is_empty() && !text.trim().is_empty() {
            self.job(Job::Prompt { panes, text });
        }
    }

    fn continue_agent(&mut self) {
        let Some(p) = self.sel_pane() else { return };
        match p.status {
            Status::Working => self.say(ToastKind::Warn, "Ya está trabajando"),
            _ if p.kind == Kind::Shell => self.say(ToastKind::Warn, "En ese panel no hay ningún agente"),
            _ => {
                let (pane, name) = (p.pane_id.clone(), Self::name(p));
                self.job(Job::Prompt { panes: vec![(pane, name)], text: self.cfg.continue_prompt.clone() });
            }
        }
    }

    fn ask_interrupt(&mut self) {
        let Some(p) = self.sel_pane() else { return };
        if p.status != Status::Working {
            return self.say(ToastKind::Warn, "No está trabajando: no hay nada que interrumpir");
        }
        let name = Self::name(p);
        let body = match self.sel_child() {
            Some(c) => format!("«{}» es un subagente interno de {name}: se interrumpe el turno entero de {name}.", c.name),
            None => "Corta el turno en curso. El agente conserva la sesión y se le puede seguir hablando.".into(),
        };
        self.mode = Mode::Confirm(Confirm {
            title: format!("¿Interrumpir a {name}?"),
            body,
            danger: false,
            action: Action::Interrupt(vec![p.pane_id.clone()]),
        });
    }

    fn ask_stop_all(&mut self) {
        let ids: Vec<String> =
            self.scoped_agents().filter(|p| p.status == Status::Working && Some(&p.pane_id) != self.origin_pane.as_ref()).map(|p| p.pane_id.clone()).collect();
        if ids.is_empty() {
            return self.say(ToastKind::Warn, "No hay ningún otro agente trabajando");
        }
        self.mode = Mode::Confirm(Confirm {
            title: format!("¿Interrumpir a los {} agentes que trabajan?", ids.len()),
            body: format!("Ámbito: {}. No se interrumpe el agente desde el que abriste esta vista.", self.scope_label()),
            danger: true,
            action: Action::Interrupt(ids),
        });
    }

    fn ask_close(&mut self) {
        let Some(p) = self.sel_pane() else { return };
        let name = Self::name(p);
        let body = if p.status == Status::Working {
            "Está trabajando: se pierde el turno en curso. El panel desaparece de herdr.".to_string()
        } else {
            "El panel desaparece de herdr. La sesión del agente queda guardada en disco.".to_string()
        };
        self.mode = Mode::Confirm(Confirm {
            title: format!("¿Cerrar el panel de {name}?"),
            body,
            danger: true,
            action: Action::Close(p.pane_id.clone()),
        });
    }

    fn run_action(&mut self, a: Action) {
        match a {
            Action::Interrupt(ids) => {
                let panes = self
                    .panes
                    .iter()
                    .filter(|p| ids.contains(&p.pane_id))
                    .map(|p| (p.pane_id.clone(), Self::name(p), self.interrupt_keys(p)))
                    .collect();
                self.job(Job::Interrupt { panes });
            }
            Action::Close(id) => {
                if let Some(p) = self.panes.iter().find(|p| p.pane_id == id) {
                    self.job(Job::Close { pane: id, name: Self::name(p) });
                }
            }
        }
    }

    /// Reanuda la sesión guardada de un panel que se quedó en la shell; sin sesión, ofrece lanzar uno nuevo.
    fn resume(&mut self) {
        let Some(p) = self.sel_pane() else { return };
        if p.kind != Kind::Shell {
            return self.say(ToastKind::Warn, "El agente ya está en marcha");
        }
        let cfg = self.agent_cfg(p);
        let session = p.session.as_ref().map(|s| match s {
            SessionRef::Id(i) => i.clone(),
            SessionRef::Path(p) => p.to_string_lossy().into_owned(),
        });
        match session.filter(|_| !cfg.resume.is_empty()) {
            Some(s) => {
                let cwd = p.cwd.to_string_lossy();
                let command = cfg.resume.iter().map(|a| sh_quote(&a.replace("{session}", &s).replace("{cwd}", &cwd))).collect::<Vec<_>>().join(" ");
                self.job(Job::Launch { pane: p.pane_id.clone(), name: Self::name(p), command });
            }
            None => self.mode = Mode::New(NewAgent { sel: 0, pane: p.pane_id.clone(), in_place: true }),
        }
    }

    fn new_agent(&mut self) {
        let pane = self.sel_pane().map(|p| p.pane_id.clone()).or_else(|| self.origin_pane.clone());
        match pane {
            Some(pane) if !self.cfg.presets.is_empty() => self.mode = Mode::New(NewAgent { sel: 0, pane, in_place: false }),
            Some(_) => self.say(ToastKind::Warn, "No hay agentes definidos en [[presets]] de la configuración"),
            None => {}
        }
    }

    fn launch_preset(&mut self, n: &NewAgent) {
        let Some(preset) = self.cfg.presets.get(n.sel).cloned() else { return };
        let Some(p) = self.panes.iter().find(|p| p.pane_id == n.pane) else { return };
        let cwd = p.cwd.to_string_lossy().into_owned();
        // Los argumentos de arranque del tipo de agente (permisos, etc.) van antes que los del preset.
        let fresh = self.cfg.agents.get(&preset.kind).map(|a| a.fresh.clone()).filter(|f| !f.is_empty()).unwrap_or_else(|| vec![preset.kind.clone()]);
        if n.in_place {
            let command = fresh.iter().chain(preset.args.iter()).map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ");
            self.job(Job::Launch { pane: p.pane_id.clone(), name: Self::name(p), command });
        } else {
            let taken: HashSet<String> = self.panes.iter().filter_map(|p| p.label.clone()).collect();
            let name = unique_name(&preset.name, &taken);
            let args = fresh.iter().skip(1).chain(preset.args.iter()).cloned().collect();
            self.job(Job::Spawn { from: p.pane_id.clone(), cwd, kind: preset.kind, name, args });
        }
    }

    fn explain_selected(&mut self) {
        let Some(pane) = self.sel_pane() else { return; };
        let Some(info) = self.sel_info() else { return self.say(ToastKind::Warn,"Todavía no hay datos de la sesión para explicar"); };
        let child = self.sel_child();
        let name = child.map(|c|c.name.clone()).unwrap_or_else(||Self::name(pane));
        let prompt = crate::ai::prompt(&name,pane.status,info,child);
        let fingerprint = crate::ai::fingerprint(&self.cfg.ai,&prompt);
        self.ai_sequence = self.ai_sequence.wrapping_add(1);
        let id=self.ai_sequence;
        let cached = self.ai_cache.get(&fingerprint).cloned();
        let cancel=Arc::new(AtomicBool::new(false));
        let error=self.cfg.ai.error.clone().or_else(|| (!self.ai_live && cached.is_none()).then(||"La explicación requiere el modo conectado".into()));
        let loading=error.is_none() && cached.is_none();
        self.mode=Mode::Explain(Explanation {id,fingerprint,title:crate::ai::title(&name),model:self.cfg.ai.model.clone(),effort:self.cfg.ai.effort.clone(),text:cached.unwrap_or_default(),error,loading,scroll:0,cancel:cancel.clone()});
        if !loading { return; }
        let cfg=self.cfg.ai.clone();let tx=self.ai_tx.clone();
        std::thread::spawn(move||{
            let result=crate::ai::explain(&cfg,&prompt,cancel.clone(),|text|{
                if tx.send(Msg::Explanation {id,text:Ok(text.into()),done:false}).is_err() {cancel.store(true,Ordering::Relaxed);}
            }).map_err(|e|e.to_string());
            if !cancel.load(Ordering::Relaxed) { let _=tx.send(Msg::Explanation {id,text:result,done:true}); }
        });
    }

    fn copy_reply(&mut self) {
        let text = match self.sel_child() {
            Some(c) => c.last_reply.clone(),
            None => self.sel_info().and_then(|i| i.last_reply.clone()),
        };
        match text {
            Some(t) => {
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]52;c;{}\x07", base64(t.as_bytes()));
                let _ = out.flush();
                self.say(ToastKind::Ok, "Última respuesta copiada");
            }
            None => self.say(ToastKind::Warn, "Todavía no hay respuesta que copiar"),
        }
    }

    fn next_attention(&mut self) {
        let n = self.items.len();
        let wants = |it: &Item| matches!(it, Item::Agent(i) if matches!(self.panes[*i].status, Status::Blocked | Status::Done));
        match (1..=n).map(|d| (self.sel + d) % n.max(1)).find(|&j| wants(&self.items[j])) {
            Some(j) => self.select(j),
            None => self.say(ToastKind::Ok, "Nada pendiente de tu atención"),
        }
    }

    fn jump_status(&mut self, s: Status) {
        let n = self.items.len();
        if let Some(j) = (1..=n).map(|d| (self.sel + d) % n.max(1)).find(|&j| matches!(&self.items[j], Item::Agent(i) if self.panes[*i].status == s)) {
            self.select(j);
        }
    }

    fn toggle_scope(&mut self) {
        if self.origin_ws.is_none() {
            return;
        }
        self.scope = if self.scope == Scope::All { Scope::Workspace } else { Scope::All };
        self.rail_top = 0;
        self.rebuild();
    }

    fn toggle_wall(&mut self) {
        self.view = if self.view == View::Wall { View::Detail } else { View::Wall };
        if self.view == View::Wall {
            self.focus = Focus::Rail;
            // En el mosaico solo hay agentes: si lo seleccionado es un subagente, su padre.
            if let Some(Item::Child { pane, .. }) = self.items.get(self.sel)
                && let Some(n) = self.items.iter().position(|it| matches!(it, Item::Agent(i) if i == pane))
            {
                self.select(n);
            }
        }
        if self.view == View::Wall && !self.wall_items().contains(&self.sel) && let Some(&first) = self.wall_items().first() { self.select(first); }
        self.push_wants();
    }

    fn set_tab(&mut self, t: StageTab) {
        if self.sel_child().is_some() && t != StageTab::Activity {
            return;
        }
        self.tab_user = Some(t);
        self.zoom = false;
        self.push_wants();
    }

    // ───────────────────────────── teclado ─────────────────────────────

    pub fn on_event(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            TermEvent::Mouse(m) => self.on_mouse(m),
            TermEvent::Paste(s) => self.on_paste(s),
            _ => {}
        }
    }

    fn on_paste(&mut self, s: String) {
        match &mut self.mode {
            Mode::Input(inp) => {
                let at = byte_at(&inp.text, inp.cursor);
                inp.text.insert_str(at, &s);
                inp.cursor += s.chars().count();
            }
            Mode::Direct => {
                if let Some(p) = self.sel_pane() {
                    self.job(Job::Direct { pane: p.pane_id.clone(), input: keys::Input::Text(s) });
                }
            }
            Mode::Search => {
                self.filter.push_str(s.lines().next().unwrap_or(""));
                self.rebuild();
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.key_normal(k),
            Mode::Help => {}
            Mode::Explain(mut ex) => {
                match k.code {
                    KeyCode::Char('c') if ctrl => {ex.cancel.store(true,Ordering::Relaxed);self.quit=true;}
                    KeyCode::Esc | KeyCode::Char('q') => {ex.cancel.store(true,Ordering::Relaxed);}
                    KeyCode::Char('e') => {ex.cancel.store(true,Ordering::Relaxed);self.explain_selected();}
                    KeyCode::Up | KeyCode::Char('k') => {ex.scroll=ex.scroll.saturating_sub(1);self.mode=Mode::Explain(ex);}
                    KeyCode::Down | KeyCode::Char('j') => {ex.scroll=ex.scroll.saturating_add(1);self.mode=Mode::Explain(ex);}
                    KeyCode::PageUp => {ex.scroll=ex.scroll.saturating_sub(10);self.mode=Mode::Explain(ex);}
                    KeyCode::PageDown => {ex.scroll=ex.scroll.saturating_add(10);self.mode=Mode::Explain(ex);}
                    KeyCode::Home => {ex.scroll=0;self.mode=Mode::Explain(ex);}
                    KeyCode::End => {ex.scroll=usize::MAX;self.mode=Mode::Explain(ex);}
                    _ => self.mode=Mode::Explain(ex),
                }
            }
            Mode::Direct => {
                if ctrl && k.code == KeyCode::Char('q') {
                    self.push_wants();
                    return;
                }
                self.mode = Mode::Direct;
                if let (Some(pane), Some(input)) = (self.sel_pane().map(|p| p.pane_id.clone()), keys::translate(&k)) {
                    self.screen_up = 0;
                    self.job(Job::Direct { pane, input });
                }
            }
            Mode::Confirm(c) => match k.code {
                KeyCode::Enter | KeyCode::Char('s' | 'S' | 'y' | 'Y') => self.run_action(c.action),
                KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q') => {}
                _ => self.mode = Mode::Confirm(c),
            },
            Mode::Search => match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.rebuild();
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up => {
                    if k.code == KeyCode::Down {
                        self.move_sel(1);
                    } else if k.code == KeyCode::Up {
                        self.move_sel(-1);
                    }
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.mode = Mode::Search;
                    self.rebuild();
                }
                KeyCode::Char(c) if !ctrl => {
                    self.filter.push(c);
                    self.mode = Mode::Search;
                    self.rebuild();
                }
                _ => self.mode = Mode::Search,
            },
            Mode::New(mut n) => {
                let len = self.cfg.presets.len();
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => {}
                    KeyCode::Enter => self.launch_preset(&n),
                    KeyCode::Up | KeyCode::Char('k') => {
                        n.sel = n.sel.checked_sub(1).unwrap_or(len.saturating_sub(1));
                        self.mode = Mode::New(n);
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        n.sel = (n.sel + 1) % len.max(1);
                        self.mode = Mode::New(n);
                    }
                    KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < len => {
                        n.sel = c as usize - '1' as usize;
                        self.launch_preset(&n);
                    }
                    _ => self.mode = Mode::New(n),
                }
            }
            Mode::Input(mut inp) => {
                let alt = k.modifiers.contains(KeyModifiers::ALT);
                let n = inp.text.chars().count();
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter if alt => insert(&mut inp, '\n'),
                    KeyCode::Char('j') if ctrl => insert(&mut inp, '\n'),
                    KeyCode::Enter => {
                        if inp.text.trim().is_empty() {
                            return;
                        }
                        return self.send_message(&inp.targets, inp.text);
                    }
                    KeyCode::Char('u') if ctrl => {
                        inp.text.clear();
                        inp.cursor = 0;
                    }
                    KeyCode::Char('w') if ctrl => {
                        let chars: Vec<char> = inp.text.chars().collect();
                        let mut i = inp.cursor;
                        while i > 0 && chars[i - 1].is_whitespace() {
                            i -= 1;
                        }
                        while i > 0 && !chars[i - 1].is_whitespace() {
                            i -= 1;
                        }
                        let (a, b) = (byte_at(&inp.text, i), byte_at(&inp.text, inp.cursor));
                        inp.text.replace_range(a..b, "");
                        inp.cursor = i;
                    }
                    KeyCode::Char('a') if ctrl => inp.cursor = 0,
                    KeyCode::Char('e') if ctrl => inp.cursor = n,
                    KeyCode::Home => inp.cursor = 0,
                    KeyCode::End => inp.cursor = n,
                    KeyCode::Left => inp.cursor = inp.cursor.saturating_sub(1),
                    KeyCode::Right => inp.cursor = (inp.cursor + 1).min(n),
                    KeyCode::Backspace if inp.cursor > 0 => {
                        let (a, b) = (byte_at(&inp.text, inp.cursor - 1), byte_at(&inp.text, inp.cursor));
                        inp.text.replace_range(a..b, "");
                        inp.cursor -= 1;
                    }
                    KeyCode::Delete if inp.cursor < n => {
                        let (a, b) = (byte_at(&inp.text, inp.cursor), byte_at(&inp.text, inp.cursor + 1));
                        inp.text.replace_range(a..b, "");
                    }
                    KeyCode::Char(c) if !ctrl && !alt => insert(&mut inp, c),
                    _ => {}
                }
                self.mode = Mode::Input(inp);
            }
        }
    }

    fn key_normal(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let wall = self.view == View::Wall;
        let stage = self.focus == Focus::Stage && !wall;
        match k.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.zoom {
                    self.zoom = false;
                } else if stage {
                    self.focus = Focus::Rail;
                } else if wall {
                    self.toggle_wall();
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.rebuild();
                } else if !self.marks.is_empty() {
                    self.marks.clear();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Enter => self.goto(),
            KeyCode::Up | KeyCode::Char('k') if wall => self.wall_move(0, -1),
            KeyCode::Down | KeyCode::Char('j') if wall => self.wall_move(0, 1),
            KeyCode::Left | KeyCode::Char('h') if wall => self.wall_move(-1, 0),
            KeyCode::Right | KeyCode::Char('l') if wall => self.wall_move(1, 0),
            KeyCode::Up | KeyCode::Char('k') if stage => self.scroll_stage(-1),
            KeyCode::Down | KeyCode::Char('j') if stage => self.scroll_stage(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.scroll_stage(-10),
            KeyCode::PageDown => self.scroll_stage(10),
            KeyCode::Home | KeyCode::Char('g') if stage => self.scroll_stage(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') if stage => self.scroll_stage(isize::MAX / 2),
            KeyCode::Home if wall => { if let Some(&i) = self.wall_items().first() { self.select(i); } }
            KeyCode::End if wall => { if let Some(&i) = self.wall_items().last() { self.select(i); } }
            KeyCode::Home => {
                if let Some(i) = (0..self.items.len()).find(|&i| self.selectable(i)) {
                    self.select(i);
                }
            }
            KeyCode::End => {
                let last = (0..self.items.len()).rev().find(|&i| self.selectable(i));
                if let Some(i) = last {
                    self.select(i);
                }
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab if !stage => self.focus = Focus::Stage,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Tab | KeyCode::BackTab => self.focus = Focus::Rail,
            KeyCode::Char('1') => self.set_tab(StageTab::Activity),
            KeyCode::Char('2') => self.set_tab(StageTab::Screen),
            KeyCode::Char('3') => self.set_tab(StageTab::Files),
            KeyCode::Char('m') => self.start_message(),
            KeyCode::Char('c') => self.continue_agent(),
            KeyCode::Char('i') => self.ask_interrupt(),
            KeyCode::Char('I') => self.ask_stop_all(),
            KeyCode::Char('x') => self.ask_close(),
            KeyCode::Char('r') => self.resume(),
            KeyCode::Char('n') => self.new_agent(),
            KeyCode::Char('t') if self.sel_pane().is_some() => {
                    if let Some(Item::Child { pane, .. }) = self.items.get(self.sel)
                        && let Some(parent) = self.items.iter().position(|it| matches!(it, Item::Agent(i) if i == pane)) { self.select(parent); }
                    if wall {
                        self.view = View::Detail;
                    }
                    self.tab_user = Some(StageTab::Screen);
                    self.screen_up = 0;
                    self.mode = Mode::Direct;
                    self.push_wants();
            }
            KeyCode::Char('s') => self.toggle_scope(),
            KeyCode::Char('v') => self.toggle_wall(),
            KeyCode::Char('a') => self.next_attention(),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('y') => self.copy_reply(),
            KeyCode::Char('e') => self.explain_selected(),
            KeyCode::Char('z') => self.zoom = !self.zoom && self.tab() == StageTab::Activity,
            KeyCode::Char('H') => {
                self.all_children = !self.all_children;
                self.rebuild();
            }
            KeyCode::Char('J') => self.detail_top = self.detail_top.saturating_add(3),
            KeyCode::Char('K') => self.detail_top = self.detail_top.saturating_sub(3),
            KeyCode::Char(' ') => {
                if let Some(p) = self.sel_pane().filter(|p| p.kind != Kind::Shell).map(|p| p.pane_id.clone()) {
                    if !self.marks.remove(&p) {
                        self.marks.insert(p);
                    }
                    self.move_sel(1);
                }
            }
            _ => {}
        }
    }

    fn wall_move(&mut self, dx: isize, dy: isize) {
        let cards = self.wall_items();
        if cards.is_empty() {
            return;
        }
        let cols = self.wall_cols.max(1) as isize;
        let cur = cards.iter().position(|&n| n == self.sel).unwrap_or(0) as isize;
        let next = cur + dx + dy * cols;
        if next >= 0 && (next as usize) < cards.len() {
            self.select(cards[next as usize]);
        } else if dy > 0 && cur / cols < (cards.len() as isize - 1) / cols {
            self.select(*cards.last().unwrap());
        }
    }

    /// Desplaza lo que enseña el escenario: eventos, pantalla o ficheros.
    fn scroll_stage(&mut self, d: isize) {
        if self.zoom {
            self.detail_top = self.detail_top.saturating_add_signed(d.clamp(-10_000, 10_000));
            return;
        }
        match self.tab() {
            StageTab::Activity => {
                let n = self.stage_events().len();
                if n == 0 {
                    return;
                }
                let next = (self.ev_sel as isize).saturating_add(d).clamp(0, n as isize - 1) as usize;
                self.ev_sel = next;
                self.follow = next + 1 == n;
                self.detail_top = 0;
            }
            // La pantalla se ancla abajo: subir es alejarse del final.
            StageTab::Screen => self.screen_up = self.screen_up.saturating_add_signed((-d).clamp(-10_000, 10_000)),
            StageTab::Files => self.files_top = self.files_top.saturating_add_signed(d.clamp(-10_000, 10_000)),
        }
    }

    // ───────────────────────────── ratón ─────────────────────────────

    fn on_mouse(&mut self, m: MouseEvent) {
        if let Mode::Explain(ex) = &mut self.mode { match m.kind {MouseEventKind::ScrollUp=>ex.scroll=ex.scroll.saturating_sub(3),MouseEventKind::ScrollDown=>ex.scroll=ex.scroll.saturating_add(3),_=>{}} return; }
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let pos = ratatui::layout::Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d: isize = if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                if self.view == View::Detail && self.stage_area.contains(pos) {
                    self.scroll_stage(d * 3);
                } else if self.view == View::Wall {
                    self.wall_move(0, d);
                } else {
                    self.move_sel(d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(hit) = self.hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, h)| *h) else { return };
                match hit {
                    Hit::Row(i) | Hit::Card(i) => {
                        let Some(key) = self.items.get(i).and_then(|it| self.key_of(it)) else { return; };
                        let double = self.last_click.as_ref().is_some_and(|(t, previous)| previous == &key && t.elapsed() < Duration::from_millis(400));
                        self.last_click = Some((Instant::now(), key));
                        self.select(i);
                        if matches!(hit, Hit::Row(_)) {
                            self.focus = Focus::Rail;
                        }
                        if double {
                            self.goto();
                        }
                    }
                    Hit::Tab(t) => self.set_tab(t),
                    Hit::Event(i) => {
                        let n = self.stage_events().len();
                        if i < n {
                            self.ev_sel = i;
                            self.follow = i + 1 == n;
                            self.detail_top = 0;
                            self.focus = Focus::Stage;
                        }
                    }
                    Hit::Pill(s) => self.jump_status(s),
                    Hit::Btn(Btn::Scope) => self.toggle_scope(),
                    Hit::Btn(Btn::Wall) => self.toggle_wall(),
                    Hit::Btn(Btn::Help) => self.mode = Mode::Help,
                    Hit::Btn(Btn::Quit) => self.quit = true,
                }
            }
            _ => {}
        }
    }
}

impl Drop for App { fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); if let Mode::Explain(ex)=&self.mode {ex.cancel.store(true,Ordering::Relaxed);} } }

fn insert(inp: &mut Input, c: char) {
    let at = byte_at(&inp.text, inp.cursor);
    inp.text.insert(at, c);
    inp.cursor += 1;
}

/// Posición en bytes del carácter número `i`.
pub fn byte_at(s: &str, i: usize) -> usize {
    s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(s.len())
}

fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_=/.,:@%+".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Nombre válido para herdr (`[a-z][a-z0-9_-]{0,31}`) que no esté ya en uso.
fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    let mut slug: String =
        base.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().trim_matches('-').to_string();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    if !slug.starts_with(|c: char| c.is_ascii_lowercase()) {
        slug = format!("a-{slug}");
    }
    slug.truncate(24);
    let slug = slug.trim_end_matches('-').to_string();
    if !taken.contains(&slug) {
        return slug;
    }
    (2..).map(|n| format!("{slug}-{n}")).find(|s| !taken.contains(s)).unwrap()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// ───────────────────────────── hilos de fondo ─────────────────────────────

/// Estado de herdr y pantallas de los paneles que la interfaz está enseñando.
fn spawn_poller(herdr: Herdr, tx: Sender<Msg>, wants: Arc<Mutex<Wants>>, refresh_ms: u64, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut last_snap: Option<Instant> = None;
        let mut seen: HashMap<String, u64> = HashMap::new();
        while !stop.load(Ordering::Relaxed) {
            let w = wants.lock().map(|w| w.clone()).unwrap_or_default();
            if last_snap.is_none_or(|t| t.elapsed() >= Duration::from_millis(refresh_ms)) {
                last_snap = Some(Instant::now());
                if tx.send(Msg::Snapshot(herdr.snapshot().map_err(|e| e.to_string()))).is_err() {
                    return;
                }
            }
            seen.retain(|k, _| w.screens.contains(k));
            for pane in &w.screens {
                let Ok(text) = herdr.read(pane, None, true) else { continue };
                let mut h = std::collections::hash_map::DefaultHasher::new();
                text.hash(&mut h);
                let h = h.finish();
                if seen.insert(pane.clone(), h) != Some(h) {
                    let mut lines = ansi::parse(&text);
                    while lines.last().is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty())) {
                        lines.pop();
                    }
                    if tx.send(Msg::Screen { pane: pane.clone(), lines: Arc::new(lines) }).is_err() {
                        return;
                    }
                }
            }
            let pause = if w.fast { 70 } else if w.screens.is_empty() { refresh_ms } else { 300.min(refresh_ms) };
            let started = Instant::now();
            let pause = Duration::from_millis(pause);
            while started.elapsed() < pause && !stop.load(Ordering::Relaxed) { std::thread::sleep(pause.saturating_sub(started.elapsed()).min(Duration::from_millis(100))); }
        }
    });
}

/// Transcripts de los agentes: abre la fuente de cada sesión pedida y la mantiene al día.
fn spawn_sessions(tx: Sender<Msg>, wants: Arc<Mutex<Wants>>, stop: Arc<AtomicBool>) {
    struct Open {
        session: SessionRef,
        kind: Kind,
        cwd: PathBuf,
        src: Option<Box<dyn sources::Source>>,
        tried: Instant,
        sent: bool,
    }
    std::thread::spawn(move || {
        let mut open: HashMap<String, Open> = HashMap::new();
        let mut round: usize = 0;
        while !stop.load(Ordering::Relaxed) {
            let mut restart = false;
            let w = wants.lock().map(|w| w.sessions.clone()).unwrap_or_default();
            open.retain(|k, _| w.iter().any(|x| &x.pane == k));
            for (n, want) in w.iter().enumerate() {
                // La seleccionada va en cada vuelta; el resto, por turnos, salvo su primera carga.
                let known = open.get(&want.pane).is_some_and(|o| o.session == want.session && o.kind == want.kind && o.cwd == want.cwd);
                if known && n > 0 && !round.wrapping_add(n).is_multiple_of(4) {
                    continue;
                }
                if !known {
                    open.insert(want.pane.clone(), Open { session: want.session.clone(), kind: want.kind, cwd: want.cwd.clone(), src: None, tried: Instant::now() - Duration::from_secs(60), sent: false });
                }
                let o = open.get_mut(&want.pane).unwrap();
                if o.src.is_none() {
                    if o.tried.elapsed() < Duration::from_secs(5) {
                        continue;
                    }
                    o.tried = Instant::now();
                    o.src = sources::open(want.kind, &want.session, &want.cwd);
                    if o.src.is_none() {
                        if !o.sent {
                            o.sent = true;
                            if tx.send(Msg::SessionLoaded { want: want.clone(), info: None }).is_err() {
                                return;
                            }
                        }
                        continue;
                    }
                    o.sent = false;
                }
                let src = o.src.as_mut().unwrap();
                let changed = match src.refresh() {
                    Ok(changed) => changed,
                    Err(error) => {
                        let _ = tx.send(Msg::Toast(ToastKind::Err, format!("No se pudo leer la sesión de {}: {error}", want.pane)));
                        if tx.send(Msg::SessionLoaded {want:want.clone(),info:None}).is_err() { return; }
                        o.src = None; o.tried = Instant::now(); o.sent = false;
                        continue;
                    }
                };
                if changed || !o.sent {
                    o.sent = true;
                    if tx.send(Msg::SessionLoaded { want: want.clone(), info: Some(Arc::new(src.info().clone())) }).is_err() {
                        return;
                    }
                }
                // Tras cada sesión se vuelve a mirar qué se pide: si el usuario cambia de agente
                // en plena carga inicial, el nuevo no espera a que acabe toda la ronda.
                if n == 0 || !known {
                    let now_first = wants.lock().ok().and_then(|w| w.sessions.first().map(|x| x.pane.clone()));
                    if now_first.as_ref() != w.first().map(|x| &x.pane) {
                        restart = true;
                        break;
                    }
                }
            }
            if restart {
                continue;
            }
            round = round.wrapping_add(1);
            std::thread::sleep(Duration::from_millis(350));
        }
    });
}

/// Acciones sobre herdr, en orden y fuera del hilo de la interfaz.
fn spawn_jobs(herdr: Herdr, tx: Sender<Msg>, rx: Receiver<Job>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let ok = |t: String| Msg::Toast(ToastKind::Ok, t);
        let err = |t: String| Msg::Toast(ToastKind::Err, t);
        for job in rx {
            if stop.load(Ordering::Relaxed) { break; }
            let msg = match job {
                Job::Direct { pane, input } => {
                    let r = match &input {
                        keys::Input::Key(k) => herdr.send_keys(&pane, std::slice::from_ref(k)),
                        keys::Input::Text(t) => herdr.send_text(&pane, t),
                    };
                    r.err().map(|e| err(format!("No se pudo enviar la tecla: {e}")))
                }
                Job::Prompt { panes, text } => {
                    let mut failed = vec![];
                    for (pane, name) in &panes {
                        if let Err(e) = herdr.prompt(pane, &text) {
                            failed.push(format!("{name}: {}", explain(&e.to_string())));
                        }
                    }
                    Some(if !failed.is_empty() {
                        err(failed.join(" · "))
                    } else if panes.len() == 1 {
                        ok(format!("Mensaje enviado a {}", panes[0].1))
                    } else {
                        ok(format!("Mensaje enviado a {} agentes", panes.len()))
                    })
                }
                Job::Interrupt { panes } => {
                    let mut failed = vec![];
                    for (pane, name, keys) in &panes {
                        for (n, k) in keys.iter().enumerate() {
                            if n > 0 {
                                std::thread::sleep(Duration::from_millis(180));
                            }
                            if let Err(e) = herdr.send_keys(pane, std::slice::from_ref(k)) {
                                failed.push(format!("{name}: {e}"));
                                break;
                            }
                        }
                    }
                    Some(if !failed.is_empty() {
                        err(failed.join(" · "))
                    } else if panes.len() == 1 {
                        ok(format!("{} interrumpido", panes[0].1))
                    } else {
                        ok(format!("{} agentes interrumpidos", panes.len()))
                    })
                }
                Job::Close { pane, name } => Some(match herdr.close(&pane) {
                    Ok(()) => ok(format!("Panel de {name} cerrado")),
                    Err(e) => err(format!("No se pudo cerrar {name}: {e}")),
                }),
                Job::Launch { pane, name, command } => Some(match herdr.at_shell(&pane) {
                    Ok(true) => match herdr.run(&pane, &command) {
                        Ok(()) => ok(format!("Lanzando el agente en {name}…")),
                        Err(e) => err(format!("No se pudo lanzar en {name}: {e}")),
                    },
                    Ok(false) => err(format!("{name} tiene un programa en marcha: no se lanza nada encima")),
                    Err(e) => err(format!("No se pudo comprobar {name}: {e}")),
                }),
                Job::Spawn { from, cwd, kind, name, args } => {
                    let r = herdr.split(&from, true, Some(&cwd)).and_then(|pane| {
                        // herdr tarda un instante en dejar lista la shell del panel nuevo.
                        std::thread::sleep(Duration::from_millis(400));
                        herdr.agent_start(&name, &kind, &pane, &args)
                    });
                    Some(match r {
                        Ok(()) => ok(format!("Agente {name} lanzado")),
                        Err(e) => err(format!("No se pudo lanzar {name}: {e}")),
                    })
                }
                Job::Goto { pane } => match herdr.focus(&pane) {
                    Ok(()) => Some(Msg::Quit),
                    Err(e) => Some(err(format!("No se pudo ir al panel: {e}"))),
                },
            };
            if let Some(m) = msg
                && tx.send(m).is_err()
            {
                return;
            }
        }
    });
}

/// Traduce los errores de herdr que el usuario puede resolver.
fn explain(e: &str) -> String {
    if e.contains("blocked") {
        "espera una respuesta; contéstale con t (control directo)".into()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> App {
        crate::demo::app(Config::default())
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '↑' => KeyCode::Up,
                '↓' => KeyCode::Down,
                '⏎' => KeyCode::Enter,
                '⎋' => KeyCode::Esc,
                c => KeyCode::Char(c),
            };
            app.on_event(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
    }

    fn sel_name(app: &App) -> String {
        match app.sel_child() {
            Some(c) => c.name.clone(),
            None => app.sel_pane().map(App::name).unwrap_or_default(),
        }
    }

    fn snapshot_without(app: &App, gone: &str) -> Snapshot {
        Snapshot { panes: app.panes.iter().filter(|p| p.pane_id != gone).cloned().collect(), focused_pane_id: None, focused_workspace_id: None }
    }

    #[test]
    fn al_abrir_elige_lo_que_pide_atencion() {
        let app = demo();
        assert_eq!(sel_name(&app), "frontend");
        assert_eq!(app.tab(), StageTab::Screen); // bloqueado: se enseña su pantalla
    }

    #[test]
    fn la_seleccion_sobrevive_a_los_refrescos() {
        let mut app = demo();
        press(&mut app, "↑"); // backend
        assert_eq!(sel_name(&app), "backend");
        press(&mut app, "3");
        let snap = snapshot_without(&app, "w1:p4");
        app.on_msg(Msg::Snapshot(Ok(snap)));
        assert_eq!(sel_name(&app), "backend");
        assert_eq!(app.tab(), StageTab::Files, "la pestaña elegida no se pierde al refrescar");
        // Desaparece el seleccionado: se pasa a un vecino válido.
        let snap = snapshot_without(&app, "w1:p2");
        app.on_msg(Msg::Snapshot(Ok(snap)));
        assert!(app.sel_pane().is_some());
        assert_ne!(sel_name(&app), "backend");
        // Sin paneles: nada seleccionado y ninguna tecla rompe.
        app.on_msg(Msg::Snapshot(Ok(Snapshot::default())));
        assert!(app.sel_pane().is_none() && app.items.is_empty());
        press(&mut app, "↓↑m⎋icxt⏎rn⎋v↓v /z123HaI");
    }

    #[test]
    fn subagente_que_desaparece_devuelve_al_padre() {
        let mut app = demo();
        press(&mut app, "↑↑"); // backend → segundo subagente de lider
        assert_eq!(sel_name(&app), "Qué exige PCI para datos de tarjeta");
        assert_eq!(app.stage_events().len(), 4);
        let mut info = (*app.sessions["w1:p1"]).clone();
        info.children.clear();
        app.on_msg(Msg::Session { pane: "w1:p1".into(), info: Some(Arc::new(info)) });
        assert_eq!(sel_name(&app), "lider");
    }

    #[test]
    fn marcar_y_escribir_a_varios() {
        let mut app = demo();
        press(&mut app, "↑ "); // marca backend y baja a frontend
        press(&mut app, " ");
        assert_eq!(app.marks.len(), 2);
        press(&mut app, "m");
        let Mode::Input(inp) = &app.mode else { panic!("debería abrir el mensaje") };
        assert_eq!(inp.targets.len(), 2);
        assert_eq!(inp.title, "Mensaje para 2 agentes");
        press(&mut app, "⎋⎋");
        assert!(app.marks.is_empty() && !app.quit, "el primer esc quita las marcas, no sale");
    }

    #[test]
    fn editor_de_mensajes() {
        let mut app = demo();
        press(&mut app, "↑mhola");
        let alt = |c| TermEvent::Key(KeyEvent::new(c, KeyModifiers::ALT));
        let ctrl = |c| TermEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        app.on_event(alt(KeyCode::Enter));
        app.on_event(TermEvent::Paste("año ñu".into()));
        app.on_event(TermEvent::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        app.on_event(TermEvent::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        let Mode::Input(inp) = &app.mode else { panic!() };
        assert_eq!((inp.text.as_str(), inp.cursor), ("hola\naño u", 9));
        app.on_event(ctrl('w'));
        let Mode::Input(inp) = &app.mode else { panic!() };
        assert_eq!((inp.text.as_str(), inp.cursor), ("hola\nu", 5));
        app.on_event(ctrl('a'));
        app.on_event(TermEvent::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)));
        app.on_event(ctrl('e'));
        app.on_event(ctrl('w'));
        let Mode::Input(inp) = &app.mode else { panic!() };
        assert_eq!(inp.text, "ola\n");
        app.on_event(ctrl('u'));
        press(&mut app, "⏎"); // vacío: se cierra sin enviar nada
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn ambito_filtro_y_mosaico() {
        let mut app = demo();
        assert_eq!(app.items.iter().filter(|i| matches!(i, Item::Agent(_))).count(), 5);
        press(&mut app, "s");
        assert_eq!(app.items.iter().filter(|i| matches!(i, Item::Agent(_))).count(), 7);
        assert_eq!(app.items.iter().filter(|i| matches!(i, Item::Group { .. })).count(), 2);
        assert_eq!(sel_name(&app), "frontend", "cambiar de ámbito no mueve la selección");
        press(&mut app, "/revisor⏎");
        assert_eq!(sel_name(&app), "revisor");
        press(&mut app, "/zzz");
        assert!(app.items.is_empty());
        press(&mut app, "⎋");
        assert_eq!(app.items.iter().filter(|i| matches!(i, Item::Agent(_))).count(), 7);
        // El mosaico no enseña shells ni subagentes.
        press(&mut app, "sv");
        assert_eq!(app.wall_items().len(), 4);
        app.wall_cols = 2;
        press(&mut app, "↓");
        assert!(app.wall_items().contains(&app.sel));
    }

    #[test]
    fn confirmaciones() {
        let mut app = demo();
        press(&mut app, "i"); // frontend está bloqueado, no trabajando
        assert!(matches!(app.mode, Mode::Normal));
        press(&mut app, "↑i");
        assert!(matches!(&app.mode, Mode::Confirm(c) if c.title == "¿Interrumpir a backend?"));
        press(&mut app, "n");
        assert!(matches!(app.mode, Mode::Normal));
        press(&mut app, "I");
        // No cuenta al líder: es el panel desde el que se abrió la vista.
        assert!(matches!(&app.mode, Mode::Confirm(c) if matches!(&c.action, Action::Interrupt(ids) if ids == &["w1:p2".to_string()])));
        press(&mut app, "⎋x");
        assert!(matches!(&app.mode, Mode::Confirm(c) if c.danger));
    }

    #[test]
    fn nombres_unicos() {
        let taken: HashSet<String> = ["codex".to_string(), "codex-2".to_string()].into();
        assert_eq!(unique_name("Codex", &taken), "codex-3");
        assert_eq!(unique_name("DeepSeek · Flash 4.1", &HashSet::new()), "deepseek-flash-4-1");
        assert_eq!(unique_name("4 agentes", &HashSet::new()), "a-4-agentes");
    }

    #[test]
    fn comillas_de_shell() {
        assert_eq!(sh_quote("--resume=abc"), "--resume=abc");
        assert_eq!(sh_quote("/a b/c"), "'/a b/c'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn base64_estandar() {
        assert_eq!(base64(b"hola"), "aG9sYQ==");
        assert_eq!(base64(b"hol"), "aG9s");
        assert_eq!(base64(b"ho"), "aG8=");
    }

    #[test]
    fn cursor_en_bytes() {
        assert_eq!(byte_at("añb", 2), 3);
        assert_eq!(byte_at("añb", 9), 4);
    }
    #[test]
    fn direct_mode_does_not_move_to_another_pane() {
        let mut app = demo();
        press(&mut app,"t"); assert!(matches!(app.mode,Mode::Direct));
        let gone = app.sel_pane().unwrap().pane_id.clone();
        app.on_msg(Msg::Snapshot(Ok(snapshot_without(&app,&gone))));
        assert!(matches!(app.mode,Mode::Normal));
    }

    #[test]
    fn changed_session_rejects_old_worker_results() {
        let mut app = demo();
        let p = app.sel_pane().unwrap().clone();
        let want = Want { pane:p.pane_id.clone(),kind:p.kind,session:p.session.clone().unwrap(),cwd:p.cwd.clone() };
        assert!(app.info(&p.pane_id).is_some());
        let mut snap = snapshot_without(&app,"absent");
        snap.panes.iter_mut().find(|x|x.pane_id==p.pane_id).unwrap().session=Some(SessionRef::Id("replacement".into()));
        app.on_msg(Msg::Snapshot(Ok(snap)));
        assert!(app.info(&p.pane_id).is_none());
        app.on_msg(Msg::SessionLoaded { want, info:Some(Arc::new(SessionInfo::default())) });
        assert!(app.info(&p.pane_id).is_none()); assert!(!app.probed.contains(&p.pane_id));
        app.on_msg(Msg::Screen {pane:"removed".into(),lines:Arc::new(vec![])});
        assert!(!app.screens.contains_key("removed"));
    }

    #[test]
    fn wall_home_end_and_shell_selection_are_cards() {
        let mut app = demo();
        let shell = app.items.iter().position(|it|matches!(it,Item::Agent(i) if app.panes[*i].kind==Kind::Shell)).unwrap();
        app.select(shell); press(&mut app,"v");
        assert!(app.wall_items().contains(&app.sel));
        app.on_event(TermEvent::Key(KeyEvent::new(KeyCode::End,KeyModifiers::NONE)));
        assert_eq!(app.wall_items().last().copied(),Some(app.sel));
        app.on_event(TermEvent::Key(KeyEvent::new(KeyCode::Home,KeyModifiers::NONE)));
        assert_eq!(app.wall_items().first().copied(),Some(app.sel));
    }

    #[test]
    fn bounded_history_keeps_selected_event_identity() {
        let mut app = demo(); press(&mut app,"↑1");
        let pane = app.sel_pane().unwrap().pane_id.clone();
        let mut info = app.info(&pane).unwrap().clone();
        let template = info.events[0].clone();
        info.events = (0..EVENTS_MAX).map(|n| Event {ts:n as i64,title:format!("event {n}"),..template.clone()}).collect();
        app.on_msg(Msg::Session {pane:pane.clone(),info:Some(Arc::new(info.clone()))});
        app.follow=false; app.ev_sel=10;
        info.events.remove(0);info.events.push(Event {ts:EVENTS_MAX as i64,title:"new".into(),..template});
        app.on_msg(Msg::Session {pane,info:Some(Arc::new(info))});
        assert_eq!(app.ev_sel,9); assert_eq!(app.stage_events()[app.ev_sel].title,"event 10");
    }

    #[test]
    fn idle_session_worker_stops_when_app_drops() {
        let app = demo(); let stop = app.stop.clone();
        let (tx,rx)=mpsc::channel();
        spawn_sessions(tx,Arc::new(Mutex::new(Wants::default())),stop.clone());
        drop(app); assert!(stop.load(Ordering::Relaxed));
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)),Err(mpsc::RecvTimeoutError::Disconnected)));
    }

    #[test]
    fn extreme_stall_settings_and_scroll_do_not_overflow() {
        let mut app=demo();app.cfg.stalled_minutes=u64::MAX;
        let p=app.panes.iter().find(|p|p.status==Status::Working).unwrap().clone();
        let mut info=app.info(&p.pane_id).unwrap().clone();info.updated_at=Some(i64::MIN);
        app.sessions.insert(p.pane_id.clone(),Arc::new(info));assert!(app.stalled(&p).is_none());
        app.detail_top=usize::MAX;press(&mut app,"J");assert_eq!(app.detail_top,usize::MAX);
    }

    #[test]
    fn direct_control_from_child_shows_parent_screen() {
        let mut app=demo();press(&mut app,"↑↑");assert!(app.sel_child().is_some());press(&mut app,"t");
        assert!(matches!(app.mode,Mode::Direct));assert!(app.sel_child().is_none());assert_eq!(app.tab(),StageTab::Screen);
    }

    #[test]
    fn double_click_tracks_identity_after_row_replacement() {
        let mut app=demo();let (tx,rx)=mpsc::channel();app.jobs=tx;
        let row=app.sel;let gone=app.sel_pane().unwrap().pane_id.clone();
        app.hits=vec![(Rect::new(0,0,10,1),Hit::Row(row))];
        let click=MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),column:1,row:0,modifiers:KeyModifiers::NONE};
        app.on_mouse(click);app.on_msg(Msg::Snapshot(Ok(snapshot_without(&app,&gone))));
        app.hits=vec![(Rect::new(0,0,10,1),Hit::Row(row))];app.on_mouse(click);
        assert!(rx.try_recv().is_err(),"una fila distinta no completa el doble clic anterior");
    }

    #[test]
    fn explanation_uses_selected_child_ignores_stale_results_and_cancels() {
        let mut app=demo();press(&mut app,"↑↑e");
        let Mode::Explain(ex)=&app.mode else {panic!("explicación esperada");};
        assert!(ex.title.contains("PCI"));assert_eq!(ex.model,"deepseek-v4.1-flash");assert_eq!(ex.effort,"max");let id=ex.id;let cancel=ex.cancel.clone();
        app.on_msg(Msg::Explanation{id:id+1,text:Ok("wrong".into()),done:true});
        let Mode::Explain(ex)=&mut app.mode else {panic!();};assert!(ex.text.is_empty());ex.error=None;
        app.on_msg(Msg::Explanation{id,text:Ok("Hechos observados".into()),done:true});press(&mut app,"⎋");assert!(cancel.load(Ordering::Relaxed));
        press(&mut app,"e");let Mode::Explain(ex)=&app.mode else {panic!();};assert_eq!(ex.text,"Hechos observados");assert!(!ex.loading);assert!(ex.error.is_none());
        app.on_msg(Msg::Explanation{id,text:Ok("stale".into()),done:true});let Mode::Explain(ex)=&app.mode else {panic!();};assert_eq!(ex.text,"Hechos observados");
    }

}
