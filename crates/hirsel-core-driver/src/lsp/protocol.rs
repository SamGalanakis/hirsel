use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};

const MAX_HEADER: u64 = 8 * 1024;
const MAX_BODY: usize = 64 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn read(input: &mut impl BufRead) -> io::Result<Option<serde_json::Result<Value>>> {
    let mut length = None;
    let mut header = input.take(MAX_HEADER);
    let mut line = String::new();
    loop {
        line.clear();
        if header.read_line(&mut line)? == 0 {
            return if header.limit() == MAX_HEADER {
                Ok(None)
            } else {
                Err(invalid("incomplete or oversized LSP header"))
            };
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .strip_suffix("\r\n")
            .and_then(|line| line.split_once(':'))
            .ok_or_else(|| invalid("invalid LSP header"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(invalid("duplicate LSP Content-Length"));
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|length| *length <= MAX_BODY)
                    .ok_or_else(|| invalid("invalid or oversized LSP Content-Length"))?,
            );
        }
    }
    let mut body = vec![0; length.ok_or_else(|| invalid("missing LSP Content-Length"))?];
    input.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)))
}

pub(super) fn write(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

pub(super) fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub(super) fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

pub(super) fn notify(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}
