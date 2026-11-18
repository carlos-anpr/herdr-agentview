//! Fuentes de sesión: leen lo que cada agente deja escrito en disco y lo normalizan a
//! `SessionInfo`. Una fuente por tipo de agente; todas incrementales y de solo lectura.

pub mod claude;
pub mod codex;
pub mod jsonl;
pub mod omp;
pub mod opencode;

use crate::model::{Kind, SessionInfo, SessionRef};
use std::path::Path;

/// Sesión abierta de un agente.
pub trait Source: Send {
    /// Lee lo que haya cambiado desde la última llamada y actualiza `info()`. Devuelve `true`
    /// si `info()` ha cambiado. Sin cambios en disco debe costar un `stat` (o una consulta
    /// mínima). La primera llamada hace la carga completa.
    fn refresh(&mut self) -> anyhow::Result<bool>;
    fn info(&self) -> &SessionInfo;
}

/// Abre la fuente que corresponde al tipo de agente. `cwd` es el directorio del panel: sirve
/// para abreviar rutas (`util::rel_path`) y para localizar la sesión si hace falta.
/// `None` si el tipo no tiene fuente o no se encuentra la sesión.
pub fn open(kind: Kind, session: &SessionRef, cwd: &Path) -> Option<Box<dyn Source>> {
    match kind {
        Kind::Claude => claude::open(session, cwd).ok(),
        Kind::Codex => codex::open(session, cwd).ok(),
        Kind::OpenCode => opencode::open(session, cwd).ok(),
        Kind::Omp => omp::open(session, cwd).ok(),
        Kind::Other | Kind::Shell => None,
    }
}
