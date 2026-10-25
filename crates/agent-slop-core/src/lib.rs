//! Comment extraction, rule evaluation, suppressions and configuration
//! for agent-slop.
//!
//! This crate is the deterministic engine behind the `agent-slop` binary.
//! It parses source with tree-sitter grammars (through `ast-grep`), pulls
//! out every comment and Python docstring, and matches them against a
//! reviewed corpus of sixteen rule families embedded from
//! `rules/*.toml`. Nothing in it spawns a process, reads the network or
//! consults a model; the binary and the integrations crate own those.
//!
//! # Modules
//!
//! - `lang` — the language table (Rust, Java, Python, Go, TypeScript,
//!   TSX, JavaScript, Bash, Nix, C, C++, YAML) and the comment walk.
//! - `rules` — the scan engine: lexical matchers, the structural
//!   paraphrase check, the TODO-condition interpreter and the
//!   `agent-slop: allow` suppression parser.
//! - `finding` — the `Finding` model and the ratchet identity: a
//!   path-independent 16-hex-character fingerprint.
//! - `config` — the three-tier `[slop]` configuration (built-ins, the
//!   user file, the repository's `.agent-hooks.toml`), resolved without
//!   ever failing, plus the `[slop.tune]` adjustments and the
//!   `[slop.plugins]` declarations.
//!
//! # Example
//!
//! ```
//! use agent_slop_core::{config, rules};
//!
//! let cfg = config::builtin_config();
//! let source = "# We previously always sent a Referer header.\nreferer = None\n";
//! let findings = rules::scan_source(&cfg, "client.py", source);
//!
//! assert_eq!(findings.len(), 1);
//! assert_eq!(findings[0].rule, "slop/comment-history");
//! ```

pub mod config;
pub mod finding;
pub mod lang;
pub mod rules;
