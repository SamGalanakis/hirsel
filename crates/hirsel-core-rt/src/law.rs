//! Recording-build verdicts, owned and written by the host outside program accounting.

use hirsel_core_layout::{case_of, law::*};
use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

mod model;

static ACTIVE: Mutex<Option<Observation>> = Mutex::new(None);

struct Observation {
    calls: BTreeMap<usize, ActiveCall>,
    rows: Vec<Row>,
    witnesses: BTreeMap<String, u64>,
}

struct ActiveCall {
    depth: usize,
    settling_depth: usize,
    callee: u64,
    site: u64,
    bounds: [u64; 4],
}

struct Row {
    callee: u64,
    site: u64,
    outcome: Outcome,
}

enum Outcome {
    Prop(Prop),
    Model(model::Verdict),
}

struct Prop {
    bounds: [u64; 4],
    class: &'static str,
    first: u64,
    second: u64,
    failure: u64,
}

pub(crate) struct Run(PathBuf, *const crate::abi_sites::SourceTable);

impl Run {
    /// # Safety
    /// Call before threads start and before any environment grant is snapshotted.
    /// The launcher pins the source table through the returned guard.
    pub(crate) unsafe fn start(sources: *const crate::abi_sites::SourceTable) -> Option<Self> {
        let path = std::env::var_os("HIRSEL_LAWS");
        // SAFETY: caller starts observation at single-threaded process entry.
        unsafe { std::env::remove_var("HIRSEL_LAWS") };
        let path = path?;
        *ACTIVE.lock().unwrap() = Some(Observation {
            calls: BTreeMap::new(),
            rows: Vec::new(),
            witnesses: BTreeMap::new(),
        });
        Some(Self(path.into(), sources))
    }

    fn write(&self, observation: &Observation) -> std::io::Result<()> {
        let rows = &observation.rows;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.0)?;
        let mut output = BufWriter::new(file);
        writeln!(output, "hirsel-laws-observation 2")?;
        for row in rows {
            let name = CALLEES[row.callee as usize];
            // SAFETY: startup pins the source table until this guard finishes.
            let location = unsafe { crate::abi_sites::format_site(row.site, self.1) };
            let family = match &row.outcome {
                Outcome::Prop(_) => "row",
                Outcome::Model(_) => "model",
            };
            write!(output, "{family}\t{name}\t")?;
            write_hex(&mut output, location.as_deref().unwrap_or_default())?;
            match &row.outcome {
                Outcome::Prop(prop) => {
                    let [cases, max_choices, max_rejects, gen_steps] = prop.bounds;
                    writeln!(
                        output,
                        "\t{}\t{}\t{}\t{}\t{cases}\t{max_choices}\t{max_rejects}\t{gen_steps}",
                        prop.class, prop.first, prop.second, prop.failure
                    )?;
                }
                Outcome::Model(verdict) => verdict.write(&mut output)?,
            }
        }
        for (label, count) in &observation.witnesses {
            write!(output, "witness\t{count}\t")?;
            write_hex(&mut output, label)?;
            writeln!(output)?;
        }
        writeln!(output, "end")?;
        output.flush()
    }
}

fn write_hex(output: &mut impl Write, text: &str) -> std::io::Result<()> {
    for byte in text.bytes() {
        write!(output, "{byte:02x}")?;
    }
    Ok(())
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(observation) = ACTIVE.lock().unwrap().take() {
            let _ = self.write(&observation);
        }
    }
}

/// Read a witness label while its loan is live. Only copied label bytes and the
/// observed count can outlive this hook.
///
/// # Safety
/// `label` borrows an initialized String carrier and its UTF-8 bytes through the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_law_witness(label: *const u8, cond: u8) {
    let mut active = ACTIVE.lock().unwrap();
    let Some(observation) = active.as_mut() else {
        return;
    };
    // SAFETY: the caller lends a live String through this hook.
    let label = unsafe { string_bytes(label) };
    if let Some(count) = observation.witnesses.get_mut(label) {
        if let Some(next) = count.checked_add(u64::from(cond != 0)) {
            *count = next;
        } else {
            *active = None;
        }
    } else {
        observation
            .witnesses
            .insert(label.to_owned(), u64::from(cond != 0));
    }
}

/// The String carrier and its UTF-8 bytes must remain live for `'a`.
unsafe fn string_bytes<'a>(label: *const u8) -> &'a str {
    // SAFETY: String uses the native sequence carrier (data pointer, byte length, capacity).
    unsafe {
        let len = label.add(8).cast::<usize>().read_unaligned();
        if len == 0 {
            ""
        } else {
            let bytes = label.cast::<*const u8>().read_unaligned();
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(bytes, len))
        }
    }
}

fn failure(read: &impl Fn(u64, u64) -> u64, base: u64, fields: &[u64]) -> (u64, u64) {
    let tag = tag_from_words(&fields[2..]);
    let class = base + fields[1];
    let index = case_of(&tag, read(class + tag.offset, tag.width));
    // Preserve the concrete cause kind when the engine classified a failure at a site.
    let index = if index == 2 {
        (u64::from(index) << 8) | read(class + fields[2 + TAG_WORDS], 1)
    } else {
        u64::from(index)
    };
    (read(base + fields[0], 8), index)
}

/// Borrow the selected value only for the native hook. An outer property call owns
/// the slot for its execution context until it leaves, even across task switches.
///
/// # Safety
/// For entry or outcome hooks `words` pins `len` compiler-generated descriptor
/// words, and each offset of the active variant points to initialized bytes in
/// `value`. Both borrows end on return. The context pointer is an identity only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_law_observe(
    context: *const crate::Context,
    operation: u64,
    callee: u64,
    site: u64,
    value: *const u8,
    words: *const u64,
    len: usize,
) {
    let mut active = ACTIVE.lock().unwrap();
    let Some(observation) = active.as_mut() else {
        return;
    };
    let context = context as usize;
    if operation == LEAVE {
        let call = observation.calls.get_mut(&context).unwrap();
        call.depth -= 1;
        if call.depth == 0 {
            observation.calls.remove(&context);
        }
        return;
    }
    if operation == DELEGATE {
        if let Some(call) = observation.calls.get_mut(&context)
            && call.depth == 1
        {
            call.settling_depth = 2;
        }
        return;
    }
    // SAFETY: the native emitter pins exactly these initialized words through the hook.
    let words = unsafe { std::slice::from_raw_parts(words, len) };
    let read = |offset: u64, width: u64| {
        let mut bytes = [0u8; 8];
        // SAFETY: the recipe selected for the active case names only initialized bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                value.add(offset as usize),
                bytes.as_mut_ptr(),
                width as usize,
            )
        };
        u64::from_le_bytes(bytes)
    };
    if operation == ENTER {
        observation
            .calls
            .entry(context)
            .and_modify(|call| call.depth += 1)
            .or_insert_with(|| ActiveCall {
                depth: 1,
                settling_depth: 1,
                callee,
                site,
                bounds: std::array::from_fn(|i| read(words[i], 8)),
            });
        return;
    }
    let Some(call) = observation.calls.get(&context) else {
        return;
    };
    if let Some(engine) = model_engine(call.callee) {
        if operation == OUTCOME && callee == engine && call.depth == call.settling_depth {
            // SAFETY: this is the returned model outcome and the matching layout descriptor.
            let verdict = unsafe { model::Verdict::read(value, words, call.bounds, &read) };
            observation.rows.push(Row {
                callee: call.callee,
                site: call.site,
                outcome: Outcome::Model(verdict),
            });
            // The wrapper may run user code after its delegated verdict settles.
            observation.calls.get_mut(&context).unwrap().settling_depth = 1;
        }
        return;
    }
    let outer_outcome = operation == OUTCOME && call.depth == 1;
    let outer_check =
        operation != OUTCOME && call.callee >= CHECK && call.depth == call.settling_depth;
    if !outer_outcome && !outer_check {
        return;
    }
    let (class, first, second, failure) = if operation == CORPUS_FAILURE {
        let (cases, class) = failure(&read, 0, words);
        ("falsified", cases, 0, class)
    } else {
        let exhaustive = call.callee == ALL;
        let tag = tag_from_words(words);
        let index = case_of(&tag, read(tag.offset, tag.width)) as usize;
        let pairs = if exhaustive { 4 } else { 3 };
        let offset = TAG_WORDS + 2 * index;
        let (a, b) = (words[offset], words[offset + 1]);
        if index + 1 == pairs {
            let (cases, class) = failure(&read, a, &words[TAG_WORDS + 2 * pairs..]);
            ("falsified", cases, 0, class)
        } else {
            let first = read(a, 8);
            let second = if b == ABSENT { 0 } else { read(b, 8) };
            let class = match (exhaustive, index, first) {
                (_, 0, 0) => "vacuous",
                (true, 0, _) => "held",
                (true, 1, _) => "truncated",
                (true, 2, _) => "unbounded",
                (false, 0, _) => "sampled",
                (false, 1, _) => "gave up",
                _ => unreachable!(),
            };
            (class, first, second, ABSENT)
        }
    };
    observation.rows.push(Row {
        callee: call.callee,
        site: call.site,
        outcome: Outcome::Prop(Prop {
            bounds: call.bounds,
            class,
            first,
            second,
            failure,
        }),
    });
}
