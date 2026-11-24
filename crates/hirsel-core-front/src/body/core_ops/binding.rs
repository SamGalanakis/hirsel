use super::*;

pub(crate) fn binding(f: &FnDecl) -> Option<u16> {
    if f.path.package_is("runner", "1")
        && f.path.0.modules == ["runner"]
        && matches!(
            f.path.0.name.as_str(),
            "launch_word" | "launch_text" | "launch_emit" | "launch_socket" | "launch_scratch"
        )
    {
        return Some(match f.path.0.name.as_str() {
            "launch_word" => u16::MAX,
            "launch_text" => u16::MAX - 1,
            "launch_emit" => u16::MAX - 2,
            "launch_socket" => u16::MAX - 3,
            _ => u16::MAX - 4,
        });
    }
    if !f.path.package_is("core", "1") || f.body.is_some() {
        return None;
    }
    let [module] = f.path.0.modules.as_slice() else {
        return None;
    };
    // Each bound operation's first row, by its core module and name.
    static BOUND: std::sync::OnceLock<rustc_hash::FxHashMap<(&str, &str), u16>> =
        std::sync::OnceLock::new();
    let bound = BOUND.get_or_init(|| {
        let mut bound = rustc_hash::FxHashMap::default();
        for (i, entry) in OPERATIONS.iter().enumerate() {
            if matches!(
                entry.lane,
                Lane::Runtime(_) | Lane::PendingCode | Lane::Simd(_) | Lane::Charge(_)
            ) {
                bound.entry((entry.module, entry.name)).or_insert(i as u16);
            }
        }
        bound
    });
    bound
        .get(&(module.as_str(), f.path.0.name.as_str()))
        .copied()
}

pub(in crate::body) fn simd_call(row: Option<u16>) -> Option<SimdCall> {
    match OPERATIONS.get(row? as usize)?.lane {
        Lane::Simd(op) => Some(op),
        _ => None,
    }
}
