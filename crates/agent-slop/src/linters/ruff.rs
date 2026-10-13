//! ruff adapter (SPEC-006 "Linter adapters").
//!
//! `--select` on the command line REPLACES the project's selection rather
//! than adding to it, so no selection flag is passed and `select`/`ignore`
//! filter after parsing (`super::selected`). `--force-exclude` makes ruff
//! honour its own excludes for explicitly named paths, which is what
//! "always use the project's config" requires.

use serde::Deserialize;

use super::{Adapter, Cost, PathArity, RawFinding};
use crate::finding::Severity;

pub static ADAPTER: Adapter = Adapter {
    tool: "ruff",
    bin: "ruff",
    argv: &["check", "--output-format", "json", "--force-exclude"],
    cost: Cost::A,
    evaluates_repo_code: false,
    arity: PathArity::Many,
    exts: &["py", "pyi"],
    parse,
};

#[derive(Deserialize)]
struct Pos {
    row: u32,
    column: u32,
}

#[derive(Deserialize)]
struct Item {
    code: Option<String>,
    filename: String,
    location: Pos,
    end_location: Pos,
    message: String,
    severity: Option<String>,
    fix: Option<serde_json::Value>,
}

/// Codes ruff itself documents as formatting-only. They become `hint`, the
/// taste level, so they are reported but never counted with substantive
/// findings.
fn is_stylistic(code: &str) -> bool {
    matches!(
        code.as_bytes().first(),
        Some(b'E') | Some(b'W') | Some(b'D') | Some(b'Q')
    )
}

/// A clean run prints `[]`; unparsable output yields nothing, which the
/// caller records as `skipped`.
fn parse(stdout: &str) -> Vec<RawFinding> {
    let Ok(items) = serde_json::from_str::<Vec<Item>>(stdout.trim()) else {
        return Vec::new();
    };
    items
        .into_iter()
        .map(|i| {
            // A syntax error carries no code; it is still a real finding.
            let code = i.code.unwrap_or_else(|| "syntax-error".to_string());
            let class = if is_stylistic(&code) {
                Severity::Hint
            } else {
                Severity::Warning
            };
            RawFinding {
                code,
                file: i.filename,
                line: i.location.row,
                col: i.location.column,
                end_line: i.end_location.row,
                message: i.message,
                upstream_severity: i.severity,
                fixable: i.fix.is_some(),
                class,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = format!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/linters/fixtures/{}"),
            name
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[test]
    fn clean_output_is_an_empty_array() {
        assert!(parse(&fixture("ruff-clean.json")).is_empty());
    }

    #[test]
    fn f401_carries_position_fixability_and_upstream_severity() {
        let f = parse(&fixture("ruff-f401.json"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "F401");
        assert_eq!(f[0].file, "/tmp/slopfix/ruff/a.py");
        assert_eq!((f[0].line, f[0].col, f[0].end_line), (1, 8, 1));
        assert_eq!(f[0].message, "`os` imported but unused");
        assert_eq!(f[0].upstream_severity.as_deref(), Some("error"));
        assert!(f[0].fixable, "the fixture carries a non-null fix");
        assert_eq!(f[0].class, Severity::Warning);
    }

    #[test]
    fn a_style_code_is_unfixable_here_and_classed_hint() {
        let f = parse(&fixture("ruff-e501.json"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "E501");
        assert_eq!(f[0].class, Severity::Hint);
        assert!(!f[0].fixable, "the fixture's fix is null");
    }

    #[test]
    fn garbage_output_yields_nothing_rather_than_failing() {
        assert!(parse("not json at all").is_empty());
    }
}
