//! The rule-fixture test target. The binary's engine modules are pulled
//! in as `crate::{config,finding,lang,rules}` via `#[path]` so their
//! internal `crate::` imports resolve HERE, against this test crate's
//! root — one compile of the real code, no duplication. The test bodies
//! are `include!`d from tests/rules/rule_fixtures.rs.

#[path = "../src/config.rs"]
pub mod config;
#[path = "../src/finding.rs"]
pub mod finding;
#[path = "../src/lang.rs"]
pub mod lang;
#[path = "../src/rules/mod.rs"]
pub mod rules;

mod common {
    pub use crate::{config, lang, rules};

    /// Which rule families fire on `source` under language `lang_name`,
    /// unsuppressed findings only.
    pub fn scan(lang_name: &str, source: &str) -> Vec<String> {
        let spec = lang::LANGS
            .iter()
            .find(|s| s.name == lang_name)
            .unwrap_or_else(|| panic!("unknown language {lang_name}"));
        let cfg = config::builtin_config();
        rules::scan_source(&cfg, &format!("fixture.{}", ext_for(spec.name)), source)
            .into_iter()
            .filter(|f| f.suppression.is_none())
            .map(|f| f.rule.trim_start_matches("slop/").to_string())
            .collect()
    }

    fn ext_for(lang: &str) -> &'static str {
        match lang {
            "python" => "py",
            "nix" => "nix",
            "rust" => "rs",
            "typescript" => "ts",
            "yaml" => "yaml",
            _ => "txt",
        }
    }
}

include!("rules/rule_fixtures.rs");
