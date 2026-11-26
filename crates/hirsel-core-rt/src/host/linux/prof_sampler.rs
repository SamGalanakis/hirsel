//! Linux samples a profiled program from outside with perf events, so the
//! runtime has no sampler of its own threads here.
use std::io;

use crate::prof::Stack;

pub(crate) struct Sampler;

impl Sampler {
    pub(crate) fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux samples a profiled program with perf events",
        ))
    }

    pub(crate) fn round(&mut self, _stack: &mut Stack, _emit: &mut dyn FnMut(&Stack)) {}
}
