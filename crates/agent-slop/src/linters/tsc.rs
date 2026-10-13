//! tsc adapter (SPEC-006 "Linter adapters").
//!
//! `--pretty false` emits TEXT, one diagnostic per line:
//! `a.ts(1,7): error TS2322: <message>`. Parsed with a hand scan (first
//! `(` opens the position, the matching `)` closes it) because a message
//! may itself contain colons and parentheses; the file name is whatever
//! precedes the first `(`.
//!
//! `evaluates_repo_code`: `tsconfig.json` `extends` chains and plugins
//! from `node_modules` execute, so the adapter is scan-only and explicit,
//! never `auto`, never a hook.

use super::{Adapter, Cost, PathArity, RawFinding};
use crate::finding::Severity;

pub static ADAPTER: Adapter = Adapter {
    tool: "tsc",
    bin: "tsc",
    argv: &["--noEmit", "--pretty", "false"],
    cost: Cost::C,
    evaluates_repo_code: true,
    arity: PathArity::WholeProject,
    exts: &["ts", "tsx", "mts", "cts"],
    parse,
};

fn parse_line(line: &str) -> Option<RawFinding> {
    let open = line.find('(')?;
    let close = open + 1 + line[open + 1..].find(')')?;
    let file = &line[..open];
    let (l, c) = line[open + 1..close].split_once(',')?;
    let rest = line[close + 1..].strip_prefix(':')?.trim_start();
    let (level, tail) = rest.split_once(' ')?;
    let (code, message) = tail.split_once(": ")?;
    if code.is_empty() || file.is_empty() {
        return None;
    }
    Some(RawFinding {
        code: code.to_string(),
        file: file.to_string(),
        line: l.parse().ok()?,
        col: c.parse().ok()?,
        end_line: l.parse().ok()?,
        message: message.to_string(),
        upstream_severity: Some(level.to_string()),
        fixable: false,
        class: Severity::Warning,
    })
}

fn parse(stdout: &str) -> Vec<RawFinding> {
    stdout.lines().filter_map(parse_line).collect()
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
    fn text_diagnostics_carry_file_position_and_code() {
        let f = parse(&fixture("tsc.txt"));
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].code, "TS2322");
        assert_eq!(f[0].file, "a.ts");
        assert_eq!((f[0].line, f[0].col, f[0].end_line), (1, 7, 1));
        assert_eq!(
            f[0].message,
            "Type 'string' is not assignable to type 'number'."
        );
        assert_eq!(f[0].upstream_severity.as_deref(), Some("error"));
        assert_eq!(f[1].file, "a.ts");
        assert_eq!((f[1].line, f[1].col), (3, 3));
        assert_eq!(f[2].file, "b.ts");
        assert_eq!(f[2].class, Severity::Warning);
    }

    #[test]
    fn garbage_output_yields_nothing_rather_than_failing() {
        // tsc's trailing summary lines and blank lines match no shape.
        assert!(parse("Found 3 errors in 2 files.\n\n").is_empty());
        assert!(parse("no parentheses here").is_empty());
    }
}
