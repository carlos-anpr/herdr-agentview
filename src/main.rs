mod ai;
mod ansi;
mod app;
mod config;
mod demo;
mod herdr;
mod keys;
mod model;
mod sources;
mod theme;
mod ui;
mod util;

use app::App;
use ratatui::crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture};
use ratatui::crossterm::execute;
use std::time::Duration;

const USAGE: &str = "herdr-agentview [OPCIONES]

Centro de control de agentes para herdr: qué hace cada agente, en vivo, y cómo
hablarle, pararlo o reanudarlo.

  --all             empezar viendo todos los espacios de trabajo
  --demo            datos de ejemplo, sin conectar con herdr
  --dump [PANE_ID]  imprimir los paneles que ve herdr (y la sesión de PANE_ID) y salir
  --render AxB      pintar un fotograma de A columnas por B filas como HTML y salir
  -V, --version     versión";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = config::load();
    let (mut demo, mut render, mut keys) = (false, None, String::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dump" => return dump(it.next().map(String::as_str)),
            "--all" => cfg.scope = "all".into(),
            "--demo" => demo = true,
            "--render" => render = it.next().and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))),
            "--keys" => keys = it.next().cloned().unwrap_or_default(),
            "-V" | "--version" => {
                println!("herdr-agentview {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {
                println!("{USAGE}");
                return Ok(());
            }
        }
    }
    // La demo no depende de la configuración de quien la ejecuta.
    let mut app = if demo { demo::app(config::Config::default()) } else { App::new(cfg) };
    if let Some((w, h)) = render {
        return render_html(&mut app, w, h, &keys, demo);
    }

    let mut term = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let res = run(&mut term, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    res
}

fn run(term: &mut ratatui::DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    while !app.quit {
        term.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(60))? {
            app.on_event(event::read()?);
            // Vaciar ráfagas (rueda del ratón, tecla mantenida, pegado) antes de repintar.
            while event::poll(Duration::ZERO)? {
                app.on_event(event::read()?);
                if app.quit {
                    break;
                }
            }
        }
        while let Ok(m) = app.rx.try_recv() {
            app.on_msg(m);
        }
        app.tick();
    }
    Ok(())
}

/// Pinta un fotograma fuera de la terminal y lo imprime como HTML (para revisar el diseño y para
/// las capturas). `keys` son teclas que se pulsan antes de pintar: letras tal cual y, entre llaves,
/// `{enter}` `{esc}` `{tab}` `{up}` `{down}` `{left}` `{right}`.
fn render_html(app: &mut App, w: u16, h: u16, keys: &str, demo: bool) -> anyhow::Result<()> {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let pump = |app: &mut App, ms: u64| {
        let end = std::time::Instant::now() + Duration::from_millis(ms);
        loop {
            while let Ok(m) = app.rx.try_recv() {
                app.on_msg(m);
            }
            if std::time::Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    pump(app, if demo { 0 } else { 2500 });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h))?;
    term.draw(|f| ui::draw(f, app))?; // fija la geometría que usan algunas teclas
    let mut chars = keys.chars();
    while let Some(c) = chars.next() {
        let code = if c == '{' {
            let name: String = chars.by_ref().take_while(|c| *c != '}').collect();
            match name.as_str() {
                "enter" => KeyCode::Enter,
                "esc" => KeyCode::Esc,
                "tab" => KeyCode::Tab,
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                "left" => KeyCode::Left,
                "right" => KeyCode::Right,
                _ => continue,
            }
        } else {
            KeyCode::Char(c)
        };
        app.on_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        pump(app, if demo { 0 } else { 900 });
        term.draw(|f| ui::draw(f, app))?;
    }
    app.tick = 4;
    term.draw(|f| ui::draw(f, app))?;
    print!("{}", theme::buffer_html(term.backend().buffer()));
    Ok(())
}

fn dump(pane: Option<&str>) -> anyhow::Result<()> {
    let snap = herdr::Herdr::from_env().snapshot()?;
    for p in &snap.panes {
        if pane.is_some_and(|id| id != p.pane_id) {
            continue;
        }
        println!("{:8} {:9?} {:8?} {:16} {:?}", p.pane_id, p.kind, p.status, p.label.as_deref().unwrap_or("-"), p.session);
        if pane.is_none() {
            continue;
        }
        let Some(sref) = &p.session else { continue };
        match sources::open(p.kind, sref, &p.cwd) {
            Some(mut src) => {
                let t = std::time::Instant::now();
                src.refresh()?;
                let first = t.elapsed();
                let t = std::time::Instant::now();
                src.refresh()?;
                let refresh = t.elapsed();
                println!("{:#?}", src.info());
                println!("carga inicial: {first:?} · refresco sin cambios: {refresh:?}");
            }
            None => println!("(sin fuente de sesión para este panel)"),
        }
    }
    Ok(())
}
