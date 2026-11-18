//! Paleta, glifos y utilidades de texto de la interfaz.

use crate::model::{EventKind, Kind, Status};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const BG_BAR: Color = Color::Rgb(0x1b, 0x1f, 0x26);
pub const ACCENT: Color = Color::Rgb(0x4f, 0xa3, 0xff);
pub const DIM: Color = Color::Rgb(0x80, 0x86, 0x90);
pub const FAINT: Color = Color::Rgb(0x4a, 0x50, 0x5a);
pub const BORDER: Color = Color::Rgb(0x3b, 0x41, 0x4b);
pub const TEXT: Color = Color::Rgb(0xd7, 0xdb, 0xe0);
pub const SEL_BG: Color = Color::Rgb(0x1f, 0x3a, 0x5a);
pub const SEL_BG_DIM: Color = Color::Rgb(0x26, 0x2b, 0x33);
pub const OK: Color = Color::Rgb(0x3f, 0xb9, 0x50);
pub const ERR: Color = Color::Rgb(0xf8, 0x51, 0x49);
pub const WARN: Color = Color::Rgb(0xe3, 0xb3, 0x41);
pub const CODE: Color = Color::Rgb(0x56, 0xb6, 0xc2);
pub const VIOLET: Color = Color::Rgb(0xb3, 0x8d, 0xf7);

pub const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn status_color(s: Status) -> Color {
    match s {
        Status::Working => ACCENT,
        Status::Blocked => WARN,
        Status::Done => OK,
        Status::Idle => DIM,
        Status::Unknown => FAINT,
    }
}

/// Glifo fijo del estado (el de `Working` lo sustituye el spinner al pintar).
pub fn status_glyph(s: Status) -> &'static str {
    match s {
        Status::Working => "●",
        Status::Blocked => "▲",
        Status::Done => "✓",
        Status::Idle => "○",
        Status::Unknown => "·",
    }
}

pub fn status_word(s: Status) -> &'static str {
    match s {
        Status::Working => "Trabajando",
        Status::Blocked => "Espera respuesta",
        Status::Done => "Ha terminado",
        Status::Idle => "En reposo",
        Status::Unknown => "Sin agente",
    }
}

pub fn kind_name(k: Kind, raw: Option<&str>) -> String {
    match k {
        Kind::Claude => "Claude".into(),
        Kind::Codex => "Codex".into(),
        Kind::OpenCode => "OpenCode".into(),
        Kind::Omp => "omp".into(),
        Kind::Other => raw.unwrap_or("agente").to_string(),
        Kind::Shell => "shell".into(),
    }
}

pub fn kind_color(k: Kind) -> Color {
    match k {
        Kind::Claude => Color::Rgb(0xd9, 0x77, 0x57),
        Kind::Codex => Color::Rgb(0x5f, 0xc9, 0xa8),
        Kind::OpenCode => Color::Rgb(0xe8, 0xc4, 0x7a),
        Kind::Omp => VIOLET,
        Kind::Other => CODE,
        Kind::Shell => FAINT,
    }
}

/// Verbo corto y color de cada tipo de evento en la línea de tiempo.
pub fn event_verb(k: EventKind) -> (&'static str, Color) {
    match k {
        EventKind::Prompt => ("", ACCENT),
        EventKind::Reply => ("", TEXT),
        EventKind::Read => ("leer", DIM),
        EventKind::Edit => ("editar", OK),
        EventKind::Shell => ("shell", WARN),
        EventKind::Search => ("buscar", CODE),
        EventKind::Web => ("web", CODE),
        EventKind::Task => ("agente", VIOLET),
        EventKind::Todo => ("plan", VIOLET),
        EventKind::Tool => ("usar", DIM),
        EventKind::Notice => ("aviso", WARN),
    }
}

// ───────────────────────────── spans ─────────────────────────────

pub fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(DIM))
}

pub fn faint(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(FAINT))
}

pub fn bold(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(Color::White).add_modifier(Modifier::BOLD))
}

pub fn fg(s: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(c))
}

pub fn raw(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(TEXT))
}

pub fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| width(&s.content)).sum()
}

// ───────────────────────────── texto ─────────────────────────────

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Recorta a `w` columnas con "…".
pub fn trunc(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut acc = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if acc + cw > w - 1 {
            break;
        }
        acc += cw;
        out.push(c);
    }
    out.push('…');
    out
}

/// Recorta por la izquierda (deja el final): útil para rutas.
pub fn trunc_left(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut rev: Vec<char> = Vec::new();
    let mut acc = 0;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if acc + cw > w - 1 {
            break;
        }
        acc += cw;
        rev.push(c);
    }
    std::iter::once('…').chain(rev.into_iter().rev()).collect()
}

pub fn pad(s: &str, w: usize) -> String {
    let t = trunc(s, w);
    let n = w.saturating_sub(width(&t));
    format!("{t}{}", " ".repeat(n))
}

pub fn pad_left(s: &str, w: usize) -> String {
    let t = trunc(s, w);
    let n = w.saturating_sub(width(&t));
    format!("{}{t}", " ".repeat(n))
}

/// Parte un texto en líneas de ancho `w` (por palabras; las palabras largas se cortan).
pub fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out = Vec::new();
    for raw in s.lines() {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let indent: String = raw.chars().take_while(|c| *c == ' ').take(w / 2).collect();
        let mut cur = String::new();
        for word in raw.split_whitespace() {
            if !cur.is_empty() && width(&cur) + 1 + width(word) > w {
                out.push(std::mem::take(&mut cur));
                cur.push_str(&indent);
            } else if cur.is_empty() {
                cur.push_str(&indent);
            } else {
                cur.push(' ');
            }
            let mut word = word.to_string();
            while width(&cur) + width(&word) > w {
                let room = w.saturating_sub(width(&cur)).max(1);
                let mut cut = String::new();
                let mut acc = 0;
                for c in word.chars() {
                    let cw = c.width().unwrap_or(0);
                    if acc + cw > room && !cut.is_empty() {
                        break;
                    }
                    acc += cw;
                    cut.push(c);
                }
                word = word[cut.len()..].to_string();
                cur.push_str(&cut);
                out.push(std::mem::take(&mut cur));
                cur.push_str(&indent);
            }
            cur.push_str(&word);
        }
        out.push(cur);
    }
    out
}

/// Markdown mínimo → líneas con estilo (títulos, viñetas, **negrita**, `código`, bloques).
pub fn markdown(s: &str, w: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut fence = false;
    for src in s.lines() {
        if src.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            lines.push(Line::from(Span::styled(format!("  {}", trunc(src, w.saturating_sub(2))), Style::new().fg(CODE))));
            continue;
        }
        if src.trim().is_empty() {
            // Una sola línea en blanco entre párrafos, nunca varias seguidas.
            if lines.last().is_some_and(|l: &Line| !l.spans.is_empty()) {
                lines.push(Line::default());
            }
            continue;
        }
        for l in wrap(src, w) {
            let t = l.trim_start();
            if let Some(h) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
                lines.push(Line::from(Span::styled(h.replace("**", ""), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))));
                continue;
            }
            let (prefix, body) = match t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                Some(b) => (format!("{}• ", " ".repeat(width(&l) - width(t))), b.to_string()),
                None => (String::new(), l.clone()),
            };
            let mut spans = vec![Span::styled(prefix, Style::new().fg(ACCENT))];
            spans.extend(inline_md(&body));
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn inline_md(s: &str) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let flush = |cur: &mut String, out: &mut Vec<Span<'static>>, bold: bool, code: bool| {
        if cur.is_empty() {
            return;
        }
        let mut st = Style::new().fg(TEXT);
        if bold {
            st = st.add_modifier(Modifier::BOLD).fg(Color::White);
        }
        if code {
            st = st.fg(CODE);
        }
        out.push(Span::styled(std::mem::take(cur), st));
    };
    while let Some(c) = chars.next() {
        if c == '*' && chars.peek() == Some(&'*') && !code {
            chars.next();
            flush(&mut cur, &mut out, bold, code);
            bold = !bold;
        } else if c == '`' {
            flush(&mut cur, &mut out, bold, code);
            code = !code;
        } else {
            cur.push(c);
        }
    }
    flush(&mut cur, &mut out, bold, code);
    out
}

// ───────────────────────────── tiempo y cifras ─────────────────────────────

/// Desfase local en segundos (una vez, vía `date +%z`).
fn tz_offset() -> i64 {
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if s.len() == 5 {
            let sign = if s.starts_with('-') { -1 } else { 1 };
            let h: i64 = s[1..3].parse().unwrap_or(0);
            let m: i64 = s[3..5].parse().unwrap_or(0);
            sign * (h * 3600 + m * 60)
        } else {
            0
        }
    })
}

/// "18:05" en hora local.
pub fn fmt_clock(t: i64) -> String {
    let s = (t + tz_offset()).rem_euclid(86400);
    format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
}

/// Duración compacta: "42s", "4m 12s", "2h 05m", "3d 4h".
pub fn fmt_dur(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        3600..86400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

/// Duración de una sola unidad, para columnas estrechas: "42s", "4m", "2h", "3d".
pub fn fmt_short(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// Milisegundos de una herramienta: "350ms", "2.4s", "1m 05s".
pub fn fmt_ms(ms: u64) -> String {
    match ms {
        0..1000 => format!("{ms}ms"),
        1000..10_000 => format!("{:.1}s", ms as f64 / 1000.0),
        _ => fmt_dur((ms / 1000) as i64),
    }
}

/// Cifras grandes: "950", "12.4k", "1.3M".
pub fn fmt_count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..10_000 => format!("{:.1}k", n as f64 / 1e3),
        10_000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// Nombre de modelo legible: quita proveedor y sufijos de fecha.
pub fn pretty_model(m: &str) -> String {
    let m = m.rsplit('/').next().unwrap_or(m);
    let m = m.strip_prefix("claude-").unwrap_or(m);
    // sufijo de fecha tipo -20251001
    let m = match m.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => m,
    };
    m.to_string()
}

/// Ruta para enseñar: el HOME se abrevia como `~`.
pub fn home_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    match std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
        Some(h) if s == h => "~".into(),
        Some(h) if s.starts_with(&format!("{h}/")) => format!("~{}", &s[h.len()..]),
        _ => s.into_owned(),
    }
}

/// Acorta una ruta larga conservando lo que la identifica: el principio y el final.
/// `/tmp/a/b/c/d/captura.png` → `/tmp/…/d/captura.png`.
pub fn short_path(p: &str, max: usize) -> String {
    if width(p) <= max || !p.contains('/') {
        return trunc_left(p, max);
    }
    let parts: Vec<&str> = p.split('/').collect();
    let n = parts.len();
    // Cabeza: la raíz (`~`, vacío de una ruta absoluta o primer directorio) y un nivel más.
    let head = if parts[0].is_empty() || parts[0] == "~" { parts[..2.min(n)].join("/") } else { parts[0].to_string() };
    for tail_n in (1..=3.min(n.saturating_sub(2))).rev() {
        let cand = format!("{head}/…/{}", parts[n - tail_n..].join("/"));
        if width(&cand) <= max {
            return cand;
        }
    }
    trunc_left(p, max)
}

/// Título de terminal sin los adornos que pone cada agente (prefijos, spinners, sufijo del proyecto).
pub fn clean_title(title: &str, kind: Kind) -> String {
    let mut t = title.trim();
    match kind {
        Kind::OpenCode => t = t.strip_prefix("OC | ").unwrap_or(t),
        Kind::Omp => {
            t = t.trim_start_matches(|c: char| c == 'π' || c == '>' || c.is_whitespace() || ('\u{2800}'..='\u{28ff}').contains(&c));
        }
        Kind::Codex => t = t.rsplit_once(" | ").map(|(a, _)| a).unwrap_or(t),
        _ => {}
    }
    let t = t.trim_start_matches(|c: char| ('\u{2800}'..='\u{28ff}').contains(&c) || c == '✳' || c.is_whitespace());
    t.to_string()
}

/// Quita las marcas de Markdown en línea para enseñar un texto en una sola línea.
pub fn plain(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

/// Barra de progreso de `w` celdas: tramo lleno con color y resto apagado.
pub fn gauge(pct: f32, w: usize, color: Color) -> Vec<Span<'static>> {
    let p = pct.clamp(0.0, 100.0);
    let full = ((p / 100.0) * w as f32).round() as usize;
    let full = if p > 0.0 { full.max(1) } else { 0 }.min(w);
    vec![fg("━".repeat(full), color), fg("━".repeat(w - full), FAINT)]
}

/// Color de una cuota o del contexto según lo llena que esté.
pub fn load_color(pct: f32) -> Color {
    if pct >= 90.0 {
        ERR
    } else if pct >= 70.0 {
        WARN
    } else {
        ACCENT
    }
}


/// Vuelca un buffer de ratatui como página HTML (revisión del diseño y capturas).
pub fn buffer_html(buf: &ratatui::buffer::Buffer) -> String {
    fn css(c: Color, default: &str) -> String {
        const BASE: [&str; 16] = [
            "#1e2227", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf", "#5c6370", "#ff7b86",
            "#b1e18b", "#f2d08a", "#7cc0ff", "#d992ee", "#6fd0dc", "#ffffff",
        ];
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Black => BASE[0].into(),
            Color::White => "#ffffff".into(),
            Color::Indexed(i) if i < 16 => BASE[i as usize].into(),
            Color::Indexed(i) if i >= 232 => {
                let v = 8 + (i - 232) as u32 * 10;
                format!("#{v:02x}{v:02x}{v:02x}")
            }
            Color::Indexed(i) => {
                let i = i - 16;
                let f = |v: u8| if v == 0 { 0 } else { 55 + v as u32 * 40 };
                format!("#{:02x}{:02x}{:02x}", f(i / 36), f((i / 6) % 6), f(i % 6))
            }
            _ => default.into(),
        }
    }
    let area = buf.area;
    let mut out = String::from(
        "<!doctype html><meta charset=utf-8><body style=\"margin:0;background:#0d1117\"><pre style=\"margin:0;padding:14px;\
         font:16px/18px 'DejaVu Sans Mono','JetBrains Mono',monospace;color:#d7dbe0;background:#11151b;display:inline-block\">",
    );
    for y in 0..area.height {
        let mut skip = 0;
        for x in 0..area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            skip = width(sym).saturating_sub(1);
            let (mut fgc, mut bgc) = (css(cell.fg, "#d7dbe0"), css(cell.bg, "transparent"));
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fgc, &mut bgc);
                if fgc == "transparent" {
                    fgc = "#11151b".into();
                }
            }
            // Celda de ancho fijo: un glifo que la fuente dibuje más ancho no descoloca la fila.
            let mut st = format!("display:inline-block;width:{}ch;height:18px;overflow:hidden;vertical-align:top;color:{fgc};background:{bgc}", skip + 1);
            if cell.modifier.contains(Modifier::BOLD) {
                st.push_str(";font-weight:bold");
            }
            if cell.modifier.contains(Modifier::DIM) {
                st.push_str(";opacity:.6");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                st.push_str(";font-style:italic");
            }
            let esc = sym.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            out.push_str(&format!("<span style=\"{st}\">{esc}</span>"));
        }
        out.push('\n');
    }
    out.push_str("</pre></body>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recortes() {
        assert_eq!(trunc("abcdef", 4), "abc…");
        assert_eq!(trunc_left("src/sources/claude.rs", 10), "…claude.rs");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad_left("ab", 4), "  ab");
    }

    #[test]
    fn wrap_respeta_el_ancho() {
        for w in [8, 9, 20] {
            let l = wrap("uno dos tres cuatro supercalifragilisticoespialidoso 日本語日本語日本語日本語", w);
            assert!(l.iter().all(|x| width(x) <= w), "{w}: {l:?}");
        }
        assert_eq!(wrap("uno dos tres cuatro", 9).len(), 3);
    }

    #[test]
    fn cifras() {
        assert_eq!(fmt_dur(42), "42s");
        assert_eq!(fmt_dur(252), "4m 12s");
        assert_eq!(fmt_dur(7500), "2h 05m");
        assert_eq!(fmt_short(7500), "2h");
        assert_eq!(fmt_ms(2400), "2.4s");
        assert_eq!(fmt_count(12_400), "12k");
        assert_eq!(fmt_count(1_300_000), "1.3M");
        assert_eq!(pretty_model("anthropic/claude-opus-5-5-20260601"), "opus-5-5");
        assert_eq!(pretty_model("gpt-6.1-sol"), "gpt-6.1-sol");
    }

    #[test]
    fn rutas_y_titulos() {
        assert_eq!(short_path("src/app.rs", 40), "src/app.rs");
        assert_eq!(short_path("/tmp/claude-1000/-home-x-Proyectos/9aa8/scratchpad/demo2.png", 40), "/tmp/…/9aa8/scratchpad/demo2.png");
        assert_eq!(short_path("~/Proyectos/herdr-agentview-wt/codex/src/sources/claude.rs", 34), "~/Proyectos/…/sources/claude.rs");
        assert!(width(&short_path("/a/muy/larga/ruta/con/un-fichero-de-nombre-larguisimo.rs", 20)) <= 20);
        assert_eq!(clean_title("OC | Formulario de pago", Kind::OpenCode), "Formulario de pago");
        assert_eq!(clean_title("π ⠹ 🧾 CEE: prueba", Kind::Omp), "🧾 CEE: prueba");
        assert_eq!(clean_title("Cumple el encargo | Proyectos", Kind::Codex), "Cumple el encargo");
        assert_eq!(clean_title("✳ Plan de pagos", Kind::Claude), "Plan de pagos");
    }

    #[test]
    fn barra() {
        let g = gauge(50.0, 10, ACCENT);
        assert_eq!((width(&g[0].content), width(&g[1].content)), (5, 5));
        assert_eq!(width(&gauge(1.0, 10, ACCENT)[0].content), 1);
        assert_eq!(width(&gauge(0.0, 10, ACCENT)[0].content), 0);
    }
}
