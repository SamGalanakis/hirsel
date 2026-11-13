//! Identifier symbols owned by one compilation package, including its dependencies.
//! Safe and session-local: no global interner, locks, or extended lifetimes.
use rustc_hash::FxHashMap;
use std::sync::Arc;
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Symbol(pub u32);
pub mod sym {
    use super::Symbol;
    pub const NONE: Symbol = Symbol(0);
    pub const PACKED: Symbol = Symbol(109);
    pub const STREAMABLE: Symbol = Symbol(110);
    pub const CLONE: Symbol = Symbol(111);
    pub const FN: Symbol = Symbol(1);
    pub const LET: Symbol = Symbol(2);
    pub const MUT: Symbol = Symbol(3);
    pub const MATCH: Symbol = Symbol(4);
    pub const IF: Symbol = Symbol(5);
    pub const ELSE: Symbol = Symbol(6);
    pub const LOOP: Symbol = Symbol(7);
    pub const WHILE: Symbol = Symbol(8);
    pub const FOR: Symbol = Symbol(9);
    pub const IN: Symbol = Symbol(10);
    pub const BREAK: Symbol = Symbol(11);
    pub const CONTINUE: Symbol = Symbol(12);
    pub const RETURN: Symbol = Symbol(13);
    pub const TAKE: Symbol = Symbol(14);
    pub const COPY: Symbol = Symbol(15);
    pub const VIEW: Symbol = Symbol(16);
    pub const TYPE: Symbol = Symbol(17);
    pub const STABLE: Symbol = Symbol(18);
    pub const RESOURCE: Symbol = Symbol(19);
    pub const SCOPED: Symbol = Symbol(20);
    pub const REGION: Symbol = Symbol(21);
    pub const BOUND: Symbol = Symbol(22);
    pub const PUB: Symbol = Symbol(23);
    pub const OPAQUE: Symbol = Symbol(24);
    pub const CONST: Symbol = Symbol(25);
    pub const WHERE: Symbol = Symbol(26);
    pub const TRUE: Symbol = Symbol(27);
    pub const FALSE: Symbol = Symbol(28);
    pub const BYTES: Symbol = Symbol(29);
    pub const DOTDOTEQ: Symbol = Symbol(30);
    pub const SHLEQ: Symbol = Symbol(31);
    pub const SHREQ: Symbol = Symbol(32);
    pub const DOTDOT: Symbol = Symbol(33);
    pub const ARROW: Symbol = Symbol(34);
    pub const FATARROW: Symbol = Symbol(35);
    pub const ANDAND: Symbol = Symbol(36);
    pub const OROR: Symbol = Symbol(37);
    pub const SHL: Symbol = Symbol(38);
    pub const SHR: Symbol = Symbol(39);
    pub const EQEQ: Symbol = Symbol(40);
    pub const NE: Symbol = Symbol(41);
    pub const LE: Symbol = Symbol(42);
    pub const GE: Symbol = Symbol(43);
    pub const PLUSEQ: Symbol = Symbol(44);
    pub const MINUSEQ: Symbol = Symbol(45);
    pub const STAREQ: Symbol = Symbol(46);
    pub const SLASHEQ: Symbol = Symbol(47);
    pub const PERCENTEQ: Symbol = Symbol(48);
    pub const AMPEQ: Symbol = Symbol(49);
    pub const PIPEEQ: Symbol = Symbol(50);
    pub const CARETEQ: Symbol = Symbol(51);
    pub const LPAREN: Symbol = Symbol(52);
    pub const RPAREN: Symbol = Symbol(53);
    pub const LBRACKET: Symbol = Symbol(54);
    pub const RBRACKET: Symbol = Symbol(55);
    pub const LBRACE: Symbol = Symbol(56);
    pub const RBRACE: Symbol = Symbol(57);
    pub const COMMA: Symbol = Symbol(58);
    pub const COLON: Symbol = Symbol(59);
    pub const SEMICOLON: Symbol = Symbol(60);
    pub const DOT: Symbol = Symbol(61);
    pub const AT: Symbol = Symbol(62);
    pub const QUESTION: Symbol = Symbol(63);
    pub const PLUS: Symbol = Symbol(64);
    pub const MINUS: Symbol = Symbol(65);
    pub const STAR: Symbol = Symbol(66);
    pub const SLASH: Symbol = Symbol(67);
    pub const PERCENT: Symbol = Symbol(68);
    pub const BANG: Symbol = Symbol(69);
    pub const TILDE: Symbol = Symbol(70);
    pub const AMP: Symbol = Symbol(71);
    pub const PIPE: Symbol = Symbol(72);
    pub const CARET: Symbol = Symbol(73);
    pub const EQ: Symbol = Symbol(74);
    pub const LT: Symbol = Symbol(75);
    pub const GT: Symbol = Symbol(76);
    pub const UNDERSCORE: Symbol = Symbol(77);
    pub const DROP: Symbol = Symbol(78);
    pub const COMPACT: Symbol = Symbol(79);
    pub const TWIN: Symbol = Symbol(80);
    pub const TEST: Symbol = Symbol(81);
    pub const GRANT: Symbol = Symbol(82);
    pub const OWN: Symbol = Symbol(83);
    pub const SEND: Symbol = Symbol(84);
    pub const WAITS: Symbol = Symbol(85);
    pub const PLAIN: Symbol = Symbol(86);
    pub const SHAREABLE: Symbol = Symbol(87);
    pub const KEY: Symbol = Symbol(88);
    pub const ORDERED: Symbol = Symbol(89);
    pub const INTEGER: Symbol = Symbol(90);
    pub const FLOAT: Symbol = Symbol(91);
    pub const INLINE: Symbol = Symbol(92);
    pub const BOOL: Symbol = Symbol(93);
    pub const CHAR: Symbol = Symbol(94);
    pub const U8: Symbol = Symbol(95);
    pub const U16: Symbol = Symbol(96);
    pub const U32: Symbol = Symbol(97);
    pub const U64: Symbol = Symbol(98);
    pub const I8: Symbol = Symbol(99);
    pub const I16: Symbol = Symbol(100);
    pub const I32: Symbol = Symbol(101);
    pub const I64: Symbol = Symbol(102);
    pub const F32: Symbol = Symbol(103);
    pub const F64: Symbol = Symbol(104);
    pub const NONE_VARIANT: Symbol = Symbol(105);
    pub const SOME_VARIANT: Symbol = Symbol(106);
    pub const OK_VARIANT: Symbol = Symbol(107);
    pub const ERROR_VARIANT: Symbol = Symbol(108);
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Symbols {
    strings: hirsel_core_ids::published::Table<Arc<str>>,
    #[serde(skip)]
    index: FxHashMap<Arc<str>, Symbol>,
    published_index: hirsel_core_ids::published::Map<Arc<str>, Symbol>,
    #[serde(skip)]
    single: [Symbol; 128],
}
fn empty_single() -> [Symbol; 128] {
    [sym::NONE; 128]
}
impl<'de> serde::Deserialize<'de> for Symbols {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            strings: hirsel_core_ids::published::Table<Arc<str>>,
            published_index: hirsel_core_ids::published::Map<Arc<str>, Symbol>,
        }
        let wire = Wire::deserialize(decoder)?;
        let base = wire.strings.base_len();
        let known = Self::default();
        let mut index = known.index;
        index.extend(
            wire.strings
                .own_iter()
                .enumerate()
                .filter(|(i, _)| base + *i != 0)
                .map(|(i, s)| (s.clone(), Symbol((base + i) as u32))),
        );
        Ok(Self {
            strings: wire.strings,
            index,
            published_index: wire.published_index,
            single: known.single,
        })
    }
}
impl Default for Symbols {
    fn default() -> Self {
        let mut strings: Vec<Arc<str>> = vec![
            Arc::from(""),
            Arc::from("fn"),
            Arc::from("let"),
            Arc::from("mut"),
            Arc::from("match"),
            Arc::from("if"),
            Arc::from("else"),
            Arc::from("loop"),
            Arc::from("while"),
            Arc::from("for"),
            Arc::from("in"),
            Arc::from("break"),
            Arc::from("continue"),
            Arc::from("return"),
            Arc::from("take"),
            Arc::from("copy"),
            Arc::from("view"),
            Arc::from("type"),
            Arc::from("stable"),
            Arc::from("resource"),
            Arc::from("scoped"),
            Arc::from("region"),
            Arc::from("bound"),
            Arc::from("pub"),
            Arc::from("opaque"),
            Arc::from("const"),
            Arc::from("where"),
            Arc::from("true"),
            Arc::from("false"),
            Arc::from("bytes"),
            Arc::from("..="),
            Arc::from("<<="),
            Arc::from(">>="),
            Arc::from(".."),
            Arc::from("->"),
            Arc::from("=>"),
            Arc::from("&&"),
            Arc::from("||"),
            Arc::from("<<"),
            Arc::from(">>"),
            Arc::from("=="),
            Arc::from("!="),
            Arc::from("<="),
            Arc::from(">="),
            Arc::from("+="),
            Arc::from("-="),
            Arc::from("*="),
            Arc::from("/="),
            Arc::from("%="),
            Arc::from("&="),
            Arc::from("|="),
            Arc::from("^="),
            Arc::from("("),
            Arc::from(")"),
            Arc::from("["),
            Arc::from("]"),
            Arc::from("{"),
            Arc::from("}"),
            Arc::from(","),
            Arc::from(":"),
            Arc::from(";"),
            Arc::from("."),
            Arc::from("@"),
            Arc::from("?"),
            Arc::from("+"),
            Arc::from("-"),
            Arc::from("*"),
            Arc::from("/"),
            Arc::from("%"),
            Arc::from("!"),
            Arc::from("~"),
            Arc::from("&"),
            Arc::from("|"),
            Arc::from("^"),
            Arc::from("="),
            Arc::from("<"),
            Arc::from(">"),
            Arc::from("_"),
            Arc::from("drop"),
            Arc::from("compact"),
            Arc::from("twin"),
            Arc::from("test"),
            Arc::from("grant"),
            Arc::from("own"),
            Arc::from("send"),
            Arc::from("waits"),
            Arc::from("plain"),
            Arc::from("shareable"),
            Arc::from("key"),
            Arc::from("ordered"),
            Arc::from("integer"),
            Arc::from("float"),
            Arc::from("Inline"),
            Arc::from("bool"),
            Arc::from("char"),
            Arc::from("u8"),
            Arc::from("u16"),
            Arc::from("u32"),
            Arc::from("u64"),
            Arc::from("i8"),
            Arc::from("i16"),
            Arc::from("i32"),
            Arc::from("i64"),
            Arc::from("f32"),
            Arc::from("f64"),
            Arc::from("None"),
            Arc::from("Some"),
            Arc::from("Ok"),
            Arc::from("Error"),
        ];
        strings.push(Arc::from("packed"));
        strings.push(Arc::from("streamable"));
        strings.push(Arc::from("clone"));
        let mut single = empty_single();
        for (i, s) in strings.iter().enumerate().skip(1) {
            if let [b] = s.as_bytes() {
                if b.is_ascii() {
                    single[*b as usize] = Symbol(i as u32);
                }
            }
        }
        Self {
            single,
            index: strings
                .iter()
                .enumerate()
                .skip(1)
                .map(|(i, s)| (s.clone(), Symbol(i as u32)))
                .collect(),
            strings: strings.into(),
            published_index: Default::default(),
        }
    }
}
impl Symbols {
    pub fn publish(
        &self,
        writer: &mut impl hirsel_core_ids::published::Publisher,
    ) -> std::io::Result<Self> {
        use hirsel_core_ids::published::Map;
        let names: Map<Arc<str>, Symbol> = self
            .strings
            .iter()
            .enumerate()
            .map(|(i, s)| (Arc::clone(s), Symbol(i as u32)))
            .collect();
        let known = Self::default();
        Ok(Self {
            strings: writer.table(&self.strings)?,
            index: known.index,
            published_index: writer.map(&names)?,
            single: known.single,
        })
    }
    pub fn intern(&mut self, text: &str) -> Symbol {
        let single = match text.as_bytes() {
            [b] if b.is_ascii() => Some(*b as usize),
            _ => None,
        };
        if let Some(b) = single {
            if self.single[b] != sym::NONE {
                return self.single[b];
            }
        }
        let id = if let Some(&id) = self.index.get(text) {
            id
        } else if let Some(id) = self.published_index.get(text).copied() {
            self.index.insert(Arc::from(text), id);
            id
        } else {
            let id = Symbol(
                self.strings
                    .len()
                    .try_into()
                    .expect("symbol table exceeds address space"),
            );
            let text: Arc<str> = Arc::from(text);
            self.strings.push(text.clone());
            self.index.insert(text, id);
            id
        };
        if let Some(b) = single {
            self.single[b] = id;
        }
        id
    }
    #[inline]
    pub fn find(&self, text: &str) -> Option<Symbol> {
        self.index
            .get(text)
            .copied()
            .or_else(|| self.published_index.get(text).copied())
    }
    #[inline(always)]
    pub fn text(&self, id: Symbol) -> &str {
        &self.strings[id.0 as usize]
    }
    pub fn len(&self) -> usize {
        self.strings.len()
    }
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}
pub fn keyword_symbol(s: Symbol) -> bool {
    matches!(
        s,
        sym::FN
            | sym::LET
            | sym::MUT
            | sym::MATCH
            | sym::IF
            | sym::ELSE
            | sym::LOOP
            | sym::WHILE
            | sym::FOR
            | sym::IN
            | sym::BREAK
            | sym::CONTINUE
            | sym::RETURN
            | sym::TAKE
            | sym::COPY
            | sym::VIEW
            | sym::TYPE
            | sym::STABLE
            | sym::RESOURCE
            | sym::SCOPED
            | sym::REGION
            | sym::BOUND
            | sym::PUB
            | sym::OPAQUE
            | sym::CONST
            | sym::WHERE
            | sym::TRUE
            | sym::FALSE
            | sym::BYTES
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::{
        ContentHash,
        published::{self, Descriptor, Provider},
    };
    use std::{
        io,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Names {
        rows: Vec<(Arc<str>, Symbol)>,
        lookups: AtomicUsize,
    }
    impl Provider<(Arc<str>, Symbol)> for Names {
        fn row(&self, index: u32) -> io::Result<(Arc<str>, Symbol)> {
            Ok(self.rows[index as usize].clone())
        }
        fn find(&self, key: &[u8]) -> io::Result<Option<u32>> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            Ok(self
                .rows
                .iter()
                .position(|(name, _)| key.len() == 8 + name.len() && key.ends_with(name.as_bytes()))
                .map(|i| i as u32))
        }
    }
    struct Strings(Vec<Arc<str>>);
    impl Provider<Arc<str>> for Strings {
        fn row(&self, index: u32) -> io::Result<Arc<str>> {
            Ok(self.0[index as usize].clone())
        }
    }

    #[test]
    fn reopened_symbols_keep_reserved_names_without_product_lookups() {
        let mut original = Symbols::default();
        let library = original.intern("library_name");
        let names = Arc::new(Names {
            rows: original
                .strings
                .iter()
                .enumerate()
                .map(|(i, s)| (s.clone(), Symbol(i as u32)))
                .collect(),
            lookups: AtomicUsize::new(0),
        });
        let descriptor = Descriptor {
            namespace: ContentHash([173; 32]),
            table: 0,
            len: original.len() as u32,
            rows: ContentHash([174; 32]),
            lookup: ContentHash([175; 32]),
        };
        let mut published = Symbols {
            strings: published::Table::published(published::bind(
                descriptor,
                Arc::new(Strings(original.strings.iter().cloned().collect())),
            )),
            published_index: published::Map::published(published::bind(
                Descriptor {
                    table: 1,
                    ..descriptor
                },
                names.clone(),
            )),
            index: FxHashMap::default(),
            single: original.single,
        };
        let own = published.intern("own_name");
        let mut reopened: Symbols =
            serde_json::from_value(serde_json::to_value(&published).unwrap()).unwrap();
        names.lookups.store(0, Ordering::Relaxed);
        for (name, id) in &original.index {
            if *id != library {
                assert_eq!(reopened.intern(name), *id);
            }
        }
        assert_eq!(names.lookups.load(Ordering::Relaxed), 0);
        assert_eq!(reopened.intern("own_name"), own);
        assert_eq!(reopened.intern("library_name"), library);
        assert_eq!(reopened.text(library), "library_name");
        assert_eq!(reopened.index.get("library_name"), Some(&library));
        assert_eq!(reopened.intern("library_name"), library);
        assert_eq!(names.lookups.load(Ordering::Relaxed), 1);
        let fresh = reopened.intern("fresh_name");
        assert_eq!(fresh, Symbol(own.0 + 1));
        assert_eq!(reopened.intern("fresh_name"), fresh);
        assert_eq!(reopened.text(fresh), "fresh_name");
        assert_eq!(names.lookups.load(Ordering::Relaxed), 2);
    }
}
