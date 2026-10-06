use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

fn write_message(stdin: &mut impl Write, value: &serde_json::Value) {
    let body = serde_json::to_string(value).expect("serialize");
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write frame");
    stdin.flush().expect("flush");
}

fn read_message(reader: &mut impl BufRead) -> serde_json::Value {
    let mut content_length = 0_usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header");
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
            content_length = value.parse().expect("content length");
        }
    }
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).expect("read body");
    serde_json::from_slice(&body).expect("decode body")
}

#[test]
fn stdio_lifecycle() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lilac-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lilac-lsp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));

    write_message(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} },
        }),
    );
    let response = read_message(&mut reader);
    assert_eq!(response["id"], 1);
    assert!(
        response["result"]["capabilities"].is_object(),
        "no capabilities: {response}"
    );

    write_message(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );
    write_message(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null,
        }),
    );
    let response = read_message(&mut reader);
    assert_eq!(response["id"], 2);

    write_message(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );

    let status = child.wait().expect("wait for child");
    assert_eq!(status.code(), Some(0));
}
