mod program;
#[path = "../support.rs"]
mod support;
use program::{Document, Value};
use support::*;
struct Node(Value<Node>);
#[derive(Default)]
struct Tree;
impl Tree {
    fn print(n: &Node, d: usize) -> String {
        match &n.0 {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(s) => s.clone(),
            Value::Text(s) => program::quote(s),
            Value::Array(xs) => program::aggregate(
                '[',
                ']',
                xs.iter().map(|n| Self::print(n, d + 1)).collect(),
                d,
            ),
            Value::Object(xs) => program::aggregate(
                '{',
                '}',
                xs.iter()
                    .map(|(k, n)| format!("{}: {}", program::quote(k), Self::print(n, d + 1)))
                    .collect(),
                d,
            ),
        }
    }
}
impl Document for Tree {
    type Node = Node;
    fn node(&mut self, v: Value<Node>) -> Node {
        Node(v)
    }
    fn pretty(&self, n: &Node) -> String {
        Self::print(n, 0)
    }
    fn query(&self, n: &Node, path: &[&str]) -> Result<String> {
        let mut p = n;
        for key in path {
            p = match &p.0 {
                Value::Object(xs) => &xs.iter().find(|(k, _)| k == key).ok_or("MissingPath")?.1,
                Value::Array(xs) => xs
                    .get(key.parse::<usize>().map_err(|_| "WrongKind")?)
                    .ok_or("MissingPath")?,
                _ => return Err("WrongKind".into()),
            };
        }
        Ok(Self::print(p, 0))
    }
}
fn main() {
    program::run::<Tree>();
}
