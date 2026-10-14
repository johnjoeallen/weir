//! Loads the entry file and the local components it imports with `use`.

use crate::ast::File;
use crate::diag::Diag;
use crate::parser::parse_file;
use std::path::{Path, PathBuf};

pub struct Import {
    /// Index into `Program::files`.
    pub file: usize,
    /// `#name` selects one flow; `None` imports every non-private flow.
    pub name: Option<String>,
    pub spec: String,
    pub line: u32,
    pub col: u32,
}

pub struct Loaded {
    pub file: File,
    pub src: String,
    pub imports: Vec<Import>,
}

/// `files[0]` is the entry file.
#[derive(Default)]
pub struct Program {
    pub files: Vec<Loaded>,
    /// Problems found while loading (parse errors, missing or unsupported imports).
    pub diags: Vec<Diag>,
    /// Imports that could not be loaded: (importing file, selected flow name).
    /// The checker uses these to avoid a cascade of "unknown name" errors.
    pub failed_imports: Vec<(usize, Option<String>)>,
}

pub fn load(entry: &str) -> Program {
    let mut prog = Program::default();
    let mut stack: Vec<PathBuf> = Vec::new();
    load_file(&mut prog, Path::new(entry), entry, &mut stack);
    prog
}

fn load_file(prog: &mut Program, path: &Path, shown: &str, stack: &mut Vec<PathBuf>) -> Option<usize> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            prog.diags.push(Diag::error(shown, 1, 1, "E-READ", format!("cannot read {shown}: {e}"), None));
            return None;
        }
    };
    let file = match parse_file(shown, &src) {
        Ok(f) => f,
        Err(d) => {
            prog.diags.push(d);
            return None;
        }
    };
    let idx = prog.files.len();
    let uses = file.uses.clone();
    prog.files.push(Loaded { file, src, imports: vec![] });
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    stack.push(canon);
    for u in uses {
        let (spec, name) = match u.spec.split_once('#') {
            Some((s, n)) => (s.to_string(), Some(n.to_string())),
            None => (u.spec.clone(), None),
        };
        if spec.starts_with("github:") || spec.starts_with("http://") || spec.starts_with("https://") {
            prog.diags.push(Diag::error(
                shown,
                u.span.line,
                u.span.col,
                "E-USE-REMOTE-UNSUPPORTED",
                format!("`{}` is a remote component, and this build of Weir can only import local files", u.spec),
                Some("vendor the component into a local folder and import it with a relative path: use \"./lib/name.weir\"".into()),
            ));
            prog.failed_imports.push((idx, name));
            continue;
        }
        let base = path.parent().unwrap_or(Path::new("."));
        let target = base.join(spec.split('@').next().unwrap_or(&spec));
        let canon_target = target.canonicalize().unwrap_or_else(|_| target.clone());
        if stack.contains(&canon_target) {
            prog.diags.push(Diag::error(
                shown,
                u.span.line,
                u.span.col,
                "E-USE-CYCLE",
                format!("`{}` imports itself, directly or through another component", u.spec),
                Some("remove the import cycle".into()),
            ));
            prog.failed_imports.push((idx, name));
            continue;
        }
        // The same file imported twice is loaded once.
        let existing = prog.files.iter().position(|l| {
            Path::new(&l.file.path).canonicalize().map(|c| c == canon_target).unwrap_or(false)
        });
        let fidx = match existing {
            Some(i) => Some(i),
            None => load_file(prog, &target, &target.to_string_lossy(), stack),
        };
        match fidx {
            Some(f) => prog.files[idx].imports.push(Import { file: f, name, spec: u.spec.clone(), line: u.span.line, col: u.span.col }),
            None => prog.failed_imports.push((idx, name)),
        }
    }
    stack.pop();
    Some(idx)
}
