//! What each definition of an ELF release object owns besides its text: the
//! data its part defines strong whether or not its code reaches it.
use super::{InputSymbol, Section, raw};
use std::collections::HashMap;

/// A definition's owned symbols by its text section, and each one's owner.
type Owned = (HashMap<usize, Vec<usize>>, HashMap<usize, usize>);

/// [`Input::owned`] and [`Input::owner`]: each strong descriptor whose code
/// word (its first word) relocates to a symbol of a text section, and each
/// strong marker named after such a descriptor.
pub(super) fn owned(sections: &[Section], symbols: &[InputSymbol]) -> Owned {
    let strong_data = |y: &InputSymbol| {
        y.bind == raw::STB_GLOBAL
            && y.kind == raw::STT_OBJECT
            && y.section.is_some_and(|s| !sections[s].text)
    };
    let mut descriptors = HashMap::new();
    let mut owner = HashMap::new();
    for (i, y) in symbols.iter().enumerate() {
        if !strong_data(y) || !y.name.ends_with(b"$static") {
            continue;
        }
        let section = &sections[y.section.unwrap()];
        let code = section.relocations.iter().find(|r| r.0 == y.value);
        let text = code
            .and_then(|r| symbols[r.1].section)
            .filter(|&t| sections[t].text);
        if let Some(text) = text {
            descriptors.insert(y.name, text);
            owner.insert(i, text);
        }
    }
    for (i, y) in symbols.iter().enumerate() {
        if !strong_data(y) {
            continue;
        }
        let Some(at) = y.name.windows(11).position(|w| w == b"$spawnable$") else {
            continue;
        };
        if let Some(&text) = descriptors.get(&y.name[..at]) {
            owner.insert(i, text);
        }
    }
    let mut owned: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut order: Vec<_> = owner.iter().map(|(&y, &t)| (y, t)).collect();
    order.sort_unstable();
    for (symbol, text) in order {
        owned.entry(text).or_default().push(symbol);
    }
    (owned, owner)
}
