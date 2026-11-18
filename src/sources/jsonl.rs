//! Lector incremental de ficheros JSONL que crecen (transcripts de agentes).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

/// Quien consume las líneas.
pub trait Sink {
    /// El fichero se ha truncado o sustituido: hay que olvidar todo lo acumulado.
    fn reset(&mut self);
    /// Una línea completa nueva, sin el salto final. Nunca vacía.
    fn line(&mut self, line: &[u8]);
}

/// Cursor sobre un fichero JSONL. Cada `poll` entrega solo las líneas completas añadidas desde
/// el anterior; una línea a medio escribir se deja para la siguiente llamada.
pub struct Tail {
    path: PathBuf,
    offset: u64,
}

impl Tail {
    pub fn new(path: impl Into<PathBuf>) -> Tail {
        Tail { path: path.into(), offset: 0 }
    }

    /// Entrega lo nuevo a `sink` y devuelve cuántas líneas ha entregado. Si no hay cambios cuesta
    /// un `stat`. Fichero inexistente: 0 líneas, sin error (puede no haberse creado todavía).
    pub fn poll(&mut self, sink: &mut impl Sink) -> io::Result<usize> {
        let len = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        if len < self.offset {
            self.offset = 0;
            sink.reset();
        }
        if len == self.offset {
            return Ok(0);
        }
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut r = BufReader::with_capacity(1 << 16, f);
        let mut buf = Vec::with_capacity(4096);
        let mut n = 0;
        loop {
            buf.clear();
            let read = r.read_until(b'\n', &mut buf)?;
            if read == 0 || buf.last() != Some(&b'\n') {
                break; // fin, o línea a medias: se reintenta en el próximo poll
            }
            self.offset += read as u64;
            let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !line.iter().all(u8::is_ascii_whitespace) {
                sink.line(line);
                n += 1;
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[derive(Default)]
    struct Rec {
        lines: Vec<String>,
        resets: usize,
    }
    impl Sink for Rec {
        fn reset(&mut self) {
            self.resets += 1;
            self.lines.clear();
        }
        fn line(&mut self, l: &[u8]) {
            self.lines.push(String::from_utf8_lossy(l).into_owned());
        }
    }

    #[test]
    fn incremental_parcial_y_truncado() {
        let p = std::env::temp_dir().join(format!("agentview-tail-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut t = Tail::new(&p);
        let mut r = Rec::default();
        assert_eq!(t.poll(&mut r).unwrap(), 0);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).unwrap();
        write!(f, "{{\"a\":1}}\n\n{{\"b\":").unwrap();
        assert_eq!(t.poll(&mut r).unwrap(), 1);
        assert_eq!(t.poll(&mut r).unwrap(), 0);
        writeln!(f, "2}}").unwrap();
        assert_eq!(t.poll(&mut r).unwrap(), 1);
        assert_eq!(r.lines, ["{\"a\":1}", "{\"b\":2}"]);
        std::fs::write(&p, "{\"c\":3}\n").unwrap();
        assert_eq!(t.poll(&mut r).unwrap(), 1);
        assert_eq!((r.resets, r.lines.as_slice()), (1, &["{\"c\":3}".to_string()][..]));
        let _ = std::fs::remove_file(&p);
    }
}
