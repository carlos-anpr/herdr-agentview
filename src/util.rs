//! Utilidades comunes a las fuentes y a la UI: tiempo, rutas y recortes de texto.

use std::path::Path;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// ISO 8601 → segundos Unix. Admite fracción de segundo, `Z` y desfases `+hh:mm` / `-hhmm`.
/// Sin zona se interpreta como UTC.
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { std::str::from_utf8(b.get(r)?).ok()?.parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let off = match b.get(i) {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let rest: Vec<u8> = b[i + 1..].iter().copied().filter(u8::is_ascii_digit).collect();
            if rest.len() < 2 {
                return None;
            }
            let p = |r: &[u8]| -> i64 { std::str::from_utf8(r).ok().and_then(|x| x.parse().ok()).unwrap_or(0) };
            let secs = p(&rest[0..2]) * 3600 + if rest.len() >= 4 { p(&rest[2..4]) * 60 } else { 0 };
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec - off)
}

/// Milisegundos Unix → segundos Unix.
pub fn ms_to_s(ms: i64) -> i64 {
    ms.div_euclid(1000)
}

/// Una sola línea: junta todos los espacios y saltos en uno, recorta extremos y limita a `max`
/// caracteres (con `…` si corta).
pub fn one_line(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut cut = 0; // longitud en bytes de los primeros `max - 1` caracteres
    for (i, w) in s.split_whitespace().enumerate() {
        for c in (i > 0).then_some(' ').into_iter().chain(w.chars()) {
            if n + 1 == max {
                cut = out.len();
            }
            if n == max {
                out.truncate(cut);
                out.push('…');
                return out;
            }
            out.push(c);
            n += 1;
        }
    }
    out
}

/// Primera línea con contenido de `s`, pasada por `one_line`.
pub fn first_line(s: &str, max: usize) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    one_line(l, max)
}

/// Limita a `max` caracteres conservando los saltos de línea (con `…` si corta).
pub fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Abrevia una ruta para enseñarla: relativa a `cwd` si cuelga de él, `~/…` si cuelga del HOME
/// y tal cual en otro caso. No toca el disco.
pub fn rel_path(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    if let Ok(r) = p.strip_prefix(cwd) {
        let r = r.to_string_lossy();
        return if r.is_empty() { ".".into() } else { r.into_owned() };
    }
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(r) = p.strip_prefix(Path::new(&home))
    {
        let r = r.to_string_lossy();
        return if r.is_empty() { "~".into() } else { format!("~/{r}") };
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-11-03T16:12:24.971Z"), Some(1793722344));
        assert_eq!(parse_iso("2026-11-03T17:12:24+01:00"), Some(1793722344));
        assert_eq!(parse_iso("2026-11-03T11:12:24-0500"), Some(1793722344));
        assert_eq!(parse_iso("2026-11-03 16:12:24"), Some(1793722344));
        assert_eq!(parse_iso("ayer"), None);
        assert_eq!(parse_iso("2026-13-03T16:12:24Z"), None);
    }

    #[test]
    fn lineas() {
        assert_eq!(one_line("  hola \n  mundo\t cruel ", 80), "hola mundo cruel");
        assert_eq!(one_line("abcdefghij", 5), "abcd…");
        assert_eq!(one_line("abcde", 5), "abcde");
        assert_eq!(one_line("ab cde", 5), "ab c…");
        assert_eq!(first_line("\n\n  uno dos \ntres", 80), "uno dos");
        assert_eq!(clip("uno\ndos", 80), "uno\ndos");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn rutas() {
        let cwd = Path::new("/w/repo");
        assert_eq!(rel_path("/w/repo/src/a.rs", cwd), "src/a.rs");
        assert_eq!(rel_path("/w/repo", cwd), ".");
        assert_eq!(rel_path("/etc/hosts", cwd), "/etc/hosts");
        assert_eq!(rel_path("src/a.rs", cwd), "src/a.rs");
    }
}
