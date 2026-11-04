//! Byte repetition must retain the reserve-and-fill loop's bulk lowering.
use hirsel_driver::native;
use hirsel_engine::with_compiler_stack;
use hirsel_front_phases::load_program_source;
use std::path::Path;
