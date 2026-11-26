use super::*;

#[test]
fn every_memory_sample_admission_refusal_matches_the_evaluator() {
    let mut executions = 0;
    for source in [
        include_str!("../../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../../hirsel-core-ir/samples/variants.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let mut cases = Vec::new();
        for case in sample_cases(&module, source) {
            let baseline = eval::run(&module, case.function, &case.args, case.limit);
            for nth in 1..=baseline.admissions {
                for later in [false, true] {
                    cases.push(Case {
                        function: case.function,
                        args: case.args.clone(),
                        limit: case.limit,
                        injection: Some((nth, later)),
                    });
                }
            }
            cases.push(case);
        }
        executions += cases.len();
        agree_cases(&module, &cases, true);
    }
    // Each run contributes one baseline plus two injections per admission attempt.
    // Reading the sample paths gives (runs, attempts): admission (11, 27),
    // cleanup (13, 26), counted (2, 4), failure (5, 4), variants (7, 10).
    // Both admission f2 runs attempt the initial allocation, bound entry (MEM-25)
    // and growth (MEM-26). The 16-byte bound refuses growth, the 32-byte bound
    // admits it. Replacing the old single f2 run with those two adds seven cases.
    assert_eq!(executions, 180);
    println!("whole memory sample executions including every admission refusal: {executions}");
}
