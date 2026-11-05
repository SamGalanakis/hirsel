//! Byte repetition must retain the reserve-and-fill loop's bulk lowering.
use hirsel_driver::native;
use hirsel_front_phases::load_program_source;
use hirsel_intern::jobs::with_compiler_stack;
use std::path::Path;
