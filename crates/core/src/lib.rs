//! A starter Rust Cargo workspace wired with a Nix dev shell, SHA-pinned CI,
//! supply-chain auditing, and a `dg` decision graph.
//!
//! This is the workspace's core library crate: reusable logic lives here, unit-
//! and mutation-tested independently of the binary crate that consumes it. This
//! crate-level documentation is the single source of truth for the README
//! overview: edit it here and run `cargo rdme` to sync `README.md`.

#[must_use]
pub fn greeting(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Hello, world!".to_string()
    } else {
        format!("Hello, {name}!")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greets_named() {
        assert_eq!(greeting("Ada"), "Hello, Ada!");
    }

    #[test]
    fn falls_back_on_blank() {
        assert_eq!(greeting("  "), "Hello, world!");
    }
}
