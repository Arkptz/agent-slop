//! Language table and the comment walk (`rules/README.md`, "Comment node
//! kinds"; SPEC-006 "Ratchet" for why ranges matter).
//!
//! The kind table is a Rust constant because it must agree with the set of
//! compiled grammars — a mismatch has to be a compile-time-visible defect,
//! not a rule that silently matches nothing. The walk matches only the
//! OUTERMOST node spanning a whole comment and never descends into a
//! matched comment: Rust's `doc_comment` is a CHILD of
//! `line_comment`/`block_comment` with a different range and marker-stripped
//! text, so matching all three kinds emits two findings per doc comment,
//! two fingerprints, and a ratchet that double-counts.

use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_core::{Language, Node};
use ast_grep_language::{LanguageExt, SupportLang};

/// One language: its canonical name, the grammar that parses it, and the
/// outermost comment node kinds of that grammar (measured per grammar, see
/// `rules/README.md`).
pub struct LangSpec {
    pub name: &'static str,
    pub lang: SupportLang,
    pub comment_kinds: &'static [&'static str],
}

use SupportLang::{Bash, Cpp, Go, Java, JavaScript, Nix, Python, Rust, Tsx, TypeScript, Yaml, C};

/// rust and java have the two-kind split; every other v1 language has a
/// single `comment` kind.
pub static LANGS: &[LangSpec] = &[
    LangSpec {
        name: "rust",
        lang: Rust,
        comment_kinds: &["line_comment", "block_comment"],
    },
    LangSpec {
        name: "java",
        lang: Java,
        comment_kinds: &["line_comment", "block_comment"],
    },
    LangSpec {
        name: "python",
        lang: Python,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "go",
        lang: Go,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "typescript",
        lang: TypeScript,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "tsx",
        lang: Tsx,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "javascript",
        lang: JavaScript,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "bash",
        lang: Bash,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "nix",
        lang: Nix,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "c",
        lang: C,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "cpp",
        lang: Cpp,
        comment_kinds: &["comment"],
    },
    LangSpec {
        name: "yaml",
        lang: Yaml,
        comment_kinds: &["comment"],
    },
];

/// One extracted comment with its position and the identifiers of the code
/// that follows it — the neighbour `comment-paraphrase` compares against.
pub struct Comment {
    pub text: String,
    /// 1-based
    pub line: u32,
    /// 1-based character column
    pub col: u32,
    /// 1-based line of the comment's last character
    pub end_line: u32,
    /// Identifiers of the first code construct after this comment (empty
    /// when the file ends in comments).
    pub next_code_ids: Vec<String>,
}

/// Identifier-ish node kinds across the v1 grammars: value/field/property
/// names. String literals and types are deliberately excluded — a comment
/// restating a string constant is paraphrase of data, not of structure.
fn is_ident_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "field_identifier"
            | "property_identifier"
            | "variable_name"
            | "key"
            | "string_scalar"
    )
}

fn collect_ids(node: Node<'_, StrDoc<SupportLang>>, out: &mut Vec<String>, depth: u32, cap: u32) {
    if out.len() >= 24 || depth > cap {
        return;
    }
    if is_ident_kind(&node.kind()) {
        out.push(node.text().to_string());
    }
    for child in node.children() {
        collect_ids(child, out, depth + 1, cap);
    }
}

/// Manual preorder over the syntax tree. `Node::dfs()` from a matched
/// comment would descend into it (and yield its `doc_comment` child); this
/// walk stops at every matched comment instead.
struct Walk {
    kind_ids: Vec<u16>,
}

impl Walk {
    fn run<'r>(&self, root: Node<'r, StrDoc<SupportLang>>) -> Vec<Node<'r, StrDoc<SupportLang>>> {
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if self.kind_ids.contains(&node.kind_id()) {
                found.push(node);
                continue; // never descend into a matched comment
            }
            for child in node.children().collect::<Vec<_>>().into_iter().rev() {
                stack.push(child);
            }
        }
        found.sort_by_key(|n| (n.range().start,));
        found
    }
}

/// Extract every comment of `source` under `spec`, in source order, each
/// with the identifiers of the code that follows it.
pub fn extract(spec: &LangSpec, source: &str) -> Vec<Comment> {
    let kind_ids: Vec<u16> = spec
        .comment_kinds
        .iter()
        .map(|k| spec.lang.kind_to_id(k))
        .collect();
    let grep = spec.lang.ast_grep(source);
    let root = grep.root();
    let walk = Walk { kind_ids };
    let nodes = walk.run(root.clone());

    let mut out = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let start = node.start_pos();
        let end = node.end_pos();
        let end_line = end.line() as u32 + 1;
        // YAML keys live six wrapper levels deep; the previous depth cap
        // hid every key, which is why config files were invisible to
        // comment-paraphrase. Other languages keep the shallow cap: going
        // deep there collects the whole function below and turns prose
        // that names its dependencies into "overlap" (measured +653
        // findings on config_v3, almost all false positives).
        let next_code_ids = next_ids_after(&root, node.range().start, spec.name == "yaml");
        out.push(Comment {
            text: node.text().to_string(),
            line: start.line() as u32 + 1,
            col: start.column(node) as u32 + 1,
            end_line,
            next_code_ids,
        });
    }
    if spec.name == "python" {
        out.extend(docstrings(&root));
        out.sort_by_key(|c| (c.line, c.col));
    }
    out
}

/// Python docstrings: a lone string statement opening a module, class or
/// function body. They carry the same prose as comments, and prose written
/// there was invisible to every rule. `next_code_ids` carries the owner's
/// name (class/function identifier) when there is one: a docstring
/// documents its owner, and the restates-name verdict compares against
/// exactly that name. Module docstrings keep it empty — the module has
/// no single name.
fn docstrings(root: &Node<'_, StrDoc<SupportLang>>) -> Vec<Comment> {
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        let owner_ok = match node.kind().as_ref() {
            "module" => true,
            "block" => node.parent().is_some_and(|p| {
                matches!(
                    p.kind().as_ref(),
                    "function_definition" | "class_definition"
                )
            }),
            _ => false,
        };
        if owner_ok {
            let first = node
                .children()
                .find(|c| c.is_named() && c.kind() != "comment");
            if let Some(stmt) = first.filter(|s| s.kind() == "expression_statement") {
                let mut named = stmt.children().filter(|c| c.is_named());
                if let (Some(string), None) = (named.next(), named.next()) {
                    if string.kind() == "string" {
                        let start = string.start_pos();
                        // The owner definition's identifier, when the
                        // docstring hangs off a class or function block.
                        let owner_name = node.parent().and_then(|p| {
                            p.children()
                                .find(|c| c.kind() == "identifier")
                                .map(|c| c.text().to_string())
                        });
                        // A module docstring whose module wraps exactly one
                        // definition effectively names that definition —
                        // corpus offenders are single-class module headers
                        // restating the class name.
                        let owner_name = match (owner_name, node.kind().as_ref()) {
                            (Some(name), _) => Some(name),
                            (None, "module") => {
                                let defs: Vec<_> = node
                                    .children()
                                    .filter(|c| {
                                        matches!(
                                            c.kind().as_ref(),
                                            "function_definition" | "class_definition"
                                        )
                                    })
                                    .collect();
                                (defs.len() == 1)
                                    .then(|| defs.first())
                                    .flatten()
                                    .and_then(|d| {
                                        d.children()
                                            .find(|c| c.kind() == "identifier")
                                            .map(|c| c.text().to_string())
                                    })
                            }
                            (None, _) => None,
                        };
                        out.push(Comment {
                            text: string.text().to_string(),
                            line: start.line() as u32 + 1,
                            col: start.column(&string) as u32 + 1,
                            end_line: string.end_pos().line() as u32 + 1,
                            next_code_ids: owner_name.into_iter().collect(),
                        });
                    }
                }
            }
        }
        stack.extend(node.children());
    }
    out
}

/// Identifiers of the first code node that STARTS after `offset` and is not
/// itself a comment. Depth- and count-capped so a pathological file cannot
/// stall the walk.
fn next_ids_after(root: &Node<'_, StrDoc<SupportLang>>, offset: usize, deep: bool) -> Vec<String> {
    let mut ids = Vec::new();
    let mut stack = vec![root.clone()];
    if std::env::var_os("AGENT_SLOP_DEBUG_IDS").is_some() {
        eprintln!("offset={offset} deep={deep}");
    }
    while let Some(node) = stack.pop() {
        if ids.len() >= 24 {
            break;
        }
        let is_comment = matches!(
            node.kind().as_ref(),
            "comment" | "line_comment" | "block_comment" | "doc_comment"
        );
        // In YAML every wrapper (stream, document, mappings) begins at byte
        // 0, so "begins after the comment" never selects the pair below;
        // the walk must therefore descend past position checks and take the
        // first mapping value that contains the documented pair.
        // In the other languages ancestors still fail the check and their
        // children are walked.
        let starts_after = node.range().start > offset;
        if !is_comment && (starts_after || deep) && !node.is_leaf() {
            // The first code construct at/after the comment in YAML is the
            // mapping value below it; elsewhere it starts strictly after.
            // deep also drops the depth cap: a nested key sits nine wrapper
            // levels down (stream > document > mapping > pair > block_node
            // > mapping > pair > flow_node > plain > string_scalar).
            collect_ids(node, &mut ids, 0, if deep { 16 } else { 4 });
            break;
        }
        for child in node.children().collect::<Vec<_>>().into_iter().rev() {
            stack.push(child);
        }
    }
    ids
}

/// Map a file path to its spec by extension. Unknown → `None` (the caller
/// skips the file).
pub fn spec_for_path(path: &str) -> Option<&'static LangSpec> {
    let ext = path.rsplit_once('.')?.1;
    let name = match ext {
        "rs" => "rust",
        "java" => "java",
        "py" | "pyi" => "python",
        "go" => "go",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "sh" | "bash" => "bash",
        "nix" => "nix",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "yaml" | "yml" => "yaml",
        _ => return None,
    };
    LANGS.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE measured trap: one `///` doc comment yields exactly ONE extracted
    /// comment, because `doc_comment` is a child of `line_comment` and the
    /// walk must not descend into a matched comment.
    #[test]
    fn one_doc_comment_yields_exactly_one_extraction() {
        let spec = LANGS.iter().find(|s| s.name == "rust").unwrap();
        let src = "\
/// Increments the counter.
fn inc() {}
";
        let comments = extract(spec, src);
        assert_eq!(
            comments.len(),
            1,
            "got {:?}",
            comments.iter().map(|c| c.text.clone()).collect::<Vec<_>>()
        );
        assert!(comments[0].text.starts_with("///"));
        assert_eq!(comments[0].line, 1);
    }

    #[test]
    fn yaml_comment_ids_cover_the_key_below_nested() {
        // Indented mapping under a key: the comment documents `port`.
        let src = "services:\n  # The port the web service listens on\n  port: 8000\n";
        let cs = extract(spec_for_path("a.yaml").unwrap(), src);
        assert_eq!(cs.len(), 1);
        assert!(
            cs[0].next_code_ids.iter().any(|i| i == "port"),
            "nested yaml key below a comment must be the id: {:?}",
            cs[0].next_code_ids
        );
    }

    #[test]
    fn yaml_comment_ids_cover_the_key_below() {
        let src = "# The port the service listens on\nport: 8000\n";
        let cs = extract(spec_for_path("a.yaml").unwrap(), src);
        assert_eq!(cs.len(), 1);
        println!("ids = {:?}", cs[0].next_code_ids);
        assert!(
            cs[0].next_code_ids.iter().any(|i| i.contains("port")),
            "yaml key below a comment must land in next_code_ids: {:?}",
            cs[0].next_code_ids
        );
    }

    #[test]
    fn all_five_rust_comment_forms_extract_exactly_five() {
        let src = concat!(
            "//! inner doc\n",
            "/// outer doc\n",
            "// plain\n",
            "/** block doc */\n",
            "/* block */\n",
            "let x = 1;\n",
        );
        let spec = LANGS.iter().find(|s| s.name == "rust").unwrap();
        let comments = extract(spec, src);
        assert_eq!(
            comments.len(),
            5,
            "got {:?}",
            comments.iter().map(|c| c.text.clone()).collect::<Vec<_>>()
        );
        for (i, c) in comments.iter().enumerate() {
            assert_eq!(c.line, i as u32 + 1);
        }
    }

    #[test]
    fn python_and_nix_extract_hash_comments() {
        let py = LANGS.iter().find(|s| s.name == "python").unwrap();
        let comments = extract(py, "# hello\nx = 1\n");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "# hello");

        let nix = LANGS.iter().find(|s| s.name == "nix").unwrap();
        let comments = extract(nix, "{\n  # a comment\n  x = 1;\n}\n");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "# a comment");
        assert_eq!(comments[0].line, 2);
    }

    #[test]
    fn string_literals_are_not_comments() {
        let spec = LANGS.iter().find(|s| s.name == "rust").unwrap();
        let comments = extract(spec, "let s = \"// not a comment\";\n");
        assert!(comments.is_empty());
    }

    #[test]
    fn path_mapping_covers_v1_languages() {
        assert_eq!(spec_for_path("src/main.rs").unwrap().name, "rust");
        assert_eq!(spec_for_path("a.py").unwrap().name, "python");
        assert_eq!(spec_for_path("a.tsx").unwrap().name, "tsx");
        assert_eq!(spec_for_path("flake.nix").unwrap().name, "nix");
        assert_eq!(spec_for_path("conf.yaml").unwrap().name, "yaml");
        assert!(spec_for_path("README.md").is_none());
        assert!(spec_for_path("Cargo.toml").is_none());
    }
}
