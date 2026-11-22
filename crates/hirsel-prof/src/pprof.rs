use std::collections::{BTreeMap, HashMap};
use std::io::{BufWriter, Write};
use std::path::Path;

use rusqlite::Connection;

use crate::profile::{Profile, Result, Unsigned};

/// Write the standard pprof Profile protobuf. pprof accepts an uncompressed
/// message as well as a gzip envelope.
pub fn export_pprof(input: &Path, output: &Path) -> Result<()> {
    let profile = Profile::open(input)?;
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
    let mut scratch = tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new(".")))?;
    {
        let mut stream = BufWriter::new(scratch.as_file_mut());
        encode(profile.connection(), &mut stream)?;
        stream.flush()?;
    }
    scratch.as_file().sync_all()?;
    scratch.persist_noclobber(output)?;
    Ok(())
}

fn encode(db: &Connection, out: &mut impl Write) -> Result<()> {
    let primary: String = db.query_row(
        "SELECT value FROM run_meta WHERE key='primary_event'",
        [],
        |r| r.get(0),
    )?;
    let unit: String = db.query_row(
        "SELECT value FROM run_meta WHERE key='primary_unit'",
        [],
        |r| r.get(0),
    )?;
    let duration: Unsigned = db.query_row(
        "SELECT value FROM run_meta WHERE key='duration_ns'",
        [],
        |r| r.get(0),
    )?;

    let mut strings = Strings::default();
    let mut events = BTreeMap::from([(primary.clone(), 0)]);
    let mut counters = db.prepare("SELECT DISTINCT event FROM sample_counter ORDER BY event")?;
    for event in counters.query_map([], |row| row.get::<_, String>(0))? {
        let event = event?;
        let next = events.len();
        events.entry(event).or_insert(next);
    }
    let mut sample_types = events.iter().collect::<Vec<_>>();
    sample_types.sort_by_key(|(_, index)| **index);
    let mut event_kind = Vec::new();
    for (event, index) in &sample_types {
        let event_unit = if **index == 0 {
            unit.as_str()
        } else if event.as_str() == "cpu-clock" {
            "nanoseconds"
        } else {
            event
        };
        let message = value_type(strings.id(event), strings.id(event_unit));
        if **index == 0 {
            event_kind = message.clone();
        }
        field(out, 1, &message)?;
    }
    field(out, 11, &event_kind)?;
    number(out, 10, i64::try_from(duration.0)? as u64)?;
    number(out, 14, strings.id(&primary))?;

    let mut function_sources = db.prepare(
        "SELECT f.id,f.name,f.instance_label,
                (SELECT fr.file FROM frame fr WHERE fr.function_id=f.id AND fr.file IS NOT NULL
                 ORDER BY fr.file,fr.line LIMIT 1)
         FROM function f ORDER BY f.id",
    )?;
    let mut rows = function_sources.query([])?;
    while let Some(row) = rows.next()? {
        let mut message = Vec::new();
        number(&mut message, 1, row.get::<_, i64>(0)?.try_into()?)?;
        number(&mut message, 2, strings.id(&row.get::<_, String>(1)?))?;
        number(&mut message, 3, strings.id(&row.get::<_, String>(2)?))?;
        if let Some(file) = row.get::<_, Option<String>>(3)? {
            number(&mut message, 4, strings.id(&file))?;
        }
        field(out, 5, &message)?;
    }

    let mut mappings = HashMap::new();
    let mut statement = db.prepare(
        "SELECT id,start_addr_hex,end_addr_hex,file_offset,path,build_id,
          EXISTS(SELECT 1 FROM frame WHERE mapping_id=m.id AND function_id IS NOT NULL),
          EXISTS(SELECT 1 FROM frame WHERE mapping_id=m.id AND file IS NOT NULL),
          EXISTS(SELECT 1 FROM frame WHERE mapping_id=m.id AND line IS NOT NULL),
          EXISTS(SELECT 1 FROM frame WHERE mapping_id=m.id AND inlined=1)
        FROM mapping m ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let start = address(&row.get::<_, String>(1)?)?;
        let end = address(&row.get::<_, String>(2)?)?;
        mappings.insert(id, start);
        let mut message = Vec::new();
        number(&mut message, 1, id.try_into()?)?;
        number(&mut message, 2, start)?;
        number(&mut message, 3, end)?;
        number(&mut message, 4, row.get::<_, Unsigned>(3)?.0)?;
        number(&mut message, 5, strings.id(&row.get::<_, String>(4)?))?;
        if let Some(id) = row.get::<_, Option<String>>(5)? {
            number(&mut message, 6, strings.id(&id))?;
        }
        for field in 7..=10 {
            if row.get::<_, bool>(field - 1)? {
                number(&mut message, field as u64, 1)?;
            }
        }
        field(out, 3, &message)?;
    }

    let mut locations = Locations {
        mapping: mappings,
        ids: HashMap::new(),
    };
    let mut site = db.prepare(
        "SELECT c.parent_id,fr.id,fr.mapping_id,fr.rel_pc,fr.function_id,fr.line,fr.col,fr.inlined
         FROM callsite c JOIN frame fr ON fr.id=c.frame_id WHERE c.id=?1",
    )?;
    let mut counters =
        db.prepare("SELECT event,value FROM sample_counter WHERE sample_id=?1 ORDER BY event")?;
    let mut statement =
        db.prepare("SELECT id,callsite_id,raw_pc_hex FROM cpu_sample ORDER BY id")?;
    let mut samples = statement.query([])?;
    while let Some(row) = samples.next()? {
        let id: i64 = row.get(0)?;
        let highest: i64 = row.get(1)?;
        let leaf_pc = address(&row.get::<_, String>(2)?)?;
        let mut pc = Some(leaf_pc);
        let mut current = Some(highest);
        let mut refs = Vec::new();
        let mut frames = Vec::new();
        while let Some(id) = current {
            let frame = site.query_row([id], |row| {
                Ok(Frame {
                    parent: row.get(0)?,
                    id: row.get(1)?,
                    mapping: row.get(2)?,
                    offset: row.get::<_, Option<Unsigned>>(3)?.map(|n| n.0),
                    function: row.get(4)?,
                    line: row.get(5)?,
                    col: row.get(6)?,
                    inlined: row.get(7)?,
                })
            })?;
            current = frame.parent;
            if current.is_some_and(|parent| parent >= id) {
                return Err("invalid callsite chain in .hprof".into());
            }
            let inlined = frame.inlined;
            frames.push(frame);
            if !inlined || current.is_none() {
                refs.push(locations.intern(out, &frames, pc.take().unwrap_or_default())?);
                frames.clear();
            }
        }
        let mut weights = vec![0; events.len()];
        let mut values = counters.query([id])?;
        while let Some(value) = values.next()? {
            let name: String = value.get(0)?;
            weights[events[&name]] = i64::try_from(value.get::<_, Unsigned>(1)?.0)? as u64;
        }
        let mut message = Vec::new();
        packed(&mut message, 1, &refs)?;
        packed(&mut message, 2, &weights)?;
        field(out, 2, &message)?;
    }
    for s in strings.ordered {
        field(out, 6, s.as_bytes())?;
    }
    Ok(())
}

fn address(hex: &str) -> Result<u64> {
    Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16)?)
}

struct Strings {
    ids: HashMap<String, u64>,
    ordered: Vec<String>,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            ids: HashMap::from([(String::new(), 0)]),
            ordered: vec![String::new()],
        }
    }
}

impl Strings {
    fn id(&mut self, string: &str) -> u64 {
        if let Some(id) = self.ids.get(string) {
            return *id;
        }
        let id = self.ordered.len() as u64;
        self.ordered.push(string.to_owned());
        self.ids.insert(string.to_owned(), id);
        id
    }
}

struct Frame {
    parent: Option<i64>,
    id: i64,
    mapping: Option<i64>,
    offset: Option<u64>,
    function: Option<i64>,
    line: Option<i64>,
    col: Option<i64>,
    inlined: bool,
}

struct Locations {
    mapping: HashMap<i64, u64>,
    ids: HashMap<(u64, Vec<i64>), u64>,
}

impl Locations {
    fn intern(&mut self, out: &mut impl Write, frames: &[Frame], fallback: u64) -> Result<u64> {
        let first = &frames[0];
        let address = match (first.mapping, first.offset) {
            (Some(mapping), Some(offset)) => self.mapping[&mapping]
                .checked_add(offset)
                .ok_or("profile location address overflows")?,
            _ => fallback,
        };
        let key = (address, frames.iter().map(|frame| frame.id).collect());
        if let Some(id) = self.ids.get(&key) {
            return Ok(*id);
        }
        let id = self.ids.len() as u64 + 1;
        let mut message = Vec::new();
        number(&mut message, 1, id)?;
        if let Some(mapping) = first.mapping {
            number(&mut message, 2, mapping.try_into()?)?;
        }
        number(&mut message, 3, address)?;
        for frame in frames {
            if let Some(function) = frame.function {
                let mut line = Vec::new();
                number(&mut line, 1, function.try_into()?)?;
                if let Some(source_line) = frame.line {
                    number(&mut line, 2, source_line.try_into()?)?;
                }
                if let Some(col) = frame.col {
                    number(&mut line, 3, col.try_into()?)?;
                }
                field(&mut message, 4, &line)?;
            }
        }
        field(out, 4, &message)?;
        self.ids.insert(key, id);
        Ok(id)
    }
}

fn value_type(ty: u64, unit: u64) -> Vec<u8> {
    let mut value = Vec::new();
    number(&mut value, 1, ty).expect("Vec write");
    number(&mut value, 2, unit).expect("Vec write");
    value
}

fn varint(out: &mut impl Write, mut n: u64) -> std::io::Result<()> {
    let mut bytes = [0u8; 10];
    let mut count = 0;
    while n >= 0x80 {
        bytes[count] = n as u8 | 0x80;
        n >>= 7;
        count += 1;
    }
    bytes[count] = n as u8;
    out.write_all(&bytes[..=count])
}

fn number(out: &mut impl Write, tag: u64, n: u64) -> std::io::Result<()> {
    varint(out, tag << 3)?;
    varint(out, n)
}

fn field(out: &mut impl Write, tag: u64, data: &[u8]) -> std::io::Result<()> {
    varint(out, (tag << 3) | 2)?;
    varint(out, data.len() as u64)?;
    out.write_all(data)
}

fn packed(out: &mut impl Write, tag: u64, numbers: &[u64]) -> std::io::Result<()> {
    let mut data = Vec::new();
    for &n in numbers {
        varint(&mut data, n)?;
    }
    field(out, tag, &data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProfileWriter, RunMeta};

    #[test]
    fn exports_one_physical_location_with_ordered_inline_frames_and_period() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("profile.hprof");
        let export = dir.path().join("profile.pb");
        let writer = ProfileWriter::create(&file).unwrap();
        writer
            .connection()
            .execute_batch(
                "INSERT INTO thread VALUES (1,1,1,'main',0,NULL);
             INSERT INTO mapping VALUES (1,'0x1000','0x2000',0,'/fixture','abcd',NULL,0,NULL);
             INSERT INTO function VALUES (1,'app/outer','outer@x','outer','hirsel'),
                                         (2,'app/inline','inline@x','inline','hirsel');
             INSERT INTO frame VALUES (1,1,32,1,'fixture.hirsel',10,1,0),
                                      (2,1,32,2,'fixture.hirsel',13,3,1);
             INSERT INTO callsite VALUES (1,NULL,1,0), (2,1,2,1);
             INSERT INTO cpu_sample VALUES (1,1,1,NULL,0,2,'0x1020',23,'fiber','fiber_root');
             INSERT INTO sample_counter VALUES (1,'instructions',23);",
            )
            .unwrap();
        writer
            .finish(&RunMeta {
                capture_mode: "run",
                target: "x86_64-unknown-linux-gnu",
                cmdline: "[]",
                build_id: "abcd",
                image_digest: "",
                symbol_tier: "full_dwarf",
                clock: "CLOCK_MONOTONIC",
                primary_event: "instructions",
                primary_unit: "instructions",
                freq_hz: 997,
                fallback_reason: "",
                start_ns: 0,
                duration_ns: 25,
                samples_lost: 0,
            })
            .unwrap();
        export_pprof(&file, &export).unwrap();
        let encoded = std::fs::read(export).unwrap();

        // profile.proto uses field 4 for Location. Its Address is 0x1020
        // and Lines appear innermost first. Both share this location ID.
        let location = [
            0x22, 0x17, 0x08, 1, 0x10, 1, 0x18, 0xa0, 0x20, 0x22, 6, 0x08, 2, 0x10, 13, 0x18, 3,
            0x22, 6, 0x08, 1, 0x10, 10, 0x18, 1,
        ];
        assert!(encoded.windows(location.len()).any(|part| part == location));
        // Sample { location_id: [1], value: [23] } in field 2.
        let sample = [0x12, 6, 0x0a, 1, 1, 0x12, 1, 23];
        assert!(encoded.windows(sample.len()).any(|part| part == sample));
    }
}
