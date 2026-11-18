//! Render con ratatui. Cada frame reconstruye `app.hits` para el ratón.

use crate::ansi;
use crate::app::*;
use crate::model::*;
use crate::theme::*;
use crate::util::now;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

fn spin(app: &App) -> &'static str {
    SPIN[(app.tick as usize / 2) % SPIN.len()]
}

fn panel(title: Vec<Span<'static>>, color: Color) -> Block<'static> {
    Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::new().fg(color)).title(Line::from(title))
}

/// Recorta una lista de spans a `w` columnas, con `…` si corta.
fn clip_spans(spans: Vec<Span<'static>>, w: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= w {
        return spans;
    }
    let mut out = Vec::new();
    let mut acc = 0;
    for sp in spans {
        let sw = width(&sp.content);
        if acc + sw < w {
            acc += sw;
            out.push(sp);
        } else {
            let room = w - acc;
            if room > 0 {
                out.push(Span::styled(trunc(&sp.content, room), sp.style));
            }
            break;
        }
    }
    out
}

/// Línea de ancho exacto `w`: `left` a la izquierda (recortado si no cabe) y `right` pegado al borde.
fn fit(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize) -> Vec<Span<'static>> {
    let rw = spans_width(&right).min(w);
    let gap = if rw > 0 { 1 } else { 0 };
    let mut out = clip_spans(left, w.saturating_sub(rw + gap));
    let used = spans_width(&out);
    out.push(Span::raw(" ".repeat(w.saturating_sub(used + rw))));
    out.extend(clip_spans(right, rw));
    out
}

fn with_bg(spans: Vec<Span<'static>>, bg: Option<Color>) -> Line<'static> {
    let l = Line::from(spans);
    match bg {
        Some(c) => l.style(Style::new().bg(c)),
        None => l,
    }
}

/// Glifo de estado de un panel, con el spinner si trabaja.
fn status_span(app: &App, p: &Pane) -> Span<'static> {
    if app.stalled(p).is_some() {
        return fg("◌", WARN);
    }
    match p.status {
        Status::Working => fg(spin(app), ACCENT),
        s => fg(status_glyph(s), status_color(s)),
    }
}

/// Qué está haciendo un agente ahora mismo, en una línea, y el tiempo que acompaña.
fn now_line(app: &App, p: &Pane) -> (Vec<Span<'static>>, String) {
    let info = app.info(&p.pane_id);
    let t = now();
    let since = |ts: Option<i64>| ts.map(|x| fmt_short(t.saturating_sub(x))).unwrap_or_default();
    if p.kind == Kind::Shell {
        let txt = if p.session.is_some() { "apagado · r lo reanuda" } else { "sin agente · r lanza uno" };
        return (vec![faint(txt)], String::new());
    }
    if let Some(q) = app.stalled(p) {
        return (vec![fg(format!("sin novedades desde hace {}", fmt_short(q)), WARN)], since(info.and_then(|i| i.turn_started_at)));
    }
    match p.status {
        Status::Blocked => (vec![fg("espera tu respuesta", WARN)], since(info.and_then(|i| i.updated_at))),
        Status::Working => {
            let spans = match info.and_then(|i| i.events.last()) {
                Some(e) => event_brief(e),
                None => vec![dim(clean_title(&p.title, p.kind))],
            };
            (spans, since(info.and_then(|i| i.turn_started_at)))
        }
        Status::Done => {
            let txt = info.and_then(|i| i.last_reply.as_deref()).map(|r| plain(&crate::util::first_line(r, 160))).unwrap_or_else(|| "ha terminado".into());
            (vec![fg(txt, OK)], since(info.and_then(|i| i.updated_at)))
        }
        _ => {
            // En reposo: lo último que dijo; si aún no ha dicho nada, lo que se le pidió.
            let txt = info
                .and_then(|i| i.last_reply.as_deref().map(|r| plain(&crate::util::first_line(r, 160))).or_else(|| i.last_prompt.clone()).or_else(|| i.title.clone()))
                .unwrap_or_else(|| clean_title(&p.title, p.kind));
            (vec![dim(txt)], since(info.and_then(|i| i.updated_at)))
        }
    }
}

/// Un evento resumido para una línea estrecha: verbo y objeto.
fn event_brief(e: &Event) -> Vec<Span<'static>> {
    let (verb, color) = event_verb(e.kind);
    match e.kind {
        EventKind::Prompt => vec![fg("❯ ", ACCENT), dim(e.title.clone())],
        EventKind::Reply => vec![dim(e.title.clone())],
        EventKind::Read | EventKind::Edit => vec![fg(format!("{verb} "), color), dim(short_path(&e.title, 28))],
        _ => vec![fg(format!("{verb} "), color), dim(e.title.clone())],
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.hits.clear();
    let area = f.area();
    if area.width < 40 || area.height < 10 {
        f.render_widget(Paragraph::new("Ventana demasiado pequeña"), area);
        return;
    }
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(area);
    draw_header(f, app, head);
    if !app.loaded {
        let msg = match &app.error {
            Some(e) => vec![fg("No se puede hablar con herdr: ", ERR), raw(e.clone())],
            None => vec![fg(format!("{} ", spin(app)), ACCENT), dim("Conectando con herdr…")],
        };
        f.render_widget(Paragraph::new(Line::from(msg)).centered(), Rect::new(body.x, body.y + body.height / 2, body.width, 1));
    } else {
        match app.view {
            View::Detail => {
                let rail_w = ((u32::from(body.width) * 30 / 100) as u16).clamp(30, 46).min(body.width.saturating_sub(30));
                let [rail, stage] = Layout::horizontal([Constraint::Length(rail_w), Constraint::Min(20)]).areas(body);
                app.rail_area = rail;
                app.stage_area = stage;
                draw_rail(f, app, rail);
                draw_stage(f, app, stage);
            }
            View::Wall => {
                app.rail_area = Rect::default();
                app.stage_area = body;
                draw_wall(f, app, body);
            }
        }
    }
    draw_footer(f, app, foot);
    if let Mode::Explain(ex) = &mut app.mode { draw_explanation(f, ex, area); }
    match &app.mode {
        Mode::Help => draw_help(f, area),
        Mode::Confirm(c) => draw_confirm(f, c, area),
        Mode::New(n) => draw_new(f, app, n, area),
        _ => {}
    }
}

// ─────────────────────────────── cabecera ───────────────────────────────

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let narrow = area.width < 110;
    let mut counts = [0usize; 4];
    for p in app.scoped_agents() {
        match p.status {
            Status::Working => counts[0] += 1,
            Status::Blocked => counts[1] += 1,
            Status::Done => counts[2] += 1,
            _ => counts[3] += 1,
        }
    }
    let pills = [
        (Status::Working, counts[0], "trabajando"),
        (Status::Blocked, counts[1], if counts[1] == 1 { "espera respuesta" } else { "esperan respuesta" }),
        (Status::Done, counts[2], if counts[2] == 1 { "ha terminado" } else { "han terminado" }),
        (Status::Idle, counts[3], "en reposo"),
    ];
    let wall = app.view == View::Wall;
    let all = app.scope == Scope::All;
    let btns = [
        (Btn::Wall, match (wall, narrow) {
            (true, true) => " ☰ ",
            (true, false) => " ☰ lista ",
            (false, true) => " ▦ ",
            (false, false) => " ▦ mosaico ",
        }),
        (Btn::Scope, match (all, narrow) {
            (_, true) => " ◎ ",
            (true, false) => " ◎ solo este espacio ",
            (false, false) => " ◎ todos los espacios ",
        }),
        (Btn::Help, " ? "),
        (Btn::Quit, " ✕ "),
    ];
    let right_w: usize = btns.iter().map(|b| width(b.1)).sum();
    let avail = (area.width as usize).saturating_sub(right_w);

    let mut spans = vec![
        Span::styled(" ◆ agentview ", Style::new().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        bold(app.scope_label()),
        Span::raw("   "),
    ];
    // Contadores con palabra si caben; si no, solo el número.
    let label = |app: &App, st: Status, n: usize, word: &str, short: bool| {
        let glyph = if st == Status::Working { spin(app) } else { status_glyph(st) };
        if short { format!("{glyph} {n}") } else { format!("{glyph} {n} {word}") }
    };
    let long_w: usize = pills.iter().filter(|p| p.1 > 0).map(|(st, n, word)| width(&label(app, *st, *n, word, false)) + 3).sum();
    let short = spans_width(&spans) + long_w > avail;
    let mut x = area.x.saturating_add(spans_width(&spans).min(area.width as usize) as u16);
    for (st, n, word) in pills {
        if n == 0 {
            continue;
        }
        let text = label(app, st, n, word, short);
        let w = width(&text).min(area.width as usize) as u16;
        app.hits.push((Rect::new(x, area.y, w, 1), Hit::Pill(st)));
        spans.push(fg(text, status_color(st)));
        spans.push(Span::raw("   "));
        x = x.saturating_add(w).saturating_add(3);
    }
    if app.error.is_some() {
        spans.push(fg("sin conexión con herdr", ERR));
    }
    let mut line = clip_spans(spans, avail);
    let used = spans_width(&line);
    line.push(Span::raw(" ".repeat(avail.saturating_sub(used))));
    let mut bx = area.x + avail as u16;
    for (b, label) in btns {
        let w = width(label) as u16;
        app.hits.push((Rect::new(bx, area.y, w, 1), Hit::Btn(b)));
        line.push(fg(label, if b == Btn::Quit { ERR } else { DIM }));
        bx += w;
    }
    f.render_widget(Paragraph::new(Line::from(line)).style(Style::new().bg(BG_BAR)), area);
}

// ─────────────────────────────── pie ───────────────────────────────

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let p = app.sel_pane();
    let marked = app.marks.len();
    let msg = format!("mensaje a {marked} marcados");
    let hints: Vec<(&str, &str)> = match &app.mode {
        Mode::Direct => vec![("ctrl+q", "salir del control directo")],
        Mode::Input(_) => vec![("⏎", "enviar"), ("alt+⏎", "salto de línea"), ("esc", "cancelar")],
        Mode::Search => vec![("⏎", "aceptar"), ("esc", "quitar filtro")],
        Mode::Confirm(_) => vec![("⏎", "sí"), ("esc", "no")],
        Mode::New(_) => vec![("↑↓", "elegir"), ("⏎", "lanzar"), ("esc", "cancelar")],
        Mode::Help => vec![("cualquier tecla", "cerrar")],
        Mode::Explain(_) => vec![("↑↓", "desplazar"),("e","actualizar"),("esc","cerrar")],
        Mode::Normal if marked > 0 => vec![("m", &msg), ("espacio", "marcar"), ("esc", "quitar marcas")],
        Mode::Normal => {
            let mut h: Vec<(&str, &str)> = vec![("⏎", "ir al panel")];
            match p.map(|p| (p.kind, p.status)) {
                Some((Kind::Shell, _)) => h.extend([("r", "lanzar agente"), ("t", "teclear"), ("x", "cerrar")]),
                Some((_, Status::Blocked)) => h.extend([("t", "responder"), ("x", "cerrar")]),
                Some((_, Status::Working)) => h.extend([("m", "mensaje"), ("i", "interrumpir"), ("t", "control directo")]),
                Some(_) => h.extend([("m", "mensaje"), ("c", "continuar"), ("t", "control directo")]),
                None => {}
            }
            h.push(("e","explicar con IA"));
            h.extend([("v", if app.view == View::Wall { "lista" } else { "mosaico" }), ("s", "ámbito"), ("?", "ayuda")]);
            h
        }
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in hints {
        spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
        spans.push(dim(format!(" {d}   ")));
    }
    let toast = app.toast.as_ref().map(|(t, k, _)| {
        let bg = match k {
            ToastKind::Ok => OK,
            ToastKind::Warn => WARN,
            ToastKind::Err => ERR,
        };
        vec![Span::styled(format!(" {t} "), Style::new().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD))]
    });
    let w = area.width as usize;
    let line = match toast {
        // El aviso manda: si no cabe todo, se recortan los atajos.
        Some(t) => {
            let t = clip_spans(t, w.saturating_sub(2));
            fit(spans, t, w)
        }
        None => fit(spans, vec![], w),
    };
    f.render_widget(Paragraph::new(Line::from(line)).style(Style::new().bg(BG_BAR)), area);
}

// ─────────────────────────────── lista de agentes ───────────────────────────────

fn draw_rail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Rail;
    let agents = app.items.iter().filter(|i| matches!(i, Item::Agent(_))).count();
    let mut title = vec![bold(" Agentes "), dim(format!("{agents} "))];
    if matches!(app.mode, Mode::Search) || !app.filter.is_empty() {
        title.push(fg(format!("/ {}", app.filter), ACCENT));
        title.push(if matches!(app.mode, Mode::Search) { fg("▏", ACCENT) } else { Span::raw(" ") });
    }
    let block = panel(title, if focused { ACCENT } else { BORDER });
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    if app.items.is_empty() {
        let msg = if app.filter.is_empty() { "No hay agentes en este espacio" } else { "Ningún agente coincide" };
        let hint = if app.filter.is_empty() { "s  ver todos los espacios   ·   n  lanzar uno" } else { "esc  quitar el filtro" };
        f.render_widget(Paragraph::new(vec![Line::from(dim(msg)), Line::default(), Line::from(faint(hint))]).centered(), Rect::new(inner.x, inner.y + inner.height / 3, inner.width, 3));
        return;
    }

    // Altura natural con aire entre agentes; si no cabe, modo compacto.
    let natural: usize = app
        .items
        .iter()
        .map(|it| match it {
            Item::Group { .. } => 2,
            Item::Agent(_) => 3,
            Item::Child { .. } => 1,
        })
        .sum();
    let airy = natural <= inner.height as usize + 1;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans_of: Vec<(usize, usize)> = Vec::with_capacity(app.items.len()); // (primera línea, nº de líneas)
    for (n, it) in app.items.iter().enumerate() {
        let sel = n == app.sel;
        let bg = sel.then_some(if focused { SEL_BG } else { SEL_BG_DIM });
        let start = lines.len();
        match it {
            Item::Group { label, count } => {
                if airy && n > 0 && !lines.last().is_some_and(|l: &Line| l.spans.is_empty()) {
                    lines.push(Line::default());
                }
                let head = format!(" {} ", label.to_uppercase());
                let tail = format!(" {count} ");
                let rule = w.saturating_sub(width(&head) + width(&tail) + 1);
                lines.push(Line::from(vec![faint("─"), fg(head, DIM), faint("─".repeat(rule)), faint(tail)]));
                spans_of.push((start, lines.len() - start));
                continue;
            }
            Item::Agent(i) => {
                let p = &app.panes[*i];
                let shell = p.kind == Kind::Shell;
                let mark = if app.marks.contains(&p.pane_id) { fg("◆", VIOLET) } else if sel { fg("▌", ACCENT) } else { Span::raw(" ") };
                let mut left = vec![mark, status_span(app, p), Span::raw(" ")];
                let name = App::name(p);
                left.push(if shell { dim(name) } else { bold(name) });
                if app.fresh.contains(&p.pane_id) {
                    left.push(fg(" •", ACCENT));
                }
                let kind = fg(kind_name(p.kind, p.agent_raw.as_deref()), if shell { FAINT } else { kind_color(p.kind) });
                lines.push(with_bg(fit(left, vec![kind, Span::raw(" ")], w), bg));
                let (what, when) = now_line(app, p);
                let mut l2 = vec![Span::raw("   ")];
                l2.extend(what);
                lines.push(with_bg(fit(l2, vec![faint(when), Span::raw(" ")], w), bg));
                // El hueco va tras el último subagente, o tras el agente si no tiene ninguno.
                let has_kids = matches!(app.items.get(n + 1), Some(Item::Child { .. }));
                spans_of.push((start, 2));
                if airy && !has_kids {
                    lines.push(Line::default());
                }
            }
            Item::Child { pane, child, last } => {
                let Some(c) = app.info(&app.panes[*pane].pane_id).and_then(|i| i.children.get(*child)) else {
                    spans_of.push((start, 0));
                    continue;
                };
                let glyph = if c.running { fg(spin(app), ACCENT) } else { fg("✓", FAINT) };
                let left = vec![
                    if sel { fg("▌", ACCENT) } else { Span::raw(" ") },
                    faint(if *last { "  └ " } else { "  ├ " }),
                    glyph,
                    Span::raw(" "),
                    if c.running { raw(c.name.clone()) } else { dim(c.name.clone()) },
                ];
                let right = vec![faint(c.agent_type.clone().unwrap_or_default()), Span::raw(" ")];
                lines.push(with_bg(fit(left, right, w), bg));
                spans_of.push((start, 1));
                if airy && *last {
                    lines.push(Line::default());
                }
            }
        }
    }

    // Mantener la selección a la vista.
    let h = inner.height as usize;
    if let Some(&(start, len)) = spans_of.get(app.sel) {
        if start < app.rail_top {
            // Si justo encima hay una cabecera de grupo, enseñarla también.
            app.rail_top = start.saturating_sub(if start <= 2 { start } else { 0 });
        } else if start + len > app.rail_top + h {
            app.rail_top = start + len - h;
        }
    }
    app.rail_top = app.rail_top.min(lines.len().saturating_sub(h));
    let top = app.rail_top;
    for (n, &(start, len)) in spans_of.iter().enumerate() {
        if len == 0 || matches!(app.items[n], Item::Group { .. }) || start + len <= top || start >= top + h {
            continue;
        }
        let y0 = start.max(top) - top;
        let y1 = (start + len).min(top + h) - top;
        app.hits.push((Rect::new(inner.x, inner.y + y0 as u16, inner.width, (y1 - y0) as u16), Hit::Row(n)));
    }
    let more_up = top > 0;
    let more_down = top + h < lines.len();
    let view: Vec<Line> = lines.into_iter().skip(top).take(h).collect();
    f.render_widget(Paragraph::new(view), inner);
    if more_up {
        f.render_widget(Paragraph::new(faint("▲")), Rect::new(area.x + area.width - 2, area.y, 1, 1));
    }
    if more_down {
        f.render_widget(Paragraph::new(faint("▼")), Rect::new(area.x + area.width - 2, area.y + area.height - 1, 1, 1));
    }
}

// ─────────────────────────────── escenario ───────────────────────────────

fn draw_stage(f: &mut Frame, app: &mut App, area: Rect) {
    let direct = matches!(app.mode, Mode::Direct);
    let focused = app.focus == Focus::Stage;
    let Some(pi) = app.sel_pane_idx() else {
        f.render_widget(panel(vec![], BORDER), area);
        return;
    };
    let p = app.panes[pi].clone();
    let child = app.sel_child().cloned();
    let info = app.sessions.get(&p.pane_id).cloned();
    let info = info.as_deref();

    let name = App::name(&p);
    let mut title = vec![Span::raw(" "), status_span(app, &p), Span::raw(" "), bold(name.clone())];
    if let Some(c) = &child {
        title.push(faint("  ›  "));
        title.push(if c.running { fg(spin(app), ACCENT) } else { fg("✓", OK) });
        title.push(bold(format!(" {}", trunc(&c.name, 50))));
    }
    title.push(Span::raw(" "));
    let border = if direct { WARN } else if focused { ACCENT } else { BORDER };
    let where_ = format!(" {} · {} ", p.workspace_label, p.pane_id);
    let block = panel(title, border).title(Line::from(faint(where_)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 6 || inner.width < 20 {
        return;
    }
    let inner = Rect::new(inner.x + 1, inner.y, inner.width - 2, inner.height);

    let input_h = match &app.mode {
        Mode::Input(_) => 6.min(inner.height.saturating_sub(6)),
        _ => 0,
    };
    let [head, tabs, body, input] =
        Layout::vertical([Constraint::Length(4), Constraint::Length(2), Constraint::Min(1), Constraint::Length(input_h)]).areas(inner);

    draw_stage_head(f, app, &p, info, child.as_ref(), head);
    let tab = app.tab();
    draw_tabs(f, app, &p, info, child.is_some(), tab, tabs);
    match tab {
        StageTab::Activity => draw_activity(f, app, &p, info, child.as_ref(), body),
        StageTab::Screen => draw_screen(f, app, &p, body),
        StageTab::Files => draw_files(f, app, info, body),
    }
    if let Mode::Input(inp) = &app.mode {
        draw_input(f, inp, input);
    }
}

fn draw_stage_head(f: &mut Frame, app: &App, p: &Pane, info: Option<&SessionInfo>, child: Option<&Child>, area: Rect) {
    let w = area.width as usize;
    let t = now();
    // 1) Qué se le ha pedido.
    let task = match child {
        // Del subagente, el encargo literal: su nombre ya está en el título del panel.
        Some(c) => c.events.iter().find(|e| e.kind == EventKind::Prompt).map(|e| e.title.clone()).unwrap_or_else(|| c.name.clone()),
        None => info
            .and_then(|i| i.last_prompt.clone().or_else(|| i.title.clone()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| match clean_title(&p.title, p.kind) {
                _ if p.kind == Kind::Shell => "Panel sin agente".into(),
                t if t.is_empty() => "Sin tarea".into(),
                t => t,
            }),
    };
    let l1 = Line::from(vec![Span::styled(trunc(&task, w), Style::new().fg(Color::White).add_modifier(Modifier::BOLD))]);

    // 2) Estado, modelo y dónde trabaja.
    let sep = || faint("  ·  ");
    let mut l2: Vec<Span<'static>> = Vec::new();
    match child {
        Some(c) => {
            let (word, color) = if c.running { ("Trabajando", ACCENT) } else { ("Ha terminado", OK) };
            l2.push(fg(word, color));
            let span = match (c.started_at, c.updated_at) {
                (Some(a), _) if c.running => Some(t.saturating_sub(a)),
                (Some(a), Some(b)) => Some(b.saturating_sub(a)),
                _ => None,
            };
            if let Some(s) = span {
                l2.push(dim(format!(" {}", fmt_dur(s))));
            }
            if let Some(ty) = &c.agent_type {
                l2.push(sep());
                l2.push(fg(ty.clone(), VIOLET));
            }
            if let Some(m) = &c.model {
                l2.push(sep());
                l2.push(raw(pretty_model(m)));
            }
            l2.push(sep());
            l2.push(dim(format!("subagente de {}", App::name(p))));
        }
        None => {
            let stalled = app.stalled(p);
            let color = if stalled.is_some() { WARN } else { status_color(p.status) };
            l2.push(fg(status_word(p.status), color));
            let dur = match p.status {
                Status::Working => info.and_then(|i| i.turn_started_at).map(|s| fmt_dur(t.saturating_sub(s))),
                Status::Unknown => None,
                _ => info.and_then(|i| i.updated_at).map(|s| format!("hace {}", fmt_dur(t.saturating_sub(s)))),
            };
            if let Some(d) = dur {
                l2.push(dim(format!(" {d}")));
            }
            if let Some(q) = stalled {
                l2.push(fg(format!("  sin novedades desde hace {}", fmt_dur(q)), WARN));
            }
            l2.push(sep());
            l2.push(fg(kind_name(p.kind, p.agent_raw.as_deref()), kind_color(p.kind)));
            if let Some(m) = info.and_then(|i| i.model.as_deref()) {
                l2.push(raw(format!(" {}", pretty_model(m))));
                if let Some(e) = info.and_then(|i| i.effort.as_deref()) {
                    l2.push(dim(format!(" {e}")));
                }
            }
            l2.push(sep());
            l2.push(dim(home_path(&p.cwd)));
        }
    }

    // 3) Consumo: contexto, cuotas, tokens, coste y avance del plan.
    let mut l3: Vec<Span<'static>> = Vec::new();
    let gap = || Span::raw("    ");
    match (child, info) {
        (Some(c), _) => {
            if c.tokens_in > 0 || c.tokens_out > 0 {
                l3.extend([dim("tokens "), raw(format!("{} ↓ {} ↑", fmt_count(c.tokens_in), fmt_count(c.tokens_out)))]);
            }
        }
        (None, Some(i)) => {
            if let Some(used) = i.context_used {
                l3.push(dim("contexto "));
                match i.context_window.filter(|w| *w > 0) {
                    Some(win) => {
                        let pct = used as f32 * 100.0 / win as f32;
                        l3.extend(gauge(pct, 10, load_color(pct)));
                        l3.push(raw(format!(" {:.0}%", pct)));
                    }
                    None => l3.push(raw(fmt_count(used))),
                }
                l3.push(gap());
            }
            for lim in &i.limits {
                l3.push(dim(format!("{} ", lim.name)));
                l3.extend(gauge(lim.used_pct, 6, load_color(lim.used_pct)));
                l3.push(raw(format!(" {:.0}%", lim.used_pct)));
                l3.push(gap());
            }
            if i.tokens_in > 0 || i.tokens_out > 0 {
                l3.extend([dim("tokens "), raw(format!("{} ↓ {} ↑", fmt_count(i.tokens_in), fmt_count(i.tokens_out))), gap()]);
            }
            if let Some(c) = i.cost_usd.filter(|c| *c > 0.0) {
                l3.extend([dim("coste "), raw(format!("${c:.2}")), gap()]);
            }
            if !i.todos.is_empty() {
                let done = i.todos.iter().filter(|t| t.state == TodoState::Done).count();
                let pct = done as f32 * 100.0 / i.todos.len() as f32;
                l3.push(dim("plan "));
                l3.extend(gauge(pct, 8, OK));
                l3.push(raw(format!(" {done}/{}", i.todos.len())));
            }
        }
        (None, None) => {}
    }
    let lines = vec![l1, Line::from(clip_spans(l2, w)), Line::from(clip_spans(l3, w))];
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_tabs(f: &mut Frame, app: &mut App, p: &Pane, info: Option<&SessionInfo>, is_child: bool, tab: StageTab, area: Rect) {
    let files = info.map(|i| i.files.len()).unwrap_or(0);
    let has_data = App::has_source(p.kind) && info.is_some();
    let mut tabs: Vec<(StageTab, String)> = Vec::new();
    if has_data || is_child {
        tabs.push((StageTab::Activity, " 1 Actividad ".into()));
    }
    if !is_child {
        tabs.push((StageTab::Screen, if has_data { " 2 Pantalla ".into() } else { " Pantalla en vivo ".into() }));
        if files > 0 {
            tabs.push((StageTab::Files, format!(" 3 Ficheros {files} ")));
        }
    }
    let mut spans = Vec::new();
    let mut x = area.x;
    for (t, label) in tabs {
        let w = width(&label) as u16;
        let st = if t == tab { Style::new().bg(SEL_BG).fg(Color::White).add_modifier(Modifier::BOLD) } else { Style::new().fg(DIM) };
        app.hits.push((Rect::new(x, area.y, w, 1), Hit::Tab(t)));
        spans.push(Span::styled(label, st));
        spans.push(Span::raw(" "));
        x += w + 1;
    }
    let rule = Line::from(faint("─".repeat(area.width as usize)));
    f.render_widget(Paragraph::new(vec![Line::from(spans), rule]), area);
}

/// Texto que acompaña a la línea de tiempo: el detalle del evento elegido o la última respuesta.
fn detail_text(app: &App, info: Option<&SessionInfo>, child: Option<&Child>) -> Option<(&'static str, String)> {
    let events = app.stage_events();
    if (app.focus == Focus::Stage || !app.follow)
        && let Some(e) = events.get(app.ev_sel)
    {
        let body = e.detail.clone().unwrap_or_else(|| e.title.clone());
        let label = match e.kind {
            EventKind::Prompt => "Mensaje",
            EventKind::Reply => "Respuesta",
            EventKind::Shell => "Comando",
            _ => "Detalle",
        };
        return Some((label, body));
    }
    let reply = match child {
        Some(c) => c.last_reply.clone(),
        None => info.and_then(|i| i.last_reply.clone()),
    }?;
    Some((if child.is_some_and(|c| !c.running) { "Informe final" } else { "Última respuesta" }, reply))
}

fn draw_activity(f: &mut Frame, app: &mut App, p: &Pane, info: Option<&SessionInfo>, child: Option<&Child>, area: Rect) {
    let events: Vec<Event> = app.stage_events().to_vec();
    if events.is_empty() {
        let line = if info.is_none() && !app.probed.contains(&p.pane_id) {
            Line::from(vec![fg(format!("{} ", spin(app)), ACCENT), dim("Leyendo la sesión…")])
        } else {
            Line::from(dim("Todavía no hay actividad en esta sesión"))
        };
        f.render_widget(Paragraph::new(line).centered(), Rect::new(area.x, area.y + area.height / 3, area.width, 1));
        return;
    }
    // Plan a la derecha si lo hay y sobra ancho.
    let todos: &[Todo] = if child.is_none() { info.map(|i| i.todos.as_slice()).unwrap_or(&[]) } else { &[] };
    let (main, side) = if !todos.is_empty() && area.width >= 110 && !app.zoom {
        let [m, _, s] = Layout::horizontal([Constraint::Min(40), Constraint::Length(2), Constraint::Length(38)]).areas(area);
        (m, Some(s))
    } else {
        (area, None)
    };
    if let Some(s) = side {
        draw_plan(f, todos, s);
    }

    // Reparto vertical entre la línea de tiempo y el texto de detalle.
    let detail = detail_text(app, info, child);
    let w = main.width as usize;
    let detail_lines: Vec<Line<'static>> = detail.as_ref().map(|(_, body)| markdown(body, w.saturating_sub(2))).unwrap_or_default();
    let detail_h = if app.zoom {
        main.height
    } else if detail_lines.is_empty() {
        0
    } else {
        (detail_lines.len() as u16 + 1).min(((u32::from(main.height) * 45 / 100) as u16).max(4)).min(main.height.saturating_sub(4))
    };
    // La línea de tiempo ocupa lo que necesita y el texto va justo debajo: sin huecos en medio.
    let rows = events.len() + events.iter().skip(1).filter(|e| e.kind == EventKind::Prompt).count();
    let room = main.height.saturating_sub(detail_h);
    let timeline_h = if app.zoom { 0 } else { room.min(rows as u16 + 1) };
    let [timeline, detail_area, _] =
        Layout::vertical([Constraint::Length(timeline_h), Constraint::Length(detail_h), Constraint::Min(0)]).areas(main);

    if timeline.height > 0 {
        draw_timeline(f, app, &events, timeline);
    }
    if let Some((label, _)) = detail
        && detail_h >= 2
    {
        let total = detail_lines.len();
        let h = detail_h as usize - 1;
        app.detail_top = app.detail_top.min(total.saturating_sub(h));
        let more = total > h;
        let mut head = vec![faint("── "), fg(label, DIM), Span::raw(" ")];
        let tail = if app.zoom {
            " z reducir · J K desplazar ".to_string()
        } else if more {
            format!(" {}–{} de {total} · z ampliar ", app.detail_top + 1, (app.detail_top + h).min(total))
        } else {
            String::new()
        };
        let rule = w.saturating_sub(spans_width(&head) + width(&tail));
        head.push(faint("─".repeat(rule)));
        head.push(faint(tail));
        let mut lines = vec![Line::from(head)];
        lines.extend(detail_lines.into_iter().skip(app.detail_top).take(h).map(|mut l| {
            l.spans.insert(0, Span::raw(" "));
            l
        }));
        f.render_widget(Paragraph::new(lines), detail_area);
    }
}

fn draw_timeline(f: &mut Frame, app: &mut App, events: &[Event], area: Rect) {
    let w = area.width as usize;
    let h = area.height as usize;
    let show_sel = app.focus == Focus::Stage || !app.follow;
    // Filas: una por evento, con una línea en blanco antes de cada mensaje del usuario (cambio de turno).
    let mut rows: Vec<Option<usize>> = Vec::with_capacity(events.len() + 16);
    for (i, e) in events.iter().enumerate() {
        if e.kind == EventKind::Prompt && i > 0 {
            rows.push(None);
        }
        rows.push(Some(i));
    }
    let sel_row = rows.iter().position(|r| *r == Some(app.ev_sel)).unwrap_or(rows.len().saturating_sub(1));
    if app.follow {
        app.ev_top = rows.len().saturating_sub(h);
    } else if sel_row < app.ev_top {
        app.ev_top = sel_row;
    } else if sel_row >= app.ev_top + h {
        app.ev_top = sel_row + 1 - h;
    }
    app.ev_top = app.ev_top.min(rows.len().saturating_sub(h));

    let mut lines = Vec::with_capacity(h);
    let mut last_clock = String::new();
    for (y, row) in rows.iter().enumerate().skip(app.ev_top).take(h) {
        let Some(i) = *row else {
            lines.push(Line::default());
            last_clock.clear();
            continue;
        };
        let e = &events[i];
        let selected = show_sel && i == app.ev_sel;
        let clock = fmt_clock(e.ts);
        let time = if clock == last_clock { "     ".to_string() } else { clock.clone() };
        last_clock = clock;
        let state = match e.state {
            EventState::Running => fg(spin(app), ACCENT),
            EventState::Error => fg("✗", ERR),
            EventState::Ok => Span::raw(" "),
        };
        let mut left = vec![faint(time), Span::raw(" "), state, Span::raw(" ")];
        let (verb, color) = event_verb(e.kind);
        match e.kind {
            EventKind::Prompt => {
                left.push(fg("❯ ", ACCENT));
                left.push(Span::styled(e.title.clone(), Style::new().fg(Color::White).add_modifier(Modifier::BOLD)));
            }
            EventKind::Reply => {
                left.push(fg("◆ ", DIM));
                left.push(raw(e.title.clone()));
            }
            _ => {
                left.push(fg(pad(verb, 7), color));
                let dimmed = matches!(e.kind, EventKind::Read | EventKind::Search) && e.state == EventState::Ok;
                let title = match e.kind {
                    EventKind::Read | EventKind::Edit => short_path(&e.title, w.saturating_sub(34).max(24)),
                    _ => e.title.clone(),
                };
                left.push(if e.state == EventState::Error {
                    fg(title, ERR)
                } else if dimmed {
                    dim(title)
                } else {
                    raw(title)
                });
            }
        }
        // A la derecha: el resumen corto del cambio (+12 −3) o lo que tardó.
        let mut right = Vec::new();
        if e.kind == EventKind::Edit
            && let Some(d) = e.detail.as_deref().filter(|d| d.len() <= 16 && !d.contains('\n'))
        {
            right.push(fg(d.to_string(), OK));
            right.push(Span::raw(" "));
        }
        if let Some(ms) = e.duration_ms.filter(|ms| *ms >= 1000) {
            right.push(faint(fmt_ms(ms)));
        } else if e.state == EventState::Running {
            right.push(fg(fmt_dur(now().saturating_sub(e.ts)), ACCENT));
        }
        right.push(Span::raw(" "));
        lines.push(with_bg(fit(left, right, w), selected.then_some(SEL_BG)));
        app.hits.push((Rect::new(area.x, area.y + (y - app.ev_top) as u16, area.width, 1), Hit::Event(i)));
    }
    // Con pocos eventos, pegados arriba; la lista crece hacia abajo.
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_plan(f: &mut Frame, todos: &[Todo], area: Rect) {
    let w = area.width as usize;
    let done = todos.iter().filter(|t| t.state == TodoState::Done).count();
    let mut lines = vec![Line::from(vec![fg("Plan", DIM), faint(format!("  {done} de {}", todos.len()))]), Line::default()];
    // Si no cabe entero, centrar la ventana en la tarea activa.
    let h = (area.height as usize).saturating_sub(2);
    let active = todos.iter().position(|t| t.state == TodoState::Active).or_else(|| todos.iter().position(|t| t.state == TodoState::Pending)).unwrap_or(0);
    let top = if todos.len() <= h { 0 } else { active.saturating_sub(h / 2).min(todos.len() - h) };
    for t in todos.iter().skip(top).take(h) {
        let (mark, color, style) = match t.state {
            TodoState::Done => ("✓", OK, Style::new().fg(DIM)),
            TodoState::Active => ("▸", ACCENT, Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
            TodoState::Pending => ("○", FAINT, Style::new().fg(TEXT)),
        };
        lines.push(Line::from(vec![fg(format!("{mark} "), color), Span::styled(trunc(&t.text, w.saturating_sub(2)), style)]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Pinta las últimas líneas de la pantalla de un panel, ancladas abajo.
fn paint_screen(f: &mut Frame, lines: &[Line<'static>], up: usize, area: Rect) {
    let h = area.height as usize;
    let end = lines.len().saturating_sub(up);
    let start = end.saturating_sub(h);
    let view: Vec<Line> = lines[start..end].iter().map(|l| ansi::clip(l, area.width as usize)).collect();
    f.render_widget(Paragraph::new(view), area);
}

fn draw_screen(f: &mut Frame, app: &mut App, p: &Pane, area: Rect) {
    let direct = matches!(app.mode, Mode::Direct);
    let banner_h = if direct { 1 } else { 0 };
    let [body, banner] = Layout::vertical([Constraint::Min(1), Constraint::Length(banner_h)]).areas(area);
    match app.screens.get(&p.pane_id).cloned() {
        Some(lines) => {
            app.screen_up = app.screen_up.min(lines.len().saturating_sub(body.height as usize));
            paint_screen(f, &lines, app.screen_up, body);
        }
        None => {
            let line = Line::from(vec![fg(format!("{} ", spin(app)), ACCENT), dim("Leyendo la pantalla…")]);
            f.render_widget(Paragraph::new(line).centered(), Rect::new(body.x, body.y + body.height / 3, body.width, 1));
        }
    }
    if direct {
        let txt = format!(" CONTROL DIRECTO   lo que teclees va a {}   ·   ctrl+q para salir ", App::name(p));
        let st = Style::new().bg(WARN).fg(Color::Black).add_modifier(Modifier::BOLD);
        f.render_widget(Paragraph::new(Span::styled(pad(&txt, banner.width as usize), st)), banner);
    }
}

fn draw_files(f: &mut Frame, app: &mut App, info: Option<&SessionInfo>, area: Rect) {
    let Some(info) = info else { return };
    let w = area.width as usize;
    let h = area.height as usize;
    let mut files: Vec<&FileTouch> = info.files.iter().collect();
    files.sort_by_key(|x| std::cmp::Reverse(x.last_ts));
    app.files_top = app.files_top.min(files.len().saturating_sub(h));
    let t = now();
    let lines: Vec<Line> = files
        .iter()
        .skip(app.files_top)
        .take(h)
        .map(|x| {
            let (dir, file) = match x.path.rsplit_once('/') {
                Some((d, f)) => (format!("{d}/"), f.to_string()),
                None => (String::new(), x.path.clone()),
            };
            let n = if x.edits == 1 { "1 cambio".to_string() } else { format!("{} cambios", x.edits) };
            let right = vec![dim(pad_left(&n, 11)), faint(pad_left(&format!("hace {}", fmt_short(t.saturating_sub(x.last_ts))), 10)), Span::raw(" ")];
            Line::from(fit(vec![fg(" ✎ ", OK), dim(dir), raw(file)], right, w))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_input(f: &mut Frame, inp: &Input, area: Rect) {
    if area.height < 3 {
        return;
    }
    let block = panel(vec![Span::raw(" "), fg(inp.title.clone(), ACCENT), Span::raw(" ")], ACCENT);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let w = (inner.width as usize).saturating_sub(2).max(1);
    // Ajuste duro por columnas: así el cursor se coloca sin ambigüedad.
    let mut lines: Vec<String> = vec![String::new()];
    let (mut cx, mut cy) = (0usize, 0usize);
    let mut col = 0;
    for (n, c) in inp.text.chars().enumerate() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if c == '\n' || col + cw > w {
            lines.push(String::new());
            col = 0;
            if c == '\n' {
                if n == inp.cursor { cy = lines.len() - 2; cx = width(lines[cy].as_str()); }
                continue;
            }
        }
        if n == inp.cursor { (cx, cy) = (col, lines.len() - 1); }
        lines.last_mut().unwrap().push(c);
        col += cw;
    }
    if inp.cursor >= inp.text.chars().count() {
        (cx, cy) = (col, lines.len() - 1);
    }
    let h = inner.height as usize;
    let top = (cy + 1).saturating_sub(h);
    let view: Vec<Line> = lines.iter().skip(top).take(h).map(|l| Line::from(vec![Span::raw(" "), raw(l.clone())])).collect();
    f.render_widget(Paragraph::new(view), inner);
    if inp.text.is_empty() {
        f.render_widget(Paragraph::new(Line::from(vec![Span::raw(" "), faint("Escribe el mensaje…")])), inner);
    }
    f.set_cursor_position(Position::new(inner.x + 1 + cx as u16, inner.y + (cy - top) as u16));
}

// ─────────────────────────────── mosaico ───────────────────────────────

fn draw_wall(f: &mut Frame, app: &mut App, area: Rect) {
    let cards = app.wall_items();
    if cards.is_empty() {
        f.render_widget(Paragraph::new(dim("No hay agentes en este ámbito")).centered(), Rect::new(area.x, area.y + area.height / 3, area.width, 1));
        return;
    }
    let n = cards.len();
    let cols = match n {
        1 => 1,
        2..=4 => 2,
        5..=9 => 3,
        _ => 4,
    }
    .min((area.width / 36).max(1) as usize);
    app.wall_cols = cols;
    let rows_total = n.div_ceil(cols);
    // Como mucho las filas que quepan con una altura cómoda; el resto se alcanza desplazando.
    let rows_fit = ((area.height / 9).max(1) as usize).min(rows_total);
    let sel_pos = cards.iter().position(|&c| c == app.sel).unwrap_or(0);
    let sel_row = sel_pos / cols;
    let top_row = sel_row.saturating_sub(rows_fit - 1).min(rows_total - rows_fit);
    let top_row = if sel_row < top_row { sel_row } else { top_row };
    let ch = area.height / rows_fit as u16;
    let cw = area.width / cols as u16;
    for (pos, &item) in cards.iter().enumerate().skip(top_row * cols).take(rows_fit * cols) {
        let (r, c) = ((pos / cols - top_row) as u16, (pos % cols) as u16);
        // La última columna y la última fila se quedan con las celdas sobrantes.
        let wd = if c as usize == cols - 1 { area.width - cw * c } else { cw };
        let ht = if r as usize == rows_fit - 1 { area.height - ch * r } else { ch };
        let rect = Rect::new(area.x + cw * c, area.y + ch * r, wd, ht);
        let Item::Agent(pi) = app.items[item] else { continue };
        let p = app.panes[pi].clone();
        let selected = item == app.sel;
        let color = if selected {
            ACCENT
        } else if p.status == Status::Blocked {
            WARN
        } else {
            BORDER
        };
        let mark = if app.marks.contains(&p.pane_id) { fg("◆ ", VIOLET) } else { Span::raw("") };
        let title = vec![Span::raw(" "), mark, status_span(app, &p), Span::raw(" "), bold(App::name(&p)), Span::raw(" ")];
        let mut kind = vec![fg(format!(" {} ", kind_name(p.kind, p.agent_raw.as_deref())), kind_color(p.kind))];
        if app.scope == Scope::All {
            kind.insert(0, dim(format!(" {} ·", trunc(&p.workspace_label, (wd as usize / 3).max(8)))));
        }
        let kind = Line::from(kind).right_aligned();
        let (what, when) = now_line(app, &p);
        let inner_w = (wd as usize).saturating_sub(4);
        let mut bottom = vec![Span::raw(" ")];
        bottom.extend(clip_spans(what, inner_w.saturating_sub(width(&when) + 3)));
        bottom.push(Span::raw(" "));
        let mut block = panel(title, color).title(kind).title_bottom(Line::from(bottom));
        if !when.is_empty() {
            block = block.title_bottom(Line::from(faint(format!(" {when} "))).right_aligned());
        }
        if selected {
            block = block.border_type(BorderType::Thick);
        }
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        app.hits.push((rect, Hit::Card(item)));
        if inner.width < 4 || inner.height == 0 {
            continue;
        }
        let inner = Rect::new(inner.x + 1, inner.y, inner.width - 2, inner.height);
        match app.screens.get(&p.pane_id).cloned() {
            Some(lines) => paint_screen(f, &lines, 0, inner),
            None => f.render_widget(Paragraph::new(faint("…")).centered(), Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1)),
        }
    }
    if rows_fit < rows_total {
        let txt = format!(" fila {} de {rows_total} ", sel_row + 1);
        f.render_widget(Paragraph::new(faint(txt.clone())), Rect::new(area.x + area.width.saturating_sub(width(&txt) as u16 + 2), area.y + area.height - 1, width(&txt) as u16, 1));
    }
}

// ─────────────────────────────── ventanas ───────────────────────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_explanation(f: &mut Frame, ex: &mut Explanation, area: Rect) {
    let rect=centered(area,110,area.height.saturating_sub(2));
    let block=panel(vec![fg(format!(" Explicación · {} ",ex.title),VIOLET)],VIOLET);
    let inner=block.inner(rect);f.render_widget(Clear,rect);f.render_widget(block,rect);
    if inner.width<4 || inner.height<3 {return;}
    let [header,body,footer]=Layout::vertical([Constraint::Length(2),Constraint::Min(1),Constraint::Length(1)]).areas(inner);
    let status=if ex.loading{" · consultando…"}else{""};
    f.render_widget(Paragraph::new(Line::from(dim(format!(" {} · {}{}",ex.model,ex.effort,status)))),header);
    let lines=if let Some(error)=&ex.error {vec![Line::from(fg(error.clone(),ERR))]}
        else if ex.text.is_empty() {vec![Line::from(dim(" Preparando la explicación…"))]}
        else {markdown(&ex.text,body.width.saturating_sub(2) as usize)};
    ex.scroll=ex.scroll.min(lines.len().saturating_sub(body.height as usize));
    let view:Vec<_>=lines.into_iter().skip(ex.scroll).take(body.height as usize).map(|mut line|{line.spans.insert(0,Span::raw(" "));line}).collect();
    f.render_widget(Paragraph::new(view),body);
    f.render_widget(Paragraph::new(faint(" ↑↓ desplazar · e actualizar · esc cerrar ")),footer);
}

fn draw_confirm(f: &mut Frame, c: &Confirm, area: Rect) {
    let w = 64u16.min(area.width.saturating_sub(4));
    let body = wrap(&c.body, w as usize - 6);
    let r = centered(area, w, body.len() as u16 + 6);
    let color = if c.danger { ERR } else { WARN };
    let block = panel(vec![], color);
    let inner = block.inner(r);
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    let mut lines = vec![Line::default(), Line::from(vec![Span::raw("  "), bold(c.title.clone())])];
    lines.extend(body.into_iter().map(|l| Line::from(vec![Span::raw("  "), dim(l)])));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(" ⏎ Sí ", Style::new().bg(color).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(" esc No ", Style::new().bg(SEL_BG_DIM).fg(TEXT)),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_new(f: &mut Frame, app: &App, n: &NewAgent, area: Rect) {
    let presets = &app.cfg.presets;
    let r = centered(area, 58, presets.len() as u16 + 6);
    let title = if n.in_place { " Lanzar un agente en este panel " } else { " Nuevo agente " };
    let block = panel(vec![bold(title)], ACCENT);
    let inner = block.inner(r);
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    let w = inner.width as usize;
    let mut lines = vec![Line::default()];
    let visible = inner.height.saturating_sub(4) as usize;
    let top = n.sel.saturating_add(1).saturating_sub(visible);
    for (i, p) in presets.iter().enumerate().skip(top).take(visible) {
        let sel = i == n.sel;
        let left = vec![
            Span::raw("  "),
            fg(format!("{} ", if i < 9 { (i + 1).to_string() } else { " ".into() }), if sel { ACCENT } else { FAINT }),
            if sel { bold(p.name.clone()) } else { raw(p.name.clone()) },
        ];
        let right = vec![dim(p.kind.clone()), Span::raw("  ")];
        lines.push(with_bg(fit(left, right, w), sel.then_some(SEL_BG)));
    }
    lines.push(Line::default());
    let hint = if n.in_place { "Se lanza en la shell del panel seleccionado" } else { "Se abre en un panel nuevo, debajo del seleccionado" };
    lines.push(Line::from(vec![Span::raw("  "), faint(hint)]));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(f: &mut Frame, area: Rect) {
    const LEFT: &[(&str, &str)] = &[
        ("", "Moverse"),
        ("↑ ↓", "Elegir agente o evento"),
        ("→ ←  tab", "Entrar y salir del detalle"),
        ("1 2 3", "Actividad · Pantalla · Ficheros"),
        ("a", "Siguiente que pide atención"),
        ("/", "Filtrar por nombre o tarea"),
        ("s", "Este espacio / todos"),
        ("v", "Mosaico de pantallas en vivo"),
        ("", ""),
        ("", "Leer"),
        ("z", "Ampliar la respuesta o el detalle"),
        ("J K", "Desplazar ese texto"),
        ("y", "Copiar la última respuesta"),
        ("e", "Explicar qué hace y por qué (IA)"),
        ("H", "Ver todos los subagentes"),
        ("q  esc", "Salir"),
    ];
    const RIGHT: &[(&str, &str)] = &[
        ("", "Actuar"),
        ("⏎", "Ir al panel del agente"),
        ("m", "Enviarle un mensaje"),
        ("c", "Decirle que continúe"),
        ("i", "Interrumpir el turno"),
        ("t", "Control directo: teclear en su panel"),
        ("r", "Reanudar o lanzar un agente apagado"),
        ("n", "Nuevo agente en un panel nuevo"),
        ("x", "Cerrar el panel"),
        ("", ""),
        ("", "Varios a la vez"),
        ("espacio", "Marcar agentes; m escribe a todos"),
        ("I", "Interrumpir a todos los que trabajan"),
    ];
    let row = |(k, d): &(&str, &str)| -> Vec<Span<'static>> {
        if k.is_empty() {
            vec![fg(pad(d, 49), DIM)]
        } else {
            vec![Span::styled(pad(k, 11), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)), raw(pad(d, 38))]
        }
    };
    // Dos columnas si caben; si no, una detrás de otra.
    let two = area.width >= 110;
    let mut lines = vec![Line::default()];
    if two {
        for i in 0..LEFT.len().max(RIGHT.len()) {
            let mut l = vec![Span::raw("  ")];
            l.extend(LEFT.get(i).map(row).unwrap_or_else(|| vec![Span::raw(" ".repeat(49))]));
            l.push(Span::raw("  "));
            l.extend(RIGHT.get(i).map(row).unwrap_or_default());
            lines.push(Line::from(l));
        }
    } else {
        for item in LEFT.iter().chain([("", "")].iter()).chain(RIGHT.iter()) {
            let mut l = vec![Span::raw("  ")];
            l.extend(row(item));
            lines.push(Line::from(l));
        }
    }
    let r = centered(area, if two { 106 } else { 55 }, lines.len() as u16 + 3);
    let block = panel(vec![bold(" Ayuda ")], ACCENT);
    let inner = block.inner(r);
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '↑' => KeyCode::Up,
                '↓' => KeyCode::Down,
                '→' => KeyCode::Right,
                '←' => KeyCode::Left,
                '⏎' => KeyCode::Enter,
                '⎋' => KeyCode::Esc,
                c => KeyCode::Char(c),
            };
            app.on_event(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
    }

    fn text(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect()
    }

    /// Ningún tamaño de ventana ni combinación de modo debe hacer panic al pintar.
    #[test]
    fn pinta_en_cualquier_tamano_y_modo() {
        let modos = ["", "v", "?", "↑i", "↑m hola\nmundo", "n", "t", "/back", "↑↑↑↑→↓↓z", "↑3", "s", "sv", "↑↑↑", "↑↑↑↑2", " ↓ m", "H", "↓r", "↓x"];
        for keys in modos {
            for (w, h) in [(40, 10), (41, 11), (60, 14), (80, 24), (100, 30), (120, 36), (170, 46), (240, 70), (300, 20), (50, 60)] {
                let mut app = crate::demo::app(crate::config::Config::default());
                let _ = text(&mut app, w, h);
                press(&mut app, keys);
                let _ = text(&mut app, w, h);
                app.tick();
                let _ = text(&mut app, w, h);
            }
        }
    }

    #[test]
    fn lo_esencial_esta_en_pantalla() {
        let mut app = crate::demo::app(crate::config::Config::default());
        let t = text(&mut app, 170, 46);
        // Al abrir se elige lo que pide atención y se enseña su pantalla.
        assert!(t.contains("frontend") && t.contains("espera tu respuesta"), "{t}");
        assert!(t.contains("Permission required"), "{t}");
        assert!(t.contains("2 trabajando") && t.contains("1 espera respuesta") && t.contains("1 ha terminado"), "{t}");
        // El líder: su actividad, su plan y sus subagentes.
        press(&mut app, "↑↑↑↑");
        let t = text(&mut app, 170, 46);
        for needle in ["lider", "Integra la pasarela", "docs/pasarela.md", "Mapear cómo se guardan", "Revisar el modelo de pedidos", "Última respuesta"] {
            assert!(t.contains(needle), "falta «{needle}»:\n{t}");
        }
        // Un subagente: su encargo y su actividad.
        press(&mut app, "↓↓");
        let t = text(&mut app, 170, 46);
        assert!(t.contains("subagente de lider") && t.contains("Informe final") && t.contains("Nunca"), "{t}");
        // El mosaico enseña las cuatro pantallas.
        press(&mut app, "v");
        let t = text(&mut app, 170, 46);
        assert!(t.contains("18 passed") && t.contains("Permission required") && t.contains("Working"), "{t}");
    }

    #[test]
    fn ancho_exacto_de_las_filas() {
        let l = fit(vec![raw("abcdefghij")], vec![raw("xyz")], 8);
        assert_eq!(spans_width(&l), 8);
        let l = fit(vec![raw("日本語日本語")], vec![raw("12")], 9);
        assert_eq!(spans_width(&l), 9);
        let l = fit(vec![raw("ab")], vec![], 6);
        assert_eq!(spans_width(&l), 6);
        assert_eq!(spans_width(&clip_spans(vec![raw("hola "), raw("mundo")], 7)), 7);
    }
    #[test]
    fn extreme_terminal_dimensions_and_token_counts_do_not_panic() {
        for (w,h) in [(3000,10),(40,1600)] {
            let mut app=crate::demo::app(crate::config::Config::default());press(&mut app,"↑1");
            let pane=app.sel_pane().unwrap().pane_id.clone();let mut info=app.info(&pane).unwrap().clone();
            info.tokens_in=u64::MAX;info.tokens_out=1;info.updated_at=Some(i64::MIN);
            app.sessions.insert(pane,std::sync::Arc::new(info));let _=text(&mut app,w,h);
        }
    }

    #[test]
    fn wrapped_input_cursor_is_at_the_wrapped_character() {
        let mut term=Terminal::new(TestBackend::new(14,6)).unwrap();
        let input=Input {title:"Message".into(),targets:vec![],text:"1234567890X".into(),cursor:10};
        let frame=term.draw(|f|draw_input(f,&input,Rect::new(0,0,14,6))).unwrap();
        assert_eq!(frame.area.width,14);
        use ratatui::backend::Backend;
        assert_eq!(term.backend_mut().get_cursor_position().unwrap(),Position::new(2,2));
    }

    #[test]
    fn preset_selection_remains_visible_after_scrolling() {
        let mut app=crate::demo::app(crate::config::Config::default());
        app.cfg.presets=(0..30).map(|n|crate::config::Preset{name:format!("preset-{n}"),kind:"test".into(),args:vec![]}).collect();
        app.mode=Mode::New(NewAgent {sel:29,pane:"none".into(),in_place:false});
        assert!(text(&mut app,80,24).contains("preset-29"));
    }

    #[test]
    fn explanation_renders_scrolls_and_handles_small_windows() {
        for (w,h) in [(40,10),(80,24),(170,46)] {
            let mut app=crate::demo::app(crate::config::Config::default());press(&mut app,"e");
            let Mode::Explain(ex)=&mut app.mode else {panic!();};ex.error=None;ex.text=(0..100).map(|n|format!("Acción {n}\n\n")).collect();ex.loading=false;
            let t=text(&mut app,w,h);assert!(t.contains("Explicación"));
            app.on_event(TermEvent::Key(KeyEvent::new(KeyCode::End,KeyModifiers::NONE)));
            let t=text(&mut app,w,h);assert!(t.contains("Acción 99"));
            press(&mut app,"⎋");assert!(matches!(app.mode,Mode::Normal));
        }
    }

}
