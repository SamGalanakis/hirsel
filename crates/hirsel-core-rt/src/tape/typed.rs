//! Typed socket resources: the Exec pipes and Unix objects whose release
//! the tape records.
use super::*;

/// The typed resources added by Exec and Unix carry CLOSE at release.
pub(crate) fn typed_socket(provider: &Provider, key: u64) -> bool {
    let state = cell_of(provider.state).state.lock().unwrap();
    state.generation == generation_of(key)
        && state
            .live
            .as_ref()
            .and_then(|live| live.typed_resources.get(&ordinal_of(key)))
            .is_some_and(|resource| resource.kind != ResourceKind::Proc)
}

pub(crate) fn socket_parts(provider: &Provider, key: u64, parts: u8) {
    let mut state = cell_of(provider.state).state.lock().unwrap();
    if state.generation == generation_of(key)
        && let Some(resource) = state
            .live
            .as_mut()
            .and_then(|live| live.typed_resources.get_mut(&ordinal_of(key)))
        && resource.kind == ResourceKind::UnixStream
    {
        resource.parts = parts;
    }
}
