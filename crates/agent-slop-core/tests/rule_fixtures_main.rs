//! The rule-fixture test target, driven through the crate's public API.
//! The test bodies are `include!`d from tests/rules/rule_fixtures.rs.

mod common {
    use agent_slop_core::{config, rules};

    /// Which rule families fire on `source` under language `lang_name`,
    /// unsuppressed findings only.
    pub fn scan(lang_name: &str, source: &str) -> Vec<String> {
        let cfg = config::builtin_config();
        rules::scan_source(&cfg, &format!("fixture.{}", ext_for(lang_name)), source)
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
            other => panic!("unknown fixture language {other}"),
        }
    }
}

include!("rules/rule_fixtures.rs");
