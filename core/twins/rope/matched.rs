mod program;
#[path = "../support.rs"]
mod support;
use support::*;
struct Chunk {
    bytes: Box<str>,
    refs: u64,
}
#[derive(Clone, Copy)]
struct Piece {
    id: u32,
    a: u32,
    b: u32,
}
type Root = Vec<Piece>;
#[derive(Default)]
struct Text {
    chunks: Vec<Option<Chunk>>,
    free: Vec<u32>,
    live: usize,
}
impl Text {
    fn piece(&mut self, s: &str) -> Piece {
        let c = Some(Chunk {
            bytes: s.into(),
            refs: 1,
        });
        self.live += 1;
        let id = if let Some(id) = self.free.pop() {
            self.chunks[id as usize] = c;
            id
        } else {
            let id = u32::try_from(self.chunks.len()).unwrap();
            self.chunks.push(c);
            id
        };
        Piece {
            id,
            a: 0,
            b: u32::try_from(s.len()).unwrap(),
        }
    }
    fn retain(&mut self, p: Piece) -> Piece {
        self.chunks[p.id as usize].as_mut().unwrap().refs += 1;
        p
    }
    fn range(&mut self, r: &Root, a: usize, b: usize) -> Root {
        let mut offset = 0;
        let mut out = Vec::new();
        for &p in r {
            let end = offset + (p.b - p.a) as usize;
            if a < end && b > offset {
                out.push(self.retain(Piece {
                    id: p.id,
                    a: p.a + u32::try_from(a.saturating_sub(offset)).unwrap(),
                    b: p.a + u32::try_from(b.min(end) - offset).unwrap(),
                }));
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
        r.iter().map(|&p| self.retain(p)).collect()
    }
    fn edit(&mut self, r: &Root, pos: usize, len: usize, s: &str) -> Result<Root> {
        let text = self.render(r);
        let end = pos.checked_add(len).ok_or("InvalidPosition")?;
        if end > text.len() || !text.is_char_boundary(pos) || !text.is_char_boundary(end) {
            return Err("InvalidPosition".into());
        }
        let mut out = self.range(r, 0, pos);
        if !s.is_empty() {
            out.push(self.piece(s));
        }
        out.extend(self.range(r, end, text.len()));
        Ok(out)
    }
    fn render(&self, r: &Root) -> String {
        r.iter()
            .map(|p| {
                &self.chunks[p.id as usize].as_ref().unwrap().bytes[p.a as usize..p.b as usize]
            })
            .collect()
    }
    fn release(&mut self, r: Root) {
        for p in r {
            let c = self.chunks[p.id as usize].as_mut().unwrap();
            c.refs -= 1;
            if c.refs == 0 {
                self.chunks[p.id as usize] = None;
                self.free.push(p.id);
                self.live -= 1;
            }
        }
    }
    fn live(&self) -> usize {
        self.live
    }
}
fn main() {
    program::run::<Text>();
}
