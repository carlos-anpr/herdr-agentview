//! Cliente del socket de herdr: JSON por líneas, una petición por conexión.

use crate::model::{Kind, Pane, SessionRef, Snapshot, Status};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Herdr {
    sock: PathBuf,
}

impl Herdr {
    pub fn from_env() -> Herdr {
        let sock = std::env::var_os("HERDR_SOCKET_PATH").filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            match std::env::var("HERDR_SESSION").ok().filter(|s| !s.is_empty()) {
                Some(s) => home.join(".config/herdr/sessions").join(s).join("herdr.sock"),
                None => home.join(".config/herdr/herdr.sock"),
            }
        });
        Herdr { sock }
    }

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut s = UnixStream::connect(&self.sock).with_context(|| format!("no se puede conectar con herdr en {}", self.sock.display()))?;
        s.set_read_timeout(Some(Duration::from_secs(8)))?;
        s.set_write_timeout(Some(Duration::from_secs(4)))?;
        let mut req = serde_json::to_vec(&json!({ "id": "agentview", "method": method, "params": params }))?;
        req.push(b'\n');
        s.write_all(&req)?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).with_context(|| format!("herdr no responde a {method}"))?;
        let v: Value = serde_json::from_str(&line).with_context(|| format!("respuesta ilegible de herdr a {method}"))?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let msg = e["message"].as_str().or_else(|| e["code"].as_str()).unwrap_or("error");
            return Err(anyhow!("{msg}"));
        }
        v.get("result").cloned().ok_or_else(|| anyhow!("respuesta de herdr sin result"))
    }

    /// Estado vivo de todos los paneles.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let r = self.call("session.snapshot", json!({}))?;
        let snapshot = r.get("snapshot").filter(|s| s.get("panes").is_some_and(Value::is_array)).ok_or_else(|| anyhow!("snapshot de herdr inválida"))?;
        Ok(parse_snapshot(snapshot))
    }

    /// Texto del terminal de un panel. Con `ansi` conserva colores y estilos (secuencias SGR).
    pub fn read(&self, pane_id: &str, lines: Option<u32>, ansi: bool) -> Result<String> {
        let r = self.call(
            "pane.read",
            json!({ "pane_id": pane_id, "source": "visible", "lines": lines,
                    "format": if ansi { "ansi" } else { "text" }, "strip_ansi": !ansi }),
        )?;
        Ok(r["read"]["text"].as_str().unwrap_or_default().to_string())
    }

    pub fn send_keys(&self, pane_id: &str, keys: &[String]) -> Result<()> {
        self.call("pane.send_keys", json!({ "pane_id": pane_id, "keys": keys })).map(drop)
    }

    /// Texto literal, sin pulsar Enter.
    pub fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.call("pane.send_text", json!({ "pane_id": pane_id, "text": text })).map(drop)
    }

    /// Escribe un comando en la shell del panel y pulsa Enter.
    pub fn run(&self, pane_id: &str, command: &str) -> Result<()> {
        self.call("pane.send_input", json!({ "pane_id": pane_id, "text": command, "keys": ["enter"] })).map(drop)
    }

    /// Envía un mensaje al agente del panel (lo escribe y lo manda).
    pub fn prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        self.call("agent.prompt", json!({ "target": pane_id, "text": text })).map(drop)
    }

    /// Lleva el foco de herdr al panel (cambia de espacio y pestaña si hace falta).
    pub fn focus(&self, pane_id: &str) -> Result<()> {
        self.call("pane.focus", json!({ "pane_id": pane_id })).map(drop)
    }

    pub fn close(&self, pane_id: &str) -> Result<()> {
        self.call("pane.close", json!({ "pane_id": pane_id })).map(drop)
    }

    /// `true` si en el panel no corre nada más que la shell (se puede lanzar un comando).
    pub fn at_shell(&self, pane_id: &str) -> Result<bool> {
        let r = self.call("pane.process_info", json!({ "pane_id": pane_id }))?;
        let p = &r["process_info"];
        Ok(p["shell_pid"].as_u64().is_some() && p["shell_pid"] == p["foreground_process_group_id"])
    }

    /// Divide un panel y devuelve el id del nuevo.
    pub fn split(&self, pane_id: &str, down: bool, cwd: Option<&str>) -> Result<String> {
        let r = self.call(
            "pane.split",
            json!({ "target_pane_id": pane_id, "direction": if down { "down" } else { "right" }, "cwd": cwd, "focus": false }),
        )?;
        r["pane"]["pane_id"].as_str().map(String::from).ok_or_else(|| anyhow!("herdr no ha devuelto el panel nuevo"))
    }

    /// Arranca un agente que herdr conoce en un panel que está en la shell.
    pub fn agent_start(&self, name: &str, kind: &str, pane_id: &str, args: &[String]) -> Result<()> {
        self.call("agent.start", json!({ "name": name, "kind": kind, "pane_id": pane_id, "args": args })).map(drop)
    }


}

fn parse_snapshot(s: &Value) -> Snapshot {
    let arr = |k: &str| s[k].as_array().cloned().unwrap_or_default();
    let ws: HashMap<String, (String, u32)> = arr("workspaces")
        .iter()
        .filter_map(|w| {
            Some((
                w["workspace_id"].as_str()?.to_string(),
                (w["label"].as_str().unwrap_or_default().to_string(), w["number"].as_u64().unwrap_or(0) as u32),
            ))
        })
        .collect();
    let tabs: HashMap<String, String> = arr("tabs")
        .iter()
        .filter_map(|t| Some((t["tab_id"].as_str()?.to_string(), t["label"].as_str().unwrap_or_default().to_string())))
        .collect();
    // El nombre del agente (`agent rename`) solo viene en la lista de agentes.
    let names: HashMap<String, String> = arr("agents")
        .iter()
        .filter_map(|a| Some((a["pane_id"].as_str()?.to_string(), a["name"].as_str()?.to_string())))
        .collect();
    let str_of = |v: &Value| v.as_str().filter(|x| !x.is_empty()).map(String::from);
    let panes = arr("panes")
        .iter()
        .filter_map(|p| {
            let pane_id = p["pane_id"].as_str()?.to_string();
            let workspace_id = p["workspace_id"].as_str().unwrap_or_default().to_string();
            let tab_id = p["tab_id"].as_str().unwrap_or_default().to_string();
            let (workspace_label, workspace_number) = ws.get(&workspace_id).cloned().unwrap_or_default();
            let agent_raw = str_of(&p["agent"]);
            let session = p["agent_session"]["value"].as_str().filter(|v| !v.is_empty()).map(|v| {
                if p["agent_session"]["kind"] == "path" { SessionRef::Path(PathBuf::from(v)) } else { SessionRef::Id(v.to_string()) }
            });
            Some(Pane {
                label: str_of(&p["label"]).or_else(|| names.get(&pane_id).cloned()),
                kind: Kind::from_herdr(agent_raw.as_deref()),
                agent_raw,
                status: Status::from_herdr(p["agent_status"].as_str().unwrap_or("unknown")),
                title: str_of(&p["terminal_title_stripped"]).or_else(|| str_of(&p["title"])).unwrap_or_default(),
                cwd: PathBuf::from(p["foreground_cwd"].as_str().or_else(|| p["cwd"].as_str()).unwrap_or_default()),
                session,
                session_agent: str_of(&p["agent_session"]["agent"]),
                focused: p["focused"].as_bool().unwrap_or(false),
                revision: p["revision"].as_u64().unwrap_or(0),
                tab_label: tabs.get(&tab_id).cloned().unwrap_or_default(),
                workspace_label,
                workspace_number,
                pane_id,
                tab_id,
                workspace_id,
            })
        })
        .collect();
    Snapshot {
        panes,
        focused_pane_id: str_of(&s["focused_pane_id"]),
        focused_workspace_id: str_of(&s["focused_workspace_id"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_minimo() {
        let v = json!({
            "focused_pane_id": "w1:p1", "focused_workspace_id": "w1",
            "workspaces": [{ "workspace_id": "w1", "label": "api", "number": 3 }],
            "tabs": [{ "tab_id": "w1:t1", "label": "1" }],
            "agents": [{ "pane_id": "w1:p2", "name": "back" }],
            "panes": [
                { "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "agent": "claude", "label": "Lider",
                  "agent_status": "working", "terminal_title_stripped": "Plan", "cwd": "/a", "foreground_cwd": "/a/b",
                  "agent_session": { "kind": "id", "value": "abc" }, "focused": true, "revision": 7 },
                { "pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t1", "agent": "omp", "agent_status": "idle",
                  "agent_session": { "kind": "path", "value": "/s/x.jsonl" }, "cwd": "/a" },
                { "pane_id": "w1:p3", "workspace_id": "w1", "tab_id": "w1:t1", "agent_status": "unknown", "cwd": "/a" }
            ]
        });
        let s = parse_snapshot(&v);
        assert_eq!(s.panes.len(), 3);
        let p = &s.panes[0];
        assert_eq!((p.kind, p.status, p.label.as_deref(), p.workspace_label.as_str(), p.workspace_number), (Kind::Claude, Status::Working, Some("Lider"), "api", 3));
        assert_eq!((p.cwd.to_str(), p.session.clone(), p.revision), (Some("/a/b"), Some(SessionRef::Id("abc".into())), 7));
        assert_eq!((s.panes[1].kind, s.panes[1].label.as_deref()), (Kind::Omp, Some("back")));
        assert_eq!(s.panes[1].session, Some(SessionRef::Path("/s/x.jsonl".into())));
        assert_eq!((s.panes[2].kind, s.panes[2].status), (Kind::Shell, Status::Unknown));
        assert_eq!(s.focused_pane_id.as_deref(), Some("w1:p1"));
    }
    #[test]
    fn rpc_accepts_null_error_and_rejects_broken_snapshots() {
        use std::os::unix::net::UnixListener;
        for (n,response,success) in [(0,serde_json::json!({"error":null,"result":{"snapshot":{"panes":[]}}}),true),(1,serde_json::json!({"result":{}}),false),(2,serde_json::json!({"error":{"message":"denied"}}),false)] {
            let path=std::env::temp_dir().join(format!("agentview-rpc-{}-{n}.sock",std::process::id()));
            let listener=UnixListener::bind(&path).unwrap();
            let worker=std::thread::spawn(move||{ let (mut stream,_)=listener.accept().unwrap();let mut request=String::new();BufReader::new(&stream).read_line(&mut request).unwrap();assert!(request.contains("session.snapshot"));writeln!(stream,"{response}").unwrap(); });
            let client=Herdr{sock:path.clone()};assert_eq!(client.snapshot().is_ok(),success);
            worker.join().unwrap();std::fs::remove_file(path).unwrap();
        }
    }

}
