#[derive(Clone, Copy)]
enum NativeCheck {
    Exact,
    #[allow(dead_code)]
    Source,
    #[allow(dead_code)]
    Memcheck,
}

type ReportWords<'a> = (&'a [u64], u64, &'a [u64], u64);
fn take_report<'a>(words: &mut &'a [u64]) -> Option<ReportWords<'a>> {
    let count = usize::try_from(*words.get(1)?).ok()?;
    if *words.first()? == 2 {
        let end = count.checked_add(2)?;
        let outcome = words.get(..end)?;
        *words = words.get(end..)?;
        return Some((outcome, 0, &[], 0));
    }
    let payload = count.checked_mul(if *words.first()? == 0 { 1 } else { 3 })?;
    let used_at = payload.checked_add(2)?;
    let trace_at = used_at.checked_add(1)?;
    let trace = usize::try_from(*words.get(trace_at)?).ok()?;
    let trace_end = trace_at.checked_add(1)?.checked_add(trace)?;
    let outcome = words.get(..used_at)?;
    let used = *words.get(used_at)?;
    let trace = words.get(trace_at + 1..trace_end)?;
    let admissions = *words.get(trace_end)?;
    *words = words.get(trace_end + 1..)?;
    Some((outcome, used, trace, admissions))
}

fn reports_agree(check: NativeCheck, got: Option<ReportWords<'_>>, want: ReportWords<'_>) -> bool {
    match (check, got) {
        (NativeCheck::Source, Some((outcome, used, traces, _))) => {
            // MEM-28 permits the native build to elide admissions. The
            // outcome, sites and finalizer trace still have to agree.
            outcome == want.0 && used <= want.1 && traces == want.2
        }
        (_, got) => got == Some(want),
    }
}

fn execute_reports(
    input: &impl NativeInput,
    runs: &[(FuncId, Vec<u64>)],
    optimize: bool,
    limit: u64,
    injection: Option<(usize, bool)>,
    reports: Option<&[eval::Report]>,
) {
    execute_reports_checked(
        input,
        runs,
        optimize,
        limit,
        injection,
        reports,
        NativeCheck::Exact,
    );
}

fn execute_reports_checked(
    input: &impl NativeInput,
    runs: &[(FuncId, Vec<u64>)],
    optimize: bool,
    limit: u64,
    injection: Option<(usize, bool)>,
    reports: Option<&[eval::Report]>,
    check: NativeCheck,
) {
    let cases: Vec<_> = runs
        .iter()
        .enumerate()
        .map(|(i, (function, args))| NativeCase {
            function: *function,
            args,
            limit,
            injection,
            report: reports.map(|reports| &reports[i]),
        })
        .collect();
    execute_cases_with_bindings(input, optimize, &cases, false, "", check);
}

struct NativeCase<'a> {
    function: FuncId,
    args: &'a [u64],
    limit: u64,
    injection: Option<(usize, bool)>,
    report: Option<&'a eval::Report>,
}

fn execute_cases(input: &impl NativeInput, optimize: bool, cases: &[NativeCase<'_>]) {
    execute_cases_with_rows(input, optimize, cases, false);
}

fn execute_cases_with_rows(
    input: &impl NativeInput,
    optimize: bool,
    cases: &[NativeCase<'_>],
    debug_lines: bool,
) {
    execute_cases_with_bindings(input, optimize, cases, debug_lines, "", NativeCheck::Exact);
}
