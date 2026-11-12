//! Header-only indexing and lexical resolution for core syntax (LANG-7, PROG-1/2).
//! Lookup tables/scopes and negative reads adapt hirsel-resolve; inputs and identities
//! belong exclusively to the new front end. No callee body is available to a resolver.
mod discovery;
mod index;
mod resolve;
pub use discovery::*;
pub use index::*;
pub use resolve::*;
#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;
