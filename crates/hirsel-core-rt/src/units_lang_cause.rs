//! A helper's failure cause, carried to its owner: an owned `String`
//! message is copied out of the helper's limit and admitted again at the join.
use super::Held;
use crate::{
    Context, block,
    causes::{self, Cause, StringMessage},
};
use hirsel_core_layout::charge;
use std::ptr;

pub(super) struct CrossedCause {
    cause: Cause,
    owned_message: Vec<u8>,
    pub(super) message_charge: u64,
}

// SAFETY: owned bytes are copied into the Vec, and the remaining cause pointer
// is either null, run-lived immutable storage or a scalar payload.
unsafe impl Send for CrossedCause {}

impl CrossedCause {
    pub(super) fn pack(cause: &Cause) -> Self {
        let StringMessage {
            data,
            len,
            capacity,
        } = cause.message;
        let (data, owned_message, message_charge) = if cause.kind() == causes::FAIL && capacity != 0
        {
            let content = if len == 0 {
                Vec::new()
            } else {
                // SAFETY: the live cause owns this initialized String prefix.
                unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec()
            };
            (ptr::null_mut(), content, charge::seq(capacity, 1).unwrap())
        } else {
            // Static message bytes live through the run. Other causes carry
            // scalar payloads in these words.
            (data, Vec::new(), 0)
        };
        Self {
            cause: Cause {
                message: StringMessage {
                    data,
                    len,
                    capacity,
                },
                site: cause.site,
            },
            owned_message,
            message_charge,
        }
    }

    /// A cause a replayed helper end delivers: a `Fail` message of
    /// capacity 0 is static text, run-lived as the run's own literals are.
    #[cfg(feature = "record")]
    pub(super) fn replayed(c: crate::tape::units::replay::DueCause) -> Self {
        let cause = match c.kind {
            causes::FAIL => {
                let len = c.message.len() as u64;
                let data = if c.capacity == 0 {
                    c.message.clone().leak().as_mut_ptr()
                } else {
                    ptr::null_mut()
                };
                Cause::fail(
                    StringMessage {
                        data,
                        len,
                        capacity: c.capacity,
                    },
                    c.site,
                )
            }
            causes::CAPACITY => Cause::capacity(
                crate::Capacity {
                    requested: c.bytes.0,
                    available: c.bytes.1,
                },
                c.site,
            ),
            kind => Cause::new(kind, c.site),
        };
        let (owned_message, message_charge) = if c.kind == causes::FAIL && c.capacity != 0 {
            (c.message, charge::seq(c.capacity, 1).unwrap())
        } else {
            (Vec::new(), 0)
        };
        Self {
            cause,
            owned_message,
            message_charge,
        }
    }

    pub(super) fn unpack(self, context: &Context, held: &mut Held) -> Cause {
        let mut cause = self.cause;
        if self.message_charge != 0 {
            let data = block::allocate_admitted(
                context,
                held.partition(self.message_charge),
                cause.message.capacity as usize,
                8,
                block::PLAIN,
            );
            // SAFETY: the copied prefix fits in the new buffer at the original capacity.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.owned_message.as_ptr(),
                    data.as_ptr(),
                    self.owned_message.len(),
                );
            }
            cause.message.data = data.as_ptr();
        }
        cause
    }
}
