//! deadnix adapter (SPEC-006 "Linter adapters").
//!
//! Output is one JSON object per file, newline-separated, with no wrapping
//! array. deadnix reports no rule code at all, so the id is the fixed
//! `deadnix/dead-code`; the specific kind of dead binding stays in the
//! message. Paths come back as `./a.nix` from a directory walk and `a.nix`
//! from named files (both recorded in the fixtures), so the caller
//! normalizes.

use serde::Deserialize;

use super::{Adapter, Cost, PathArity, RawFinding};
use agent_slop_core::finding::Severity;

pub static ADAPTER: Adapter = Adapter {
    tool: "deadnix",
    bin: "deadnix",
    argv: &["--output-format", "json"],
    cost: Cost::A,
    evaluates_repo_code: false,
    arity: PathArity::Many,
    exts: &["nix"],
    parse,
};

/// The only id this tool can produce.
pub const CODE: &str = "dead-code";

#[derive(Deserialize)]
struct Entry {
    line: u32,
    column: u32,
    message: String,
}

fn parse(stdout: &str) -> Vec<RawFinding> {
    let mut out = Vec::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(file) = value.get("file").and_then(|f| f.as_str()) else {
            continue;
        };
        let Some(results) = value.get("results").and_then(|r| r.as_array()) else {
            continue;
        };
        for item in results {
            let Ok(r) = serde_json::from_value::<Entry>(item.clone()) else {
                continue;
            };
            out.push(RawFinding {
                code: CODE.to_string(),
                file: file.to_string(),
                line: r.line,
                col: r.column,
                end_line: r.line,
                message: r.message,
                upstream_severity: None,
                fixable: true,
                class: Severity::Warning,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linters::normalize_path;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        let path = format!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/linters/fixtures/{}"),
            name
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[test]
    fn one_object_per_file_is_read_as_a_stream() {
        let f = parse(&fixture("deadnix-two.json"));
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].file, "a.nix");
        assert_eq!(f[1].file, "b.nix");
        assert_eq!(f[0].code, "dead-code");
        assert_eq!(f[0].message, "Unused let binding: unused");
        assert_eq!((f[0].line, f[0].col), (2, 3));
    }

    #[test]
    fn a_directory_walk_prefixes_dot_slash_and_normalizes_away() {
        let f = parse(&fixture("deadnix-dir.json"));
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].file, "./b.nix");
        let root = Path::new("/home/u/repo");
        let normalized: Vec<String> = f.iter().map(|x| normalize_path(&x.file, root)).collect();
        assert_eq!(normalized, vec!["b.nix", "a.nix"]);
    }

    #[test]
    fn an_unparsable_line_is_skipped_without_losing_the_rest() {
        let mixed = format!("garbage\n{}", fixture("deadnix-two.json"));
        assert_eq!(parse(&mixed).len(), 2);
    }
}
