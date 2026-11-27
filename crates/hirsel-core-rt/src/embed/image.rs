//! Subscription image state and 4-byte collection-ID prefix stripping (EMB-4).
use super::{
    account::{AccountError, ClientAccount},
    codec::{Change, Group, RowChange, ValueChange},
};
use std::collections::BTreeMap;

const STORE_KEY_PREFIX_BYTES: usize = 4;

/// Subscription image state for one live query or room admission view.
pub(crate) struct SubImage {
    value: Option<Vec<u8>>,
    /// Map `(slot, stripped_key)` -> `row_value_bytes`.
    rows: BTreeMap<(u32, Vec<u8>), Vec<u8>>,
    /// Total image bytes currently charged to `ClientAccount`.
    image_charge: u64,
    completed: bool,
}

impl SubImage {
    pub(crate) fn new() -> Self {
        Self {
            value: None,
            rows: BTreeMap::new(),
            image_charge: 0,
            completed: false,
        }
    }

    pub(crate) fn apply_group(
        &mut self,
        group: &Group,
        is_complete: bool,
        account: &ClientAccount,
    ) -> Result<Option<Change>, AccountError> {
        let mut delta_bytes: i64 = 0;
        let replaced = is_complete;
        let mut old_rows = if replaced {
            Some(std::mem::take(&mut self.rows))
        } else {
            None
        };
        if let Some(prev) = &old_rows {
            for ((_, k), v) in prev {
                delta_bytes -= (k.len() + v.len() + 32) as i64;
            }
        }

        let mut value_change = None;
        let new_val_len = group.value.len();
        let old_val_len = self.value.as_ref().map_or(0, Vec::len);
        let value_changed = match &self.value {
            None => true,
            Some(prev) => prev.as_slice() != group.value.as_slice(),
        };
        if value_changed {
            delta_bytes += new_val_len as i64 - old_val_len as i64;
        }

        // Precompute row changes and net byte delta.
        let mut row_changes = Vec::with_capacity(group.rows.len());
        let mut staged: Vec<(u32, Vec<u8>, Option<Vec<u8>>)> = Vec::with_capacity(group.rows.len());
        for row in &group.rows {
            let slot = u32::from(row.slot);
            let stripped = if row.key.len() >= STORE_KEY_PREFIX_BYTES {
                row.key[STORE_KEY_PREFIX_BYTES..].to_vec()
            } else {
                row.key.clone()
            };
            let prev = if let Some(ref mut prev_map) = old_rows {
                prev_map.remove(&(slot, stripped.clone()))
            } else {
                self.rows.get(&(slot, stripped.clone())).cloned()
            };
            if prev.as_deref() == row.value.as_deref() {
                if let Some(v) = prev {
                    // Re-insert unchanged row into new map if `replaced`.
                    if replaced {
                        delta_bytes += (stripped.len() + v.len() + 32) as i64;
                        staged.push((slot, stripped, Some(v)));
                    }
                }
                continue;
            }
            if !replaced {
                if let Some(ref p) = prev {
                    delta_bytes -= (stripped.len() + p.len() + 32) as i64;
                }
            }
            if let Some(ref nv) = row.value {
                delta_bytes += (stripped.len() + nv.len() + 32) as i64;
            }
            row_changes.push(RowChange {
                slot,
                key: stripped.clone(),
                old: prev,
                new: row.value.clone(),
            });
            staged.push((slot, stripped, row.value.clone()));
        }
        // Any remaining rows in `old_rows` on `replaced` were deleted.
        if let Some(prev_map) = old_rows {
            for ((slot, key), old) in prev_map {
                row_changes.push(RowChange {
                    slot,
                    key,
                    old: Some(old),
                    new: None,
                });
            }
        }

        if delta_bytes > 0 {
            account.try_reserve(delta_bytes as u64)?;
            self.image_charge += delta_bytes as u64;
        } else if delta_bytes < 0 {
            let refund = (-delta_bytes) as u64;
            let actual = refund.min(self.image_charge);
            account.refund(actual);
            self.image_charge -= actual;
        }

        if value_changed {
            let old = self.value.replace(group.value.clone());
            // For an empty initial completion with empty value, keep `value: None` if value is empty and first?
            // Spec: "A Change with replaced and empty rows and value is the first completion of an empty image."
            // When `group.value` is non-empty OR `old` was Some, report ValueChange.
            if !group.value.is_empty() || old.is_some() {
                value_change = Some(ValueChange {
                    old,
                    new: group.value.clone(),
                });
            }
        }
        for (slot, key, val) in staged {
            match val {
                Some(v) => {
                    self.rows.insert((slot, key), v);
                }
                None => {
                    self.rows.remove(&(slot, key));
                }
            }
        }
        let first_complete = replaced && !self.completed;
        if replaced {
            self.completed = true;
        }
        if first_complete || replaced || value_change.is_some() || !row_changes.is_empty() {
            Ok(Some(Change {
                invalidated: false,
                replaced,
                value: value_change,
                rows: row_changes,
            }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn clear_for_reset(&mut self, account: &ClientAccount) -> Change {
        self.value = None;
        self.rows.clear();
        self.completed = false;
        if self.image_charge > 0 {
            account.refund(self.image_charge);
            self.image_charge = 0;
        }
        Change {
            invalidated: true,
            replaced: false,
            value: None,
            rows: Vec::new(),
        }
    }

    pub(crate) fn release(&mut self, account: &ClientAccount) {
        self.value = None;
        self.rows.clear();
        if self.image_charge > 0 {
            account.refund(self.image_charge);
            self.image_charge = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_image_strips_4_byte_prefix_and_resets() {
        let acc = ClientAccount::new(super::super::account::PROGRESS_RESERVE + 4096).unwrap();
        let mut img = SubImage::new();
        let group = Group {
            sub: 1,
            value: vec![1],
            rows: vec![super::super::codec::Row {
                slot: 0,
                key: vec![0, 0, 0, 3, 0xaa, 0xbb],
                value: Some(vec![0xcc]),
            }],
        };
        let ch = img.apply_group(&group, true, &acc).unwrap().unwrap();
        assert!(ch.replaced);
        assert!(!ch.invalidated);
        assert_eq!(ch.rows.len(), 1);
        assert_eq!(ch.rows[0].key, vec![0xaa, 0xbb]);
        assert_eq!(ch.rows[0].new, Some(vec![0xcc]));

        let reset = img.clear_for_reset(&acc);
        assert!(reset.invalidated);
        assert!(!reset.replaced);
        assert!(reset.rows.is_empty());
        assert_eq!(acc.admissible_used(), 0);
    }
}
