//! The calling thread's guard state in one destructor-free word: its owner
//! scope's mode, whether it has run the allocator's setup, whether the oracle
//! is on, and (debug builds) its open Free recorder sections. An allocation or
//! free on a set-up thread with no scope, no oracle and no section tests this
//! word against zero and takes the full path only otherwise.
use std::cell::Cell;

/// The owner scope's mode, kept by `admission`'s scopes.
pub(crate) const MODE: u32 = 0b11;
/// This thread has not run the allocator's setup yet.
const FRESH: u32 = 1 << 2;
/// `HIRSEL_GUARD_ORACLE=1`: every free takes the full path, which asks the table.
const ORACLE: u32 = 1 << 3;
/// One open Free recorder section.
#[cfg(debug_assertions)]
const SECTION: u32 = 1 << 8;

thread_local! {
    static WORD: Cell<u32> = const { Cell::new(FRESH) };
}

/// The word, or `FRESH` (the full path) if it is unreadable. A
/// destructor-free cell is never torn down, so that never happens; `try_with`
/// inlines to one thread-pointer load where `with` stays an out-of-line call.
#[inline(always)]
pub(crate) fn word() -> u32 {
    WORD.try_with(Cell::get).unwrap_or(FRESH)
}

fn update(f: impl FnOnce(u32) -> u32) {
    let _ = WORD.try_with(|word| word.set(f(word.get())));
}

/// The word after this thread's setup: the first call sets the process limit
/// if no thread has, and records whether the oracle is on.
#[inline(always)]
pub(crate) fn ready() -> u32 {
    let word = word();
    if word & FRESH == 0 {
        return word;
    }
    set_up(word)
}

#[cold]
#[inline(never)]
fn set_up(word: u32) -> u32 {
    crate::limit_once();
    let oracle = if crate::admission::oracle_on() {
        ORACLE
    } else {
        0
    };
    let word = word & !FRESH | oracle;
    update(|_| word);
    word
}

pub(crate) fn set_mode(mode: u32) {
    update(|word| word & !MODE | mode);
}

#[cfg(debug_assertions)]
pub(crate) fn section(open: bool) {
    update(|word| if open { word + SECTION } else { word - SECTION });
}

#[cfg(debug_assertions)]
pub(crate) fn sections_open(word: u32) -> bool {
    word >= SECTION
}
