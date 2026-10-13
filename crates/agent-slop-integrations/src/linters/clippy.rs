//! clippy adapter (SPEC-006 "Linter adapters").
//!
//! `--message-format json` is one JSON object per line; only
//! `"reason":"compiler-message"` lines carry diagnostics, and everything
//! else (`compiler-artifact`, `build-finished`) is noise. The lint id sits
//! at `message.code.code` with a `clippy::` prefix that is stripped so the
//! rule id reads `clippy/needless_return` after the framework joins it.
//!
//! In the recorded output the primary span itself carries
//! `suggested_replacement: null` and the fix lives in the help `children`
//! spans, so fixability is any non-null replacement on the message or its
//! children (an empty string is a deletion and still counts).
//!
//! Cost D and `evaluates_repo_code`: `build.rs` and proc macros execute,
//! so the adapter never runs in a hook and is never selected by `auto`.

use serde::Deserialize;

use super::{Adapter, Cost, PathArity, RawFinding};
use agent_slop_core::finding::Severity;

pub static ADAPTER: Adapter = Adapter {
    tool: "clippy",
    bin: "cargo",
    argv: &["clippy", "--message-format", "json", "--all-targets"],
    cost: Cost::D,
    evaluates_repo_code: true,
    arity: PathArity::WholeProject,
    exts: &["rs"],
    parse,
};

#[derive(Deserialize)]
struct Span {
    file_name: String,
    is_primary: bool,
    line_start: u32,
    column_start: u32,
    line_end: u32,
    suggested_replacement: Option<String>,
}

#[derive(Deserialize)]
struct Diagnostic {
    message: String,
    code: Option<Code>,
    level: Option<String>,
    spans: Vec<Span>,
    #[serde(default)]
    children: Vec<Child>,
}

#[derive(Deserialize)]
struct Child {
    #[serde(default)]
    spans: Vec<Span>,
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

#[derive(Deserialize)]
struct Line {
    reason: String,
    message: Diagnostic,
}

fn fixable(d: &Diagnostic) -> bool {
    let has_fix = |s: &Span| s.suggested_replacement.is_some();
    d.spans.iter().any(has_fix) || d.children.iter().flat_map(|c| c.spans.iter()).any(has_fix)
}

fn parse(stdout: &str) -> Vec<RawFinding> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Ok(l) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        if l.reason != "compiler-message" {
            continue;
        }
        let d = &l.message;
        let Some(code) = &d.code else { continue };
        let Some(span) = d.spans.iter().find(|s| s.is_primary) else {
            continue;
        };
        out.push(RawFinding {
            // `clippy::needless_return` -> `needless_return`; rustc's own
            // ids (`unused_variables`) pass through unchanged.
            code: code.code.trim_start_matches("clippy::").to_string(),
            file: span.file_name.clone(),
            line: span.line_start,
            col: span.column_start,
            end_line: span.line_end,
            message: d.message.clone(),
            upstream_severity: d.level.clone(),
            fixable: fixable(d),
            class: Severity::Warning,
        });
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
    fn only_compiler_message_lines_become_findings() {
        let f = parse(&fixture("clippy.json"));
        // Two lints, each reported once per cargo phase in the fixture;
        // artifact and build-finished lines yield nothing.
        assert_eq!(f.len(), 4);
        assert!(f.iter().all(|x| x.file == "src/main.rs"));
    }

    #[test]
    fn the_clippy_prefix_is_stripped_and_the_position_recorded() {
        let f = parse(&fixture("clippy.json"));
        let ret = f.iter().find(|x| x.message.contains("return")).unwrap();
        assert_eq!(ret.code, "needless_return");
        assert_eq!((ret.line, ret.col, ret.end_line), (15, 5, 15));
        assert_eq!(ret.message, "unneeded `return` statement");
        assert_eq!(ret.upstream_severity.as_deref(), Some("warning"));
        assert_eq!(ret.class, Severity::Warning);
    }

    #[test]
    fn a_replacement_in_a_help_child_marks_the_lint_fixable() {
        let f = parse(&fixture("clippy.json"));
        let ret = f.iter().find(|x| x.code == "needless_return").unwrap();
        assert!(
            ret.fixable,
            "the fixture's primary span is null but a child carries \"b\""
        );
        let vec = f.iter().find(|x| x.code == "useless_vec").unwrap();
        assert_eq!((vec.line, vec.col), (2, 13));
        assert!(vec.fixable, "a child span carries \"[1, 2, 3]\"");
    }

    #[test]
    fn garbage_output_yields_nothing_rather_than_failing() {
        assert!(parse("not json at all").is_empty());
    }
}
