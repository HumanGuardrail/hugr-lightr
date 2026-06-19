use super::*;
use std::io::Cursor;

#[test]
fn mcp_initialize_responds_with_protocol_version() {
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#.to_string() + "\n";
    let mut output = Vec::<u8>::new();
    let reader = Cursor::new(input.as_bytes().to_vec());
    run_mcp_loop(reader, &mut output);
    let resp = String::from_utf8(output).unwrap();
    assert!(
        resp.contains(r#""protocolVersion":"2025-03-26""#),
        "missing protocolVersion: {resp}"
    );
    assert!(
        resp.contains(r#""name":"lightr""#),
        "missing server name: {resp}"
    );
}

#[test]
fn mcp_tools_list_returns_five_tools() {
    let input =
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#.to_string() + "\n";
    let mut output = Vec::<u8>::new();
    let reader = Cursor::new(input.as_bytes().to_vec());
    run_mcp_loop(reader, &mut output);
    let resp = String::from_utf8(output).unwrap();
    let count = [
        "lightr_snapshot",
        "lightr_hydrate",
        "lightr_status",
        "lightr_run",
        "lightr_diff",
    ]
    .iter()
    .filter(|&&n| resp.contains(n))
    .count();
    assert_eq!(count, 5, "expected 5 tools in response: {resp}");
}

#[test]
fn mcp_unknown_method_returns_32601() {
    let input =
        r#"{"jsonrpc":"2.0","id":3,"method":"unknown/method","params":{}}"#.to_string() + "\n";
    let mut output = Vec::<u8>::new();
    let reader = Cursor::new(input.as_bytes().to_vec());
    run_mcp_loop(reader, &mut output);
    let resp = String::from_utf8(output).unwrap();
    assert!(resp.contains("-32601"), "expected -32601 error: {resp}");
}

#[test]
fn mcp_eof_returns_0() {
    let input = b"";
    let mut output = Vec::<u8>::new();
    let reader = Cursor::new(input.to_vec());
    let code = run_mcp_loop(reader, &mut output);
    assert_eq!(code, 0);
}

#[test]
fn mcp_notification_no_response() {
    let input = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#
        .to_string()
        + "\n";
    let mut output = Vec::<u8>::new();
    let reader = Cursor::new(input.as_bytes().to_vec());
    run_mcp_loop(reader, &mut output);
    // no output for notifications
    assert!(
        output.is_empty(),
        "notifications must not produce output, got: {:?}",
        String::from_utf8_lossy(&output)
    );
}
