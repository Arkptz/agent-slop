#![allow(clippy::expect_used)]

use agent_slop_protocol::{
    Cost, Input, Level, Manifest, PROTOCOL_VERSION, Request, Response, RuleDecl, Stage,
    is_supported,
};
use serde_json::Value;

const MANIFEST: &str = include_str!("fixtures/manifest.json");
const REQUEST: &str = include_str!("fixtures/request.json");
const RESPONSE: &str = include_str!("fixtures/response.json");
const RESPONSE_FUTURE: &str = include_str!("fixtures/response-future.json");

/// Serialising what was read must reproduce the recorded JSON value for
/// value: nothing is lost, renamed or defaulted into existence.
fn round_trip<T>(fixture: &str) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let parsed: T = serde_json::from_str(fixture).expect("fixture parses");
    let again: Value = serde_json::to_value(&parsed).expect("serialises");
    let original: Value = serde_json::from_str(fixture).expect("fixture is JSON");
    assert_eq!(again, original);
    parsed
}

#[test]
fn manifest_round_trips() {
    let m: Manifest = round_trip(MANIFEST);
    assert_eq!(m.input, Input::Comments);
    assert_eq!(m.cost, Cost::A);
    assert_eq!(m.rules[0].default_level, Level::Warning);
    assert!(m.deterministic);
}

#[test]
fn request_round_trips() {
    let r: Request = round_trip(REQUEST);
    assert_eq!(r.stage, Stage::Scan);
    assert_eq!(r.files[0].comments[0].next_code_ids, vec!["x"]);
}

#[test]
fn response_round_trips() {
    let r: Response = round_trip(RESPONSE);
    assert_eq!(r.findings[0].comment, Some(0));
    assert_eq!(r.findings[1].line, Some(7));
}

#[test]
fn unknown_fields_are_ignored_and_protocol_mismatch_is_detected() {
    let mut value: Value = serde_json::from_str(MANIFEST).expect("json");
    value["added_in_a_minor_release"] = Value::Bool(true);
    value["rules"][0]["extra"] = Value::from("x");
    let m: Manifest = serde_json::from_value(value).expect("unknown fields are ignored");
    assert!(is_supported(m.protocol));

    let future: Response = serde_json::from_str(RESPONSE_FUTURE).expect("parses");
    assert_eq!(future.protocol, 2);
    assert!(!is_supported(future.protocol));
    assert_eq!(PROTOCOL_VERSION, 1);
}

#[test]
fn optional_fields_default_when_absent() {
    let m: Manifest = serde_json::from_str(
        r#"{"protocol":1,"name":"a","version":"1","input":"files","cost":"B"}"#,
    )
    .expect("minimal manifest");
    assert_eq!(m.rules, [] as [RuleDecl; 0]);
    assert_eq!(m.langs, [] as [String; 0]);
    assert!(!m.deterministic);
}
