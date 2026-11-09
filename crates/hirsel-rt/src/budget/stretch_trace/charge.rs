#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Charge {
    Alloc(u64),
    Free(u64),
    Credit(u64),
}
