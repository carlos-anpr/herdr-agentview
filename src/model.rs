//! Tipos compartidos: el contrato entre herdr, las fuentes de sesión y la UI.
//!
//! Dos orígenes de datos, que la app cruza por `Pane::session`:
//! - `Pane`: lo que herdr sabe de cada panel (estado, título, cwd). Lo rellena `herdr.rs`.
//! - `SessionInfo`: lo que el propio agente deja escrito en disco (transcript). Lo rellenan
//!   las fuentes de `sources/`, una por tipo de agente.

use std::path::PathBuf;

/// Tope de `SessionInfo::events` (se conservan los más recientes).
pub const EVENTS_MAX: usize = 300;
/// Tope de `Child::events`.
pub const CHILD_EVENTS_MAX: usize = 120;
/// Tope en caracteres de `Event::title`.
pub const TITLE_MAX: usize = 200;
/// Tope en caracteres de `Event::detail`, `last_reply` y similares.
pub const DETAIL_MAX: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Claude,
    Codex,
    OpenCode,
    Omp,
    /// Agente que herdr detecta pero para el que no hay fuente de sesión (gemini, cursor…).
    Other,
    /// Panel sin agente: una shell o un comando suelto.
    Shell,
}

impl Kind {
    pub fn from_herdr(agent: Option<&str>) -> Kind {
        match agent {
            Some("claude") => Kind::Claude,
            Some("codex") => Kind::Codex,
            Some("opencode") => Kind::OpenCode,
            Some("omp") | Some("pi") => Kind::Omp,
            Some(_) => Kind::Other,
            None => Kind::Shell,
        }
    }
}

/// Estado del agente según herdr. El orden es el de prioridad visual (lo que pide atención, antes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    Blocked,
    Working,
    Done,
    Idle,
    Unknown,
}

impl Status {
    pub fn from_herdr(s: &str) -> Status {
        match s {
            "blocked" => Status::Blocked,
            "working" => Status::Working,
            "done" => Status::Done,
            "idle" => Status::Idle,
            _ => Status::Unknown,
        }
    }
}

/// Referencia a la sesión del agente que da herdr (`agent_session`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SessionRef {
    /// Identificador de sesión (claude, codex, opencode).
    Id(String),
    /// Ruta directa al fichero de sesión (omp).
    Path(PathBuf),
}

/// Un panel de herdr.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Contrato compartido: conserva todos los metadatos de herdr aunque la UI no los pinte.
pub struct Pane {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    pub workspace_label: String,
    pub workspace_number: u32,
    pub tab_label: String,
    /// Nombre que le puso el usuario o el orquestador (`agent rename` / `pane rename`).
    pub label: Option<String>,
    pub kind: Kind,
    /// Tipo de agente tal cual lo da herdr (para `Kind::Other`).
    pub agent_raw: Option<String>,
    pub status: Status,
    /// Título del terminal sin adornos.
    pub title: String,
    pub cwd: PathBuf,
    pub session: Option<SessionRef>,
    /// Tipo de agente dueño de `session` (sigue ahí aunque el agente haya salido y el panel
    /// vuelva a ser una shell): permite reanudarlo.
    pub session_agent: Option<String>,
    pub focused: bool,
    /// Contador de cambios de salida del panel.
    pub revision: u64,
}

/// Foto del servidor herdr.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub panes: Vec<Pane>,
    pub focused_pane_id: Option<String>,
    pub focused_workspace_id: Option<String>,
}

// ───────────────────────── sesión del agente ─────────────────────────

/// Lo que se sabe de una sesión de agente leyendo su transcript.
///
/// Todos los campos son opcionales o vacíos por defecto: cada fuente rellena lo que su
/// formato permite y deja el resto como está. Los instantes son segundos Unix (UTC).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionInfo {
    /// Título de la sesión si el agente lo guarda.
    pub title: Option<String>,
    /// Modelo en uso, tal cual lo escribe el agente (`claude-opus-5-5`, `gpt-6.1-sol`…).
    pub model: Option<String>,
    /// Esfuerzo o variante de razonamiento (`high`, `medium`, `max`…).
    pub effort: Option<String>,
    /// Último mensaje escrito por el usuario (o por quien delega), en una línea (`TITLE_MAX`).
    pub last_prompt: Option<String>,
    /// Último texto de respuesta del asistente, completo salvo el tope `DETAIL_MAX`.
    pub last_reply: Option<String>,
    pub started_at: Option<i64>,
    /// Instante de la última entrada del transcript.
    pub updated_at: Option<i64>,
    /// Instante del último mensaje del usuario: inicio del turno en curso (o del último turno).
    pub turn_started_at: Option<i64>,
    /// Tokens de entrada acumulados en la sesión (incluye los leídos de caché).
    pub tokens_in: u64,
    /// Tokens de salida acumulados (incluye razonamiento).
    pub tokens_out: u64,
    /// Tokens que ocupa ahora mismo la ventana de contexto (última petición al modelo).
    pub context_used: Option<u64>,
    /// Tamaño de la ventana de contexto del modelo, si el transcript lo dice.
    pub context_window: Option<u64>,
    /// Coste acumulado en dólares, solo si el agente lo deja escrito (no se estima).
    pub cost_usd: Option<f64>,
    /// Cuotas de uso que el agente deja escritas (p. ej. Codex: ventana de 5 h y semanal).
    pub limits: Vec<Limit>,
    /// Lista de tareas vigente del agente (la última que escribió), en su orden.
    pub todos: Vec<Todo>,
    /// Actividad en orden cronológico; como mucho `EVENTS_MAX`, los más recientes.
    pub events: Vec<Event>,
    /// Subagentes internos lanzados desde esta sesión, en orden de arranque.
    pub children: Vec<Child>,
    /// Ficheros que la sesión ha modificado (Edit/Write/patch), sin duplicados, por última edición.
    pub files: Vec<FileTouch>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    /// Etiqueta corta: `5h`, `7d`…
    pub name: String,
    /// Porcentaje consumido, 0–100.
    pub used_pct: f32,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub text: String,
    pub state: TodoState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoState {
    Pending,
    Active,
    Done,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileTouch {
    /// Ruta ya abreviada con `util::rel_path`.
    pub path: String,
    /// Número de ediciones en la sesión.
    pub edits: u32,
    pub last_ts: i64,
}

/// Una acción del agente o un mensaje, ya resumido para pintarlo en una línea.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub ts: i64,
    pub kind: EventKind,
    /// Una sola línea, sin saltos ni espacios repetidos, como mucho `TITLE_MAX` caracteres.
    /// Es el OBJETO de la acción, no el nombre de la herramienta (ver `EventKind`).
    pub title: String,
    /// Texto ampliado opcional (puede tener saltos de línea), como mucho `DETAIL_MAX`.
    pub detail: Option<String>,
    pub state: EventState,
    /// Duración si se conoce (herramienta terminada).
    pub duration_ms: Option<u64>,
    /// Para `EventKind::Task`: `Child::id` del subagente lanzado, si se puede enlazar.
    pub child: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventState {
    /// Herramienta lanzada y todavía sin resultado.
    Running,
    Ok,
    Error,
}

/// Tipo de evento. Entre paréntesis, qué va en `Event::title` y en `Event::detail`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// Mensaje del usuario (title: primera línea útil · detail: texto completo si es más largo).
    Prompt,
    /// Texto de respuesta del asistente (title: primera línea útil · detail: texto completo).
    Reply,
    /// Lectura de un fichero (title: ruta abreviada).
    Read,
    /// Creación o modificación de un fichero (title: ruta abreviada · detail: `+12 −3` si se sabe).
    Edit,
    /// Comando de shell (title: la descripción si la herramienta la trae y, si no, el comando en
    /// una línea · detail: el comando completo).
    Shell,
    /// Búsqueda en ficheros: grep, glob, find (title: el patrón y, si cabe, dónde).
    Search,
    /// Búsqueda o descarga web (title: la consulta o la URL).
    Web,
    /// Lanzamiento de un subagente (title: descripción de la tarea · child: su id).
    Task,
    /// El agente actualiza su lista de tareas (title: la tarea activa, o `3/7 hechas`).
    Todo,
    /// Cualquier otra herramienta, MCP incluidas (title: `nombre · resumen del argumento principal`).
    Tool,
    /// Aviso del sistema: compactación de contexto, cambio de modelo, error de API…
    Notice,
}

/// Subagente interno de una sesión (herramienta Agent/Task de Claude Code, sesión hija de OpenCode…).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Child {
    /// Identificador estable dentro de la sesión padre.
    pub id: String,
    /// Descripción corta de la tarea encargada.
    pub name: String,
    /// Tipo de subagente (`Explore`, `sonnet-worker`, `general`…).
    pub agent_type: Option<String>,
    pub model: Option<String>,
    /// Sigue en marcha.
    pub running: bool,
    pub started_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Actividad del subagente; como mucho `CHILD_EVENTS_MAX`, los más recientes.
    pub events: Vec<Event>,
    /// Último texto de respuesta del subagente (su informe final cuando termina).
    pub last_reply: Option<String>,
}
