//! The logical Exec profile snapshot used by GRANT and capability snapshots.
use super::{Corrupt, Cursor};
use hirsel_core_net::exec::{ArgToken, ExecRules, Form, ProfileRules};
use std::collections::{BTreeMap, BTreeSet};

pub fn exec_snapshot_rules(bytes: &[u8]) -> Result<ExecRules, Corrupt> {
    let mut c = Cursor::new(bytes);
    let mut result = BTreeMap::<String, ProfileRules>::new();
    let profiles = read_count(&mut c)?;
    if profiles == 0 {
        return Err(Corrupt);
    }
    for _ in 0..profiles {
        let name = read_name(&mut c)?;
        if result
            .last_key_value()
            .is_some_and(|(last, _)| last.as_str() >= name)
        {
            return Err(Corrupt);
        }
        let _: [u8; 32] = c.fixed()?;
        read_str(&mut c)?;
        let mut forms = Vec::new();
        for _ in 0..read_count(&mut c)? {
            let mut tokens = Vec::new();
            for _ in 0..read_count(&mut c)? {
                tokens.push(match c.u8()? {
                    0 => ArgToken::Lit(read_field(&mut c)?.to_vec()),
                    1 => ArgToken::Any,
                    2 => ArgToken::Rest,
                    _ => return Err(Corrupt),
                });
            }
            forms.push(Form::new(tokens).ok_or(Corrupt)?);
        }
        let mut keys = BTreeSet::new();
        let mut fixed = BTreeSet::new();
        let mut last = None;
        for _ in 0..read_count(&mut c)? {
            let key = read_str(&mut c)?;
            if !hirsel_core_net::exec::valid_key(key) || last.is_some_and(|last| last >= key) {
                return Err(Corrupt);
            }
            last = Some(key);
            let group = match c.u8()? {
                0 => &mut fixed,
                1 => &mut keys,
                _ => return Err(Corrupt),
            };
            group.insert(key.to_owned());
        }
        let mut dirs = BTreeSet::<String>::new();
        for _ in 0..read_count(&mut c)? {
            let dir = read_name(&mut c)?;
            if dirs.last().is_some_and(|last| last.as_str() >= dir) {
                return Err(Corrupt);
            }
            dirs.insert(dir.to_owned());
        }
        if dirs.is_empty() {
            return Err(Corrupt);
        }
        result.insert(
            name.to_owned(),
            ProfileRules::host(forms, keys, dirs, fixed),
        );
    }
    if !c.done() {
        return Err(Corrupt);
    }
    Ok(ExecRules::host(result))
}

fn read_count(c: &mut Cursor<'_>) -> Result<usize, Corrupt> {
    Ok(u32::from_le_bytes(c.fixed()?) as usize)
}

fn read_field<'a>(c: &mut Cursor<'a>) -> Result<&'a [u8], Corrupt> {
    let n = read_count(c)?;
    Ok(c.take(n)?)
}

fn read_str<'a>(c: &mut Cursor<'a>) -> Result<&'a str, Corrupt> {
    let bytes = read_field(c)?;
    if bytes.contains(&0) {
        return Err(Corrupt);
    }
    std::str::from_utf8(bytes).map_err(|_| Corrupt)
}

fn read_name<'a>(c: &mut Cursor<'a>) -> Result<&'a str, Corrupt> {
    let name = read_str(c)?;
    if name.is_empty() || name.contains(',') {
        return Err(Corrupt);
    }
    Ok(name)
}
