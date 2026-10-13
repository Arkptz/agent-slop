//! go vet adapter (SPEC-006 "Linter adapters").
//!
//! `-json` nests findings two levels — `{"<package>":{"<analyzer>":
//! [{"posn","end","message"}]}}` — and begins with a bare `{}` line before
//! the real object, so the input is read as a stream of values, not a
//! single document. `posn`/`end` are `file:line:col` STRINGS; they are
//! split from the right twice so a path containing a colon stays part of
//! the file. Exit is 0 even with findings, which the caller already
//! ignores.

use serde_json::Value;

use super::{Adapter, Cost, PathArity, RawFinding};
use crate::finding::Severity;

pub static ADAPTER: Adapter = Adapter {
    tool: "govet",
    bin: "go",
    argv: &["vet", "-json", "./..."],
    cost: Cost::C,
    evaluates_repo_code: false,
    arity: PathArity::WholeProject,
    exts: &["go"],
    parse,
};

/// `main.go:15:8` -> (`main.go`, 15, 8). `rsplit_once` yields
/// `(prefix, suffix)`, so each split binds the right-most component last.
fn split_posn(posn: &str) -> Option<(&str, u32, u32)> {
    let (head, col) = posn.rsplit_once(':')?;
    let (file, line) = head.rsplit_once(':')?;
    Some((file, line.parse().ok()?, col.parse().ok()?))
}

fn parse(stdout: &str) -> Vec<RawFinding> {
    let mut out = Vec::new();
    let stream = serde_json::Deserializer::from_str(stdout).into_iter::<Value>();
    for value in stream {
        let Ok(root) = value else { continue };
        let Some(packages) = root.as_object() else {
            continue;
        };
        for (_package, analyzers) in packages {
            let Some(analyzers) = analyzers.as_object() else {
                continue;
            };
            for (analyzer, findings) in analyzers {
                let Some(findings) = findings.as_array() else {
                    continue;
                };
                for f in findings {
                    let Some(posn) = f.get("posn").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some((file, line, col)) = split_posn(posn) else {
                        continue;
                    };
                    let end_line = f
                        .get("end")
                        .and_then(Value::as_str)
                        .and_then(split_posn)
                        .map_or(line, |(_, l, _)| l);
                    let message = f
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    out.push(RawFinding {
                        code: analyzer.clone(),
                        file: file.to_string(),
                        line,
                        col,
                        end_line,
                        message,
                        upstream_severity: None,
                        fixable: false,
                        class: Severity::Warning,
                    });
                }
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
    fn both_nesting_levels_and_the_leading_empty_object_are_handled() {
        let f = parse(&fixture("govet.json"));
        // Two copylocks findings and one printf finding; the bare `{}`
        // first value contributes nothing.
        assert_eq!(f.len(), 3);
        let rules: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(rules, vec!["copylocks", "copylocks", "printf"]);
        assert!(f.iter().all(|x| x.file == "main.go"));
    }

    #[test]
    fn posn_strings_split_into_file_line_col() {
        let f = parse(&fixture("govet.json"));
        assert_eq!((f[0].line, f[0].col, f[0].end_line), (15, 8, 15));
        assert_eq!(
            f[0].message,
            "call of other copies lock value: slopvet.T contains sync.Mutex"
        );
        assert_eq!((f[1].line, f[1].col), (18, 14));
        assert_eq!((f[2].line, f[2].col, f[2].end_line), (12, 2, 12));
        assert!(!f[0].fixable);
        assert_eq!(f[0].class, Severity::Warning);
    }

    #[test]
    fn garbage_output_yields_nothing_rather_than_failing() {
        assert!(parse("not json at all").is_empty());
        assert!(parse("{}\nnot json\n").is_empty());
    }
}
