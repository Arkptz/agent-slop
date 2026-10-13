// Rule fixtures: harvested positives and real negatives per family
// (SPEC-006 acceptance criteria; POL-026 rule 3 — the oracle is the
// fixture corpus, recorded with `path:line` provenance, never derived
// from the matcher being validated).
//
// `include!`d from tests/rule_fixtures_main.rs; `common` (with the
// engine modules at `crate::*`) is in scope there.

use common::scan;
use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    #[serde(default)]
    family: String,
    expect: String,
    lang: String,
    #[serde(default)]
    provenance: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct FixtureFile {
    #[serde(rename = "case")]
    cases: Vec<Case>,
}

fn load_all() -> BTreeMap<String, Vec<Case>> {
    let dir = Path::new("tests/rules/fixtures");
    let mut by_family: BTreeMap<String, Vec<Case>> = BTreeMap::new();
    let mut sorted: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("fixtures dir missing: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    sorted.sort();
    for path in sorted {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let file: FixtureFile = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        for case in file.cases {
            let family = if case.family.is_empty() {
                path.file_stem().unwrap().to_string_lossy().to_string()
            } else {
                case.family.clone()
            };
            by_family.entry(family).or_default().push(case);
        }
    }
    by_family
}

/// Ties favour the family naming the defect over the generic overlap
/// verdict: a TODO naming a DDL step, step narration, a permission
/// argument and deploy narration all describe a specific defect whose
/// incidental identifier overlap must not relabel the finding.
fn first_family_hit(lang: &str, source: &str) -> Option<String> {
    let mut families = scan(lang, source);
    families.sort_by_key(|f| {
        (
            f != "manual-ddl",
            f != "todo-no-condition",
            f != "comment-steps",
            f != "self-justification",
            f != "config-deploy-narration",
        )
    });
    families.into_iter().next()
}

#[test]
fn harvested_fixtures_behave_as_expected() {
    let all = load_all();
    assert!(!all.is_empty(), "no fixtures found");
    for (family, cases) in &all {
        for case in cases {
            let hit = first_family_hit(&case.lang, &case.source);
            match case.expect.as_str() {
                "hit" => {
                    assert_eq!(
                        hit.as_deref(),
                        Some(family.as_str()),
                        "family {family} should fire on {:?} (provenance {})",
                        case.source,
                        case.provenance
                    );
                }
                "miss" => {
                    assert_ne!(
                        hit.as_deref(),
                        Some(family.as_str()),
                        "family {family} must NOT fire on {:?} (provenance {})",
                        case.source,
                        case.provenance
                    );
                }
                other => panic!("unknown expect {other:?}"),
            }
        }
    }
}

/// SPEC-006: an `error` fixture is valid only with recorded provenance —
/// a positive without a source is an author-written oracle in disguise.
#[test]
fn error_rule_positives_carry_provenance() {
    let all = load_all();
    for family in ["comment-history", "comment-meta", "review-jargon"] {
        for case in &all[family] {
            if case.expect == "hit" {
                assert!(
                    case.provenance.len() > 10,
                    "error-rule positive for {family} lacks provenance: {:?}",
                    case.source
                );
            }
        }
    }
}

/// Fails when any rule lacks a positive AND a negative fixture.
#[test]
fn every_rule_has_fixtures() {
    let all = load_all();
    for family in [
        "comment-history",
        "comment-meta",
        "review-jargon",
        "comment-paraphrase",
        "todo-no-condition",
        "perf-claim",
        "unsourced-constraint",
        "manual-ddl",
        "bare-reference",
        "comment-hedging",
        "comment-steps",
        "docstring-restates-name",
        "self-justification",
        "config-deploy-narration",
    ] {
        let cases = all
            .get(family)
            .unwrap_or_else(|| panic!("family {family} has NO fixtures at all"));
        let has_positive = cases.iter().any(|c| c.expect == "hit");
        let has_negative = cases.iter().any(|c| c.expect == "miss");
        assert!(has_positive, "family {family} lacks a positive fixture");
        assert!(has_negative, "family {family} lacks a negative fixture");
    }
}
