mod program;
#[path = "../support.rs"]
mod support;
use std::cell::Cell;
use std::rc::Rc;
use support::*;
struct Chunk {
    text: String,
    live: Rc<Cell<usize>>,
}
impl Drop for Chunk {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1);
    }
}
#[derive(Clone)]
struct Piece {
    chunk: Rc<Chunk>,
    a: usize,
    b: usize,
}
type Root = Vec<Piece>;
#[derive(Default)]
struct Text {
    live: Rc<Cell<usize>>,
}
impl Text {
    fn piece(&self, s: &str) -> Piece {
        self.live.set(self.live.get() + 1);
        Piece {
            chunk: Rc::new(Chunk {
                text: s.into(),
                live: self.live.clone(),
            }),
            a: 0,
            b: s.len(),
        }
    }
    fn range(r: &Root, a: usize, b: usize) -> Root {
        let mut offset = 0;
        let mut out = Vec::new();
        for p in r {
            let end = offset + p.b - p.a;
            if a < end && b > offset {
                out.push(Piece {
                    chunk: p.chunk.clone(),
                    a: p.a + a.saturating_sub(offset),
                    b: p.a + (b.min(end) - offset),
                });
            }
            offset = end;
        }
        out
    }
}
impl program::Text for Text {
    type Root = Root;
    fn new_text(&mut self, s: &str) -> Root {
        vec![self.piece(s)]
    }
    fn snapshot(&mut self, r: &Root) -> Root {
        r.clone()
    }
    fn edit(&mut self, r: &Root, pos: usize, len: usize, s: &str) -> Result<Root> {
        let text = self.render(r);
        let end = pos.checked_add(len).ok_or("InvalidPosition")?;
        if end > text.len() || !text.is_char_boundary(pos) || !text.is_char_boundary(end) {
            return Err("InvalidPosition".into());
        }
        let mut out = Self::range(r, 0, pos);
        if !s.is_empty() {
            out.push(self.piece(s));
        }
        out.extend(Self::range(r, end, text.len()));
        Ok(out)
    }
    fn render(&self, r: &Root) -> String {
        r.iter().map(|p| &p.chunk.text[p.a..p.b]).collect()
    }
    fn release(&mut self, r: Root) {
        drop(r);
    }
    fn live(&self) -> usize {
        self.live.get()
    }
}
fn main() {
    program::run::<Text>();
}
