//! statix adapter (SPEC-006 "Linter adapters").
//!
//! `-o json` prints one pretty-printed object per file, concatenated with
//! no wrapping array and not one object per line, so the input is read as
//! a stream of values. `code` is an integer; the printable id zero-pads
//! into statix's own vocabulary (`statix list` → `W03 manual_inherit`).
//! `severity` is `"Warn"`. Exit is 1 on findings and 0 when clean, which
//! the caller already ignores. `check` takes at most one TARGET (two
//! paths is a clap error), so batching is one run per file or directory.

use serde::Deserialize;

use super::{Adapter, Cost, PathArity, RawFinding};
use agent_slop_core::finding::Severity;

pub static ADAPTER: Adapter = Adapter {
    tool: "statix",
    bin: "statix",
    argv: &["check", "-o", "json"],
    cost: Cost::A,
    evaluates_repo_code: false,
    arity: PathArity::OnePerRun,
    exts: &["nix"],
    parse,
};

#[derive(Deserialize)]
struct Pos {
    line: u32,
    column: u32,
}

#[derive(Deserialize)]
struct At {
    from: Pos,
    to: Pos,
}

#[derive(Deserialize)]
struct Diagnostic {
    at: At,
    message: String,
    suggestion: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Report {
    code: i64,
    severity: String,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Deserialize)]
struct FileReport {
    file: String,
    report: Vec<Report>,
}

/// `4` → `W04`, matching statix's own ids. Two digits cover every id in
/// `statix list`.
fn rule_id(code: i64) -> String {
    format!("W{code:02}")
}

fn parse(stdout: &str) -> Vec<RawFinding> {
    let mut out = Vec::new();
    let stream = serde_json::Deserializer::from_str(stdout).into_iter::<FileReport>();
    for value in stream {
        let Ok(f) = value else { continue };
        for r in f.report {
            let code = rule_id(r.code);
            for d in r.diagnostics {
                out.push(RawFinding {
                    file: f.file.clone(),
                    line: d.at.from.line,
                    col: d.at.from.column,
                    end_line: d.at.to.line,
                    message: d.message,
                    upstream_severity: Some(r.severity.clone()),
                    fixable: d.suggestion.is_some(),
                    code: code.clone(),
                    class: Severity::Warning,
                });
            }
        }
    }
    out
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
    fn a_single_pretty_printed_object_yields_one_finding() {
        let f = parse(&fixture("statix-single.json"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "W04");
        assert_eq!(f[0].file, "one.nix");
        assert_eq!((f[0].line, f[0].col, f[0].end_line), (1, 3, 1));
        assert_eq!(
            f[0].message,
            "This assignment is better written with `inherit`"
        );
        assert_eq!(f[0].upstream_severity.as_deref(), Some("Warn"));
        assert!(f[0].fixable, "the fixture carries a suggestion with a fix");
        assert_eq!(f[0].class, Severity::Warning);
    }

    #[test]
    fn concatenated_objects_across_files_and_reports() {
        let f = parse(&fixture("statix-concat.json"));
        // ../sub/p.nix carries two diagnostics in one report.
        assert_eq!(f.len(), 4);
        assert_eq!(f[0].file, "../sub/q.nix");
        assert_eq!((f[0].line, f[0].col), (1, 11));
        assert_eq!(f[1].file, "../one.nix");
        assert_eq!(f[2].file, "../sub/p.nix");
        assert_eq!(f[3].file, "../sub/p.nix");
        assert_eq!(f[3].col, 12);
        assert!(f.iter().all(|x| x.code == "W04"));
    }

    #[test]
    fn code_zero_padding_matches_statix_own_vocabulary() {
        // statix-list.txt is `statix list` output; every listed id must be
        // recoverable from its integer by rule_id.
        let list = fixture("statix-list.txt");
        let mut count = 0;
        for line in list.lines() {
            let (id, _name) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("line: {line}"));
            let Some(digits) = id.strip_prefix('W') else {
                panic!("id: {id}")
            };
            let code: i64 = digits.parse().unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(rule_id(code), id);
            count += 1;
        }
        assert!(count >= 15);
    }

    #[test]
    fn a_clean_run_prints_nothing() {
        assert!(parse(&fixture("statix-clean.json")).is_empty());
    }

    #[test]
    fn garbage_output_yields_nothing_rather_than_failing() {
        assert!(parse("not json at all").is_empty());
    }
}
