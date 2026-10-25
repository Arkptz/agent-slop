//! Wire types of the `agent-slop-plugin/1` protocol.
//!
//! One process per call, no daemon. A plugin answers two subcommands:
//!
//! 1. `<command> describe` prints a [`Manifest`] on stdout.
//! 2. `<command> check` reads a [`Request`] on stdin and prints a
//!    [`Response`] on stdout. Exit status 0 means the response is valid;
//!    anything else is a failure the host reports as a skip.
//!
//! The host sends [`PROTOCOL_VERSION`] and accepts only responses and
//! manifests that carry the same number. Unknown fields are ignored on
//! read, so a field added in a minor release does not break an older peer;
//! a breaking change bumps the protocol number.
//!
//! ```
//! use agent_slop_protocol::{Manifest, PROTOCOL_VERSION, is_supported};
//!
//! let json = r#"{"protocol":1,"name":"acme","version":"0.1.0","input":"comments",
//!                "cost":"A","deterministic":true,"rules":[]}"#;
//! let manifest: Manifest = serde_json::from_str(json).unwrap();
//! assert!(is_supported(manifest.protocol));
//! assert_eq!(manifest.protocol, PROTOCOL_VERSION);
//! ```

use serde::{Deserialize, Serialize};

/// The protocol number this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Whether a peer announcing `protocol` can be understood.
#[must_use]
pub fn is_supported(protocol: u32) -> bool {
    protocol == PROTOCOL_VERSION
}

/// What a plugin consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Input {
    /// Extracted comments and docstrings, with positions.
    Comments,
    /// File paths only; the plugin reads the files itself.
    Files,
}

/// Run-cost class; the host decides from it which stages may call the
/// plugin (A is cheapest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Cost {
    A,
    B,
    C,
    D,
}

/// A rule level as the plugin declares it. The host caps what it imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Hint,
    Off,
}

/// The host stage a [`Request`] is issued from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// `agent-slop scan`.
    Scan,
    /// `agent-slop check`.
    Check,
    /// The `PostToolUse` hook; advisory, with a short deadline.
    Hook,
}

/// One rule a plugin can report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDecl {
    /// Id within the plugin; the host reports it as `<plugin>/<id>`.
    pub id: String,
    pub default_level: Level,
    /// What to do instead; shown by `explain`.
    #[serde(default)]
    pub note: String,
    /// When a hit is legitimate.
    #[serde(default)]
    pub exception: String,
}

/// The answer to `describe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol: u32,
    pub name: String,
    pub version: String,
    pub input: Input,
    pub cost: Cost,
    /// Languages for `input = "comments"`; empty means all.
    #[serde(default)]
    pub langs: Vec<String>,
    /// Extensions for `input = "files"`.
    #[serde(default)]
    pub exts: Vec<String>,
    /// Same input always yields the same findings. Required for any use
    /// that compares two runs.
    #[serde(default)]
    pub deterministic: bool,
    #[serde(default)]
    pub rules: Vec<RuleDecl>,
}

/// One extracted comment, positions 1-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireComment {
    /// Position in this file's list; findings refer back to it.
    pub index: usize,
    pub text: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    #[serde(default)]
    pub next_code_ids: Vec<String>,
    #[serde(default)]
    pub is_docstring: bool,
}

/// One file of a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestFile {
    /// Repo-relative, `/`-separated.
    pub path: String,
    pub lang: String,
    /// Present only for `input = "comments"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<WireComment>,
}

/// The stdin of `check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub stage: Stage,
    pub repo_root: String,
    /// Wall-clock budget the host will honour before killing the process.
    pub deadline_ms: u64,
    /// The opaque `settings` table from the user's config.
    #[serde(default)]
    pub settings: serde_json::Value,
    pub files: Vec<RequestFile>,
}

/// One finding on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingWire {
    /// Id within the plugin, declared in the manifest.
    pub rule: String,
    /// One of the request's paths.
    pub path: String,
    /// `input = "comments"`: index into that file's comments; position and
    /// snippet then come from the host's own comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<usize>,
    /// `input = "files"`: position of the hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_severity: Option<String>,
    #[serde(default)]
    pub fixable: bool,
}

/// The stdout of `check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    #[serde(default)]
    pub findings: Vec<FindingWire>,
    /// Free text, printed under `--format human`.
    #[serde(default)]
    pub diagnostics: Vec<String>,
}
