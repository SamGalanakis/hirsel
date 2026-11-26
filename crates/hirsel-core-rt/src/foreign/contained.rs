//! FFI-12 to FFI-14: a contained foreign function runs in the calling task's
//! own instance of its module, loaded in place from the image.
use super::handles::{Kind, Parameter};
use crate::abi_layout as abi;

/// One contained module: its precompiled artifact in the image's
/// read-execute segment, and the name of its loan-area export.
#[repr(C)]
pub struct Module {
    pub artifact: *const u8,
    pub len: usize,
    pub loan_area: *const u8,
    pub loan_area_len: usize,
}

/// One contained function's call descriptor. `types` holds one argument type
/// per packed argument, `parameters` the handle kinds as FFI-6 reads them.
#[repr(C)]
pub struct Call {
    pub module: *const Module,
    pub name: *const u8,
    pub name_len: usize,
    pub parameters: *const Parameter,
    pub types: *const u8,
    pub count: usize,
    pub new_kind: *const Kind,
    pub result: u8,
    pub release: bool,
}

const _: () = {
    use std::mem::offset_of;
    assert!(size_of::<Module>() == abi::CONTAINED_MODULE_BYTES);
    assert!(offset_of!(Module, artifact) == abi::CONTAINED_MODULE_ARTIFACT as usize);
    assert!(offset_of!(Module, len) == abi::CONTAINED_MODULE_LEN);
    assert!(offset_of!(Module, loan_area) == abi::CONTAINED_MODULE_LOAN_AREA as usize);
    assert!(offset_of!(Module, loan_area_len) == abi::CONTAINED_MODULE_LOAN_AREA_LEN);
    assert!(size_of::<Call>() == abi::CONTAINED_CALL_BYTES);
    assert!(offset_of!(Call, module) == abi::CONTAINED_CALL_MODULE as usize);
    assert!(offset_of!(Call, name) == abi::CONTAINED_CALL_NAME as usize);
    assert!(offset_of!(Call, name_len) == abi::CONTAINED_CALL_NAME_LEN);
    assert!(offset_of!(Call, parameters) == abi::CONTAINED_CALL_PARAMETERS as usize);
    assert!(offset_of!(Call, types) == abi::CONTAINED_CALL_TYPES as usize);
    assert!(offset_of!(Call, count) == abi::CONTAINED_CALL_COUNT);
    assert!(offset_of!(Call, new_kind) == abi::CONTAINED_CALL_NEW as usize);
    assert!(offset_of!(Call, result) == abi::CONTAINED_CALL_RESULT);
    assert!(offset_of!(Call, release) == abi::CONTAINED_CALL_RELEASE);
};

pub(crate) use crate::host::contained::{Instances, end_task, release};
