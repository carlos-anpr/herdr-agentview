//! Salida ANSI de un panel (`pane.read` con `format=ansi`) → líneas de ratatui.

use std::iter::Peekable;
use std::str::Chars;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Convierte el texto con secuencias ANSI en líneas con estilo.
pub fn parse(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut style = Style::default();
    let mut col = 0usize; // columna de pantalla de la línea en curso (para los tabuladores)

    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' => {
                flush(&mut spans, &mut buf, style);
                lines.push(take_line(&mut spans));
                col = 0;
            }
            '\r' => {}
            '\t' => {
                let pad = 8 - col % 8;
                buf.extend(std::iter::repeat_n(' ', pad));
                col += pad;
            }
            '\x1b' => escape(&mut it, &mut style, &mut spans, &mut buf),
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c => {
                buf.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    flush(&mut spans, &mut buf, style);
    lines.push(take_line(&mut spans));
    lines
}

/// Recorta la línea a `width` columnas conservando los estilos (sin `…`).
pub fn clip(line: &Line<'static>, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut col = 0usize;
    for span in &line.spans {
        let mut text = String::new();
        let mut lleno = false;
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if col + w > width {
                lleno = true;
                break;
            }
            col += w;
            text.push(c);
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
        if lleno {
            break;
        }
    }
    Line { style: line.style, alignment: line.alignment, spans }
}

fn flush(spans: &mut Vec<Span<'static>>, buf: &mut String, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

fn take_line(spans: &mut Vec<Span<'static>>) -> Line<'static> {
    if spans.is_empty() { Line::default() } else { Line::from(std::mem::take(spans)) }
}

/// Consume una secuencia de escape ya empezada (el `ESC` está consumido).
fn escape(it: &mut Peekable<Chars<'_>>, style: &mut Style, spans: &mut Vec<Span<'static>>, buf: &mut String) {
    match it.next() {
        // CSI: parámetros hasta la letra final. Solo `m` (SGR) tiene efecto.
        Some('[') => {
            let mut params = String::new();
            for c in it {
                if c.is_ascii() && (0x40..=0x7e).contains(&(c as u8)) {
                    if c == 'm' {
                        let before = *style;
                        apply_sgr(style, &params);
                        if *style != before {
                            flush(spans, buf, before);
                        }
                    }
                    return;
                }
                params.push(c);
            }
        }
        // OSC: hasta BEL o `ESC \`.
        Some(']') => {
            while let Some(c) = it.next() {
                match c {
                    '\x07' => break,
                    '\x1b' => {
                        if it.peek() == Some(&'\\') {
                            it.next();
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
        // `ESC` + un carácter, o `ESC` suelto al final.
        _ => {}
    }
}

/// Aplica los parámetros de un SGR. Los que no entiende los ignora.
fn apply_sgr(style: &mut Style, params: &str) {
    // Un parámetro con subparámetros (`4:3` subrayado ondulado, `58:2::r:g:b` color de subrayado)
    // solo interesa si es un color de texto o de fondo; de los demás se queda el primero.
    let mut toks: Vec<&str> = Vec::new();
    for group in if params.is_empty() { "0" } else { params }.split(';') {
        match group.split_once(':') {
            Some((head, _)) if head == "38" || head == "48" => toks.extend(group.split(':')),
            Some(("4", "0")) => toks.push("24"),
            Some(("58", _)) => {}
            Some((head, _)) => toks.push(head),
            None => toks.push(group),
        }
    }
    let mut i = 0;
    while i < toks.len() {
        let Some(n) = num(toks[i]) else {
            i += 1;
            continue;
        };
        i += 1;
        match n {
            0 => *style = Style::default(),
            1 => style.add_modifier.insert(Modifier::BOLD),
            2 => style.add_modifier.insert(Modifier::DIM),
            3 => style.add_modifier.insert(Modifier::ITALIC),
            4 => style.add_modifier.insert(Modifier::UNDERLINED),
            7 => style.add_modifier.insert(Modifier::REVERSED),
            9 => style.add_modifier.insert(Modifier::CROSSED_OUT),
            22 => style.add_modifier.remove(Modifier::BOLD | Modifier::DIM),
            23 => style.add_modifier.remove(Modifier::ITALIC),
            24 => style.add_modifier.remove(Modifier::UNDERLINED),
            27 => style.add_modifier.remove(Modifier::REVERSED),
            29 => style.add_modifier.remove(Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(Color::Indexed((n - 30) as u8)),
            39 => style.fg = Some(Color::Reset),
            40..=47 => style.bg = Some(Color::Indexed((n - 40) as u8)),
            49 => style.bg = Some(Color::Reset),
            90..=97 => style.fg = Some(Color::Indexed((n - 90 + 8) as u8)),
            100..=107 => style.bg = Some(Color::Indexed((n - 100 + 8) as u8)),
            38 | 48 | 58 => {
                let Some(color) = ext_color(&toks, &mut i) else { continue };
                match n {
                    38 => style.fg = Some(color),
                    48 => style.bg = Some(color),
                    _ => {} // 58: color del subrayado, no se pinta
                }
            }
            _ => {}
        }
    }
}

fn num(tok: &str) -> Option<u32> {
    if tok.is_empty() { Some(0) } else { tok.parse().ok() }
}

/// Color extendido tras `38`/`48`: `5;N` o `2;R;G;B`. Consume los tokens que usa.
fn ext_color(toks: &[&str], i: &mut usize) -> Option<Color> {
    let kind = num(toks.get(*i)?)?;
    *i += 1;
    match kind {
        5 => {
            let n = num(toks.get(*i)?)?;
            *i += 1;
            Some(Color::Indexed(n.min(255) as u8))
        }
        2 => {
            // `38:2::R:G:B` lleva un hueco (espacio de color) que aquí se descarta.
            if toks.get(*i).is_some_and(|t| t.is_empty()) {
                *i += 1;
            }
            let r = num(toks.get(*i)?)?;
            let g = num(toks.get(*i + 1)?)?;
            let b = num(toks.get(*i + 2)?)?;
            *i += 3;
            Some(Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subparametros_que_no_son_color() {
        let l = parse("\x1b[4:3;58:2::255:0:0msub\x1b[4:0m no \x1b[58;5;9;1mnegrita");
        assert_eq!(l[0].spans[0].style, Style::default().add_modifier(Modifier::UNDERLINED));
        assert_eq!(l[0].spans[1].style, Style::default());
        assert_eq!(l[0].spans[2].style, Style::default().add_modifier(Modifier::BOLD));
    }

    fn textos(line: &Line<'_>) -> Vec<String> {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    fn estilos(line: &Line<'_>) -> Vec<Style> {
        line.spans.iter().map(|s| s.style).collect()
    }

    #[test]
    fn colores_basicos() {
        let l = parse("\x1b[31mrojo\x1b[0m y \x1b[104mfondo");
        assert_eq!(textos(&l[0]), ["rojo", " y ", "fondo"]);
        assert_eq!(
            estilos(&l[0]),
            [
                Style::default().fg(Color::Indexed(1)),
                Style::default(),
                Style::default().bg(Color::Indexed(12))
            ]
        );
        let l = parse("\x1b[91mc\x1b[49m\x1b[39mf");
        assert_eq!(
            estilos(&l[0]),
            [Style::default().fg(Color::Indexed(9)), Style::default().fg(Color::Reset).bg(Color::Reset)]
        );
    }

    #[test]
    fn color_256_y_rgb() {
        let l = parse("\x1b[38;5;196mnaranja\x1b[48;5;17mfondo");
        assert_eq!(
            estilos(&l[0]),
            [Style::default().fg(Color::Indexed(196)), Style::default().fg(Color::Indexed(196)).bg(Color::Indexed(17))]
        );
        let a = parse("\x1b[38;2;10;20;30mx");
        let b = parse("\x1b[38:2::10:20:30mx");
        let c = parse("\x1b[38:2:10:20:30mx");
        assert_eq!(estilos(&a[0])[0].fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(estilos(&a[0]), estilos(&b[0]));
        assert_eq!(estilos(&a[0]), estilos(&c[0]));
        let bg = parse("\x1b[48:2::1:2:3mx");
        assert_eq!(estilos(&bg[0])[0].bg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn atributos_se_acumulan_y_se_quitan() {
        let l = parse("\x1b[1;4;7mx\x1b[22my\x1b[24;27mz");
        let e = estilos(&l[0]);
        assert_eq!(e[0].add_modifier, Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED);
        assert_eq!(e[1].add_modifier, Modifier::UNDERLINED | Modifier::REVERSED);
        assert_eq!(e[2].add_modifier, Modifier::empty());
        let l = parse("\x1b[3;9mq\x1b[23mr\x1b[29ms");
        assert_eq!(estilos(&l[0])[0].add_modifier, Modifier::ITALIC | Modifier::CROSSED_OUT);
        assert_eq!(estilos(&l[0])[1].add_modifier, Modifier::CROSSED_OUT);
        assert_eq!(estilos(&l[0])[2].add_modifier, Modifier::empty());
        // `ESC[m` sin parámetros equivale a 0.
        assert_eq!(estilos(&parse("\x1b[1ma\x1b[mb")[0])[1], Style::default());
        // Un parámetro que no conocemos no rompe los que vienen detrás.
        let l = parse("\x1b[99;1;53mx\x1b[0;44my");
        assert_eq!(estilos(&l[0])[0], Style::default().add_modifier(Modifier::BOLD));
        assert_eq!(estilos(&l[0])[1], Style::default().bg(Color::Indexed(4)));
    }

    #[test]
    fn estilo_cruza_lineas() {
        let l = parse("\x1b[1muno\ndos\n\x1b[0mtres");
        assert_eq!(l.len(), 3);
        assert_eq!(estilos(&l[1]), [Style::default().add_modifier(Modifier::BOLD)]);
        assert_eq!(estilos(&l[2]), [Style::default()]);
    }

    #[test]
    fn secuencias_descartadas() {
        let l = parse("\x1b]0;título\x07hola\x1b[2Ky\x1b[1;1Hadiós\x1b]8;;http://x\x1b\\fin");
        assert_eq!(textos(&l[0]), ["holayadiósfin"]);
        let l = parse("a\x1bMb\x1b");
        assert_eq!(textos(&l[0]), ["ab"]);
    }

    #[test]
    fn crlf_y_lineas_vacias() {
        let l = parse("uno\r\ndos\r");
        assert_eq!(l.len(), 2);
        assert_eq!(textos(&l[0]), ["uno"]);
        assert_eq!(textos(&l[1]), ["dos"]);
        assert_eq!(parse("a\n\nb")[1], Line::default());
        assert_eq!(parse(""), vec![Line::default()]);
    }

    #[test]
    fn tabuladores() {
        assert_eq!(textos(&parse("a\tb")[0]), ["a       b"]);
        assert_eq!(textos(&parse("12345678\tx")[0]), ["12345678        x"]);
        // El color no cambia el ancho; tras un salto se vuelve a la columna 0.
        assert_eq!(textos(&parse("\x1b[31ma\tb\n\tc")[0]), ["a       b"]);
        assert_eq!(textos(&parse("\x1b[31ma\tb\n\tc")[1]), ["        c"]);
    }

    #[test]
    fn fusion_de_spans() {
        let l = parse("a\x1b[1mb\x1b[1mc\x1b[22md");
        assert_eq!(textos(&l[0]), ["a", "bc", "d"]);
        // Controles y secuencias vacías no parten el tramo.
        assert_eq!(textos(&parse("a\x1b[2Kb\x07c")[0]), ["abc"]);
    }

    #[test]
    fn recorte() {
        let l = parse("日本語");
        assert_eq!(textos(&clip(&l[0], 6)), ["日本語"]);
        assert_eq!(textos(&clip(&l[0], 5)), ["日本"]);
        assert_eq!(textos(&clip(&l[0], 3)), ["日"]);
        assert_eq!(textos(&clip(&l[0], 1)), Vec::<String>::new());
        assert_eq!(l[0].width(), 6);
        let l = parse("a🦀b");
        assert_eq!(textos(&clip(&l[0], 2)), ["a"]);
        assert_eq!(textos(&clip(&l[0], 3)), ["a🦀"]);
        assert_eq!(l[0].width(), 4);
    }

    #[test]
    fn recorte_conserva_estilos() {
        let l = parse("\x1b[31mrojo\x1b[0mverde");
        let c = clip(&l[0], 6);
        assert_eq!(textos(&c), ["rojo", "ve"]);
        assert_eq!(c.spans[0].style, Style::default().fg(Color::Indexed(1)));
        let c = clip(&l[0], 4);
        assert_eq!(textos(&c), ["rojo"]);
    }
}
