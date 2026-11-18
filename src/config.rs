//! Configuración: `~/.config/herdr-agentview/config.toml`. Todo es opcional.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Cada cuánto se refresca el estado de herdr, en milisegundos (mínimo 200).
    pub refresh_ms: u64,
    /// Ámbito inicial: `workspace` (solo el espacio desde el que se abre) o `all`.
    pub scope: String,
    /// Texto que envía la acción «continuar».
    pub continue_prompt: String,
    /// Minutos sin actividad con el agente «trabajando» a partir de los cuales se marca como atascado.
    pub stalled_minutes: u64,
    /// Cómo se lanza, reanuda e interrumpe cada tipo de agente. Clave: `claude`, `codex`, `opencode`, `omp`.
    pub agents: BTreeMap<String, AgentCfg>,
    /// Agentes que ofrece el diálogo «nuevo agente».
    pub presets: Vec<Preset>,
    pub ai: AiConfig,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentCfg {
    /// Comando para arrancar una sesión nueva (argv).
    pub fresh: Vec<String>,
    /// Comando para reanudar una sesión (argv). `{session}` se sustituye por el id o la ruta de
    /// la sesión y `{cwd}` por el directorio del panel.
    pub resume: Vec<String>,
    /// Teclas que interrumpen el turno en curso, en nombres de herdr (`escape`, `ctrl+c`).
    pub interrupt: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preset {
    /// Nombre que se enseña en el diálogo.
    pub name: String,
    /// Tipo de agente de herdr (`claude`, `codex`, `opencode`, `omp`…).
    pub kind: String,
    /// Argumentos extra para el agente (modelo, permisos…).
    pub args: Vec<String>,
}

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

impl Default for Config {
    fn default() -> Self {
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".into(),
            AgentCfg { fresh: v(&["claude"]), resume: v(&["claude", "--resume", "{session}"]), interrupt: v(&["escape"]) },
        );
        agents.insert(
            "codex".into(),
            AgentCfg { fresh: v(&["codex"]), resume: v(&["codex", "resume", "-C", "{cwd}", "{session}"]), interrupt: v(&["escape"]) },
        );
        agents.insert(
            "opencode".into(),
            AgentCfg {
                fresh: v(&["opencode"]),
                resume: v(&["opencode", "--session", "{session}"]),
                interrupt: v(&["escape", "escape"]),
            },
        );
        agents.insert(
            "omp".into(),
            AgentCfg { fresh: v(&["omp"]), resume: v(&["omp", "--resume={session}"]), interrupt: v(&["escape"]) },
        );
        Config {
            ai: AiConfig::default(),
            refresh_ms: 700,
            scope: "workspace".into(),
            continue_prompt: "continúa".into(),
            stalled_minutes: 10,
            agents,
            presets: vec![
                Preset { name: "Claude Code".into(), kind: "claude".into(), args: vec![] },
                Preset { name: "Codex".into(), kind: "codex".into(), args: vec![] },
                Preset { name: "OpenCode".into(), kind: "opencode".into(), args: vec![] },
            ],
        }
    }
}

/// Carga la configuración; cualquier problema devuelve los valores por defecto.
pub fn load() -> Config {
    let path = config_path(std::env::var_os("XDG_CONFIG_HOME").as_deref(), std::env::var_os("HOME").as_deref());
    let mut cfg = match path.map(std::fs::read_to_string) {
        Some(Ok(src)) => parse(&src),
        _ => Config::default(),
    };
    cfg.ai = AiConfig::from_values(|key| std::env::var(key).ok());
    if cfg.ai.api_key.is_none() && cfg.ai.error.is_none() && cfg.ai.url == AI_URL {
        let root = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()).map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")));
        cfg.ai.api_key = root.and_then(|p| std::fs::read_to_string(p.join("opencode/auth.json")).ok())
            .and_then(|raw| shared_ai_key(&raw));
    }
    cfg
}

fn shared_ai_key(raw: &str) -> Option<String> {
    let auth: serde_json::Value = serde_json::from_str(raw).ok()?;
    ["herdr-gitview", "opencode-go"].into_iter().find_map(|entry| auth.get(entry).and_then(|v|v.get("key")).and_then(serde_json::Value::as_str).filter(|s|!s.trim().is_empty()).map(str::to_owned))
}

pub const AI_URL: &str = "https://opencode.ai/inference/go/openai/v1/chat/completions";
pub const AI_ENV_KEYS: &[&str] = &["AGENTVIEW_AI_MODEL", "AGENTVIEW_AI_EFFORT", "AGENTVIEW_AI_API_URL", "AGENTVIEW_AI_API_KEY_VALUE"];

#[derive(Clone, PartialEq)]
pub struct AiConfig {
    pub model: String,
    pub effort: String,
    pub url: String,
    pub api_key: Option<String>,
    pub error: Option<String>,
}
impl std::fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("AiConfig").field("model", &self.model).field("effort", &self.effort).finish_non_exhaustive() }
}
impl Default for AiConfig {
    fn default() -> Self { Self { model: "deepseek-v4.1-flash".into(), effort: "max".into(), url: AI_URL.into(), api_key: None, error: None } }
}
impl AiConfig {
    fn from_values(mut env: impl FnMut(&str) -> Option<String>) -> Self {
        let mut cfg = Self::default();
        for (key, target) in [(AI_ENV_KEYS[0], &mut cfg.model), (AI_ENV_KEYS[1], &mut cfg.effort), (AI_ENV_KEYS[2], &mut cfg.url)] {
            if let Some(value) = env(key) {
                if value.trim().is_empty() { cfg.error = Some(format!("{key} está vacía")); }
                else { *target = value.trim().into(); }
            }
        }
        cfg.api_key = env(AI_ENV_KEYS[3]);
        if cfg.api_key.as_ref().is_some_and(|v| v.trim().is_empty() || v.chars().any(char::is_control)) { cfg.error = Some("AGENTVIEW_AI_API_KEY_VALUE no es válida".into()); }
        if !matches!(cfg.effort.as_str(), "none" | "minimal" | "low" | "medium" | "high" | "max" | "xhigh") { cfg.error = Some("AGENTVIEW_AI_EFFORT: usa none, minimal, low, medium, high, max o xhigh".into()); }
        if !(cfg.url.starts_with("https://") || cfg.url.starts_with("http://")) || cfg.url.chars().any(char::is_control) { cfg.error = Some("AGENTVIEW_AI_API_URL: se esperaba una URL HTTP(S)".into()); }
        cfg
    }
}

fn config_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    match xdg.filter(|x| !x.is_empty()) {
        Some(x) => Some(Path::new(x).join("herdr-agentview/config.toml")),
        None => home.map(|h| Path::new(h).join(".config/herdr-agentview/config.toml")),
    }
}

/// Mezcla el TOML con los valores por defecto. Lo que no encaja se ignora.
fn parse(src: &str) -> Config {
    let mut cfg = Config::default();
    let Ok(doc) = src.parse::<Table>() else { return cfg };

    if let Some(n) = doc.get("refresh_ms").and_then(Value::as_integer) {
        cfg.refresh_ms = (n.max(0) as u64).max(200);
    }
    if let Some(s) = doc.get("scope").and_then(Value::as_str)
        && matches!(s, "workspace" | "all")
    {
        cfg.scope = s.to_string();
    }
    if let Some(s) = doc.get("continue_prompt").and_then(Value::as_str) {
        cfg.continue_prompt = s.to_string();
    }
    if let Some(n) = doc.get("stalled_minutes").and_then(Value::as_integer) {
        cfg.stalled_minutes = n.max(0) as u64;
    }
    if let Some(agents) = doc.get("agents").and_then(Value::as_table) {
        for (name, val) in agents {
            let Some(t) = val.as_table() else { continue };
            let a = cfg.agents.entry(name.clone()).or_default();
            if let Some(v) = strings(t.get("fresh")) {
                a.fresh = v;
            }
            if let Some(v) = strings(t.get("resume")) {
                a.resume = v;
            }
            if let Some(v) = strings(t.get("interrupt")) {
                a.interrupt = v;
            }
        }
    }
    if let Some(arr) = doc.get("presets").and_then(Value::as_array) {
        let list: Vec<Preset> = arr.iter().filter_map(preset).collect();
        if !list.is_empty() {
            cfg.presets = list;
        }
    }
    cfg
}

/// Array de cadenas; `None` si la clave falta o no es eso.
fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    let arr = v?.as_array()?;
    arr.iter().map(|x| x.as_str().map(str::to_string)).collect()
}

fn preset(v: &Value) -> Option<Preset> {
    let t = v.as_table()?;
    let name = t.get("name")?.as_str()?;
    let kind = t.get("kind")?.as_str()?;
    if name.trim().is_empty() || kind.trim().is_empty() {
        return None;
    }
    Some(Preset { name: name.into(), kind: kind.into(), args: strings(t.get("args")).unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacio_es_por_defecto() {
        assert_eq!(parse(""), Config::default());
        assert_eq!(parse("# solo comentarios\n"), Config::default());
    }

    #[test]
    fn claves_simples() {
        let c = parse("refresh_ms = 5000\nscope = \"all\"\ncontinue_prompt = \"sigue\"\nstalled_minutes = 3\n");
        assert_eq!(c.refresh_ms, 5000);
        assert_eq!(c.scope, "all");
        assert_eq!(c.continue_prompt, "sigue");
        assert_eq!(c.stalled_minutes, 3);
    }

    #[test]
    fn minimos_y_limites() {
        assert_eq!(parse("refresh_ms = 50").refresh_ms, 200);
        assert_eq!(parse("refresh_ms = 200").refresh_ms, 200);
        assert_eq!(parse("refresh_ms = -5").refresh_ms, 200);
        assert_eq!(parse("scope = \"otro\"").scope, "workspace");
        assert_eq!(parse("stalled_minutes = 0").stalled_minutes, 0);
        assert_eq!(parse("stalled_minutes = -1").stalled_minutes, 0);
    }

    #[test]
    fn tipos_equivocados() {
        let src = r#"
            refresh_ms = "700"
            scope = 1
            continue_prompt = true
            stalled_minutes = "10"
            agents = 3
            presets = "x"
        "#;
        assert_eq!(parse(src), Config::default());
        // Una clave mala no arrastra a las buenas.
        let c = parse("refresh_ms = \"700\"\nscope = \"all\"");
        assert_eq!((c.refresh_ms, c.scope.as_str()), (700, "all"));
    }

    #[test]
    fn agentes() {
        let c = parse(
            r#"
            [agents.claude]
            fresh = ["claude", "--permission-mode", "bypassPermissions"]
            resume = ["claude", "r", "{session}"]

            [agents.nuevo]
            fresh = ["nuevo"]
            "#,
        );
        let claude = &c.agents["claude"];
        assert_eq!(claude.fresh, ["claude", "--permission-mode", "bypassPermissions"]);
        assert_eq!(claude.resume, ["claude", "r", "{session}"]);
        assert_eq!(claude.interrupt, ["escape"]); // no estaba: se conserva el de por defecto
        assert_eq!(
            c.agents["nuevo"],
            AgentCfg { fresh: v(&["nuevo"]), resume: vec![], interrupt: vec![] }
        );
        assert_eq!(c.agents["codex"], Config::default().agents["codex"]);
    }

    #[test]
    fn agentes_con_tipos_equivocados() {
        let c = parse("[agents.codex]\nfresh = \"codex\"\ninterrupt = [\"ctrl+c\"]");
        assert_eq!(c.agents["codex"].fresh, ["codex"]); // el de por defecto
        assert_eq!(c.agents["codex"].interrupt, ["ctrl+c"]);
        let c = parse("agents = { claude = 3 }");
        assert_eq!(c.agents["claude"], Config::default().agents["claude"]);
    }

    #[test]
    fn presets() {
        let c = parse(
            r#"
            [[presets]]
            name = "DeepSeek Flash"
            kind = "opencode"
            args = ["--model", "ollama-cloud/deepseek-v4.1-flash"]

            [[presets]]
            name = "Sin args"
            kind = "codex"
            "#,
        );
        assert_eq!(c.presets.len(), 2);
        assert_eq!(c.presets[0].name, "DeepSeek Flash");
        assert_eq!(c.presets[0].args, ["--model", "ollama-cloud/deepseek-v4.1-flash"]);
        assert_eq!(
            c.presets[1],
            Preset { name: "Sin args".into(), kind: "codex".into(), args: vec![] }
        );
    }

    #[test]
    fn presets_invalidos_dejan_los_de_por_defecto() {
        let d = Config::default().presets;
        assert_eq!(parse("[[presets]]\nname = \"\"\nkind = \"x\"").presets, d);
        assert_eq!(parse("[[presets]]\nname = \" \"").presets, d);
        assert_eq!(parse("[[presets]]\nname = \"x\"").presets, d);
        assert_eq!(parse("[[presets]]\nname = 1\nkind = \"x\"").presets, d);
        assert_eq!(parse("presets = [\"x\"]").presets, d);
        // Uno válido basta para sustituir la lista entera.
        let c = parse("[[presets]]\nname = \"x\"\nkind = \"y\"\nargs = \"no\"");
        assert_eq!(c.presets, [Preset { name: "x".into(), kind: "y".into(), args: vec![] }]);
    }

    #[test]
    fn toml_invalido() {
        assert_eq!(parse("refresh_ms = "), Config::default());
        assert_eq!(parse("[agents.claude\nfresh = []"), Config::default());
        assert_eq!(parse("[[[presets]]]"), Config::default());
    }

    #[test]
    fn ruta() {
        let xdg = config_path(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/u")));
        assert_eq!(xdg, Some(PathBuf::from("/xdg/herdr-agentview/config.toml")));
        let vacio = config_path(Some(OsStr::new("")), Some(OsStr::new("/home/u")));
        assert_eq!(vacio, Some(PathBuf::from("/home/u/.config/herdr-agentview/config.toml")));
        assert_eq!(config_path(None, Some(OsStr::new("/home/u"))), vacio);
        assert_eq!(config_path(None, None), None);
    }
    #[test]
    fn ai_environment_contract_and_defaults() {
        let cfg=AiConfig::from_values(|_|None);assert_eq!(cfg.model,"deepseek-v4.1-flash");assert_eq!(cfg.effort,"max");
        let cfg=AiConfig::from_values(|k| match k {"AGENTVIEW_AI_MODEL"=>Some("another".into()),"AGENTVIEW_AI_EFFORT"=>Some("high".into()),_=>None});
        assert_eq!((cfg.model.as_str(),cfg.effort.as_str()),("another","high"));
        for key in AI_ENV_KEYS { assert!(AiConfig::from_values(|k|(k==*key).then(String::new)).error.is_some()); }
        assert!(AiConfig::from_values(|k|(k=="AGENTVIEW_AI_EFFORT").then(||"wrong".into())).error.is_some());
        let keys:Vec<_>=include_str!("../.env.example").lines().filter(|s|!s.starts_with('#')&&!s.trim().is_empty()).map(|s|s.split_once('=').unwrap().0).collect();
        assert_eq!(keys,AI_ENV_KEYS);
        let cfg=AiConfig {api_key:Some("private-test-key".into()),..AiConfig::default()};assert!(!format!("{cfg:?}").contains("private-test-key"));
    }

    #[test]
    fn shared_ai_credential_uses_existing_gitview_entry() {
        assert_eq!(shared_ai_key(r#"{"herdr-gitview":{"key":"shared-test"},"opencode-go":{"key":"legacy-test"}}"#).as_deref(),Some("shared-test"));
        assert_eq!(shared_ai_key(r#"{"opencode-go":{"key":"fallback-test"}}"#).as_deref(),Some("fallback-test"));
        assert!(shared_ai_key("invalid").is_none());
    }

}
