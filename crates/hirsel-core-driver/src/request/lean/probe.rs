use super::*;

/// What extracting one function refuses: the extraction itself, over the function and the
/// functions of its file it calls (directly or not), so a probe with no refusals extracts.
pub(super) fn probe(product: &CheckProduct, path: &DefPath) -> R<()> {
    let full = path.to_string();
    let (prefix, name) = full
        .rsplit_once('/')
        .ok_or_else(|| format!("{path}: not a definition path"))?;
    let (_, file) = item_of(product, path).ok_or_else(|| format!("{path}: no syntax item"))?;
    let mut names = vec![name.to_owned()];
    let mut i = 0;
    while i < names.len() {
        let at = DefPath::from_full(&format!("{prefix}/{}", names[i]))
            .ok_or_else(|| format!("{}: bad path", names[i]))?;
        i += 1;
        let def = product
            .definitions
            .get(&at)
            .ok_or_else(|| format!("{at}: not a checked function"))?;
        let ir = def.ir().map_err(|e| e.to_string())?;
        for g in &ir.functions {
            let Some(body) = &g.body else { continue };
            if ir.instances[g.instance.index()].definition != at {
                continue;
            }
            for call in &body.calls {
                let Callee::Direct(c) = call.callee else {
                    continue;
                };
                let callee = &ir.instances[ir.functions[c.index()].instance.index()].definition;
                let callee_path = callee.to_string();
                let Some(rest) = callee_path.strip_prefix(&format!("{prefix}/")) else {
                    continue;
                };
                let same_file_fn = item_of(product, callee).is_some_and(|(item, fi)| {
                    fi == file && Src { item }.kind(item.root) == K::Function
                });
                if same_file_fn && !names.iter().any(|n| n == rest) {
                    names.push(rest.to_owned());
                }
            }
        }
    }
    let extracted = extract(product, prefix, &names)?;
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &extracted.refusals {
        // Group an exclusion repeated at several spans.
        let reason = r
            .rsplit_once(" at ")
            .filter(|(_, at)| !at.contains(' '))
            .map_or(r.as_str(), |x| x.0);
        *reasons.entry(reason).or_default() += 1;
    }
    println!("{path}: {} refusals", extracted.refusals.len());
    for (r, n) in reasons {
        println!("  {n} x {r}");
    }
    Ok(())
}
