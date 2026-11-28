fn check_unaligned_copies() {
    let mut source = String::from(
        "core v1\ntype t0 = own \"Buffer\" { data: array 131 u8 }\nstatic d0 = align 1 hex fe0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f8081ef\nsig g0 = (u64) -> (u64, u8, u8)\nsig g1 = (u64, u64, u64) -> ()\n",
    );
    for size in 0..=130 {
        writeln!(source, "inst n{size} = test@0/small_copy/f{size} [] []").unwrap();
    }
    for size in 0..=130 {
        let length = if size == 130 {
            "wrapping_add v0, v2".into()
        } else {
            format!("const {size}")
        };
        writeln!(
            source,
            "fn f{size} = n{size} g0 {{
  slot s0 = t0
  addr a0 = s0 byte v8 : u8
  addr a1 = s0 byte v3 : u8
  addr a2 = v7 byte v3 : u8
  addr a3 = s0 byte v13 : u8
  addr a4 = s0 byte v2 : u8
  addr a5 = s0 byte v4 : u8
b0(v0: u64):
  live s0
  v2: u64 = const 0
  v1: u64 = {length}
  v3: u64 = const 1
  v4: u64 = const 130
  v5: u64 = const 131
  v6: u8 = const 0
  v7: ptr = static d0
  v24: u64 = const 12
  call rt.work_steps g1 (v5, v24, v2)
  jump b1(v2)
b1(v8: u64):
  v9: bool = lt v8, v5
  branch v9, b2, b3
b2:
  address a0
  store a0, v6
  v10: u64 = wrapping_add v8, v3
  jump b1(v10)
b3:
  v11: u8 = const 97
  v12: u8 = const 101
  address a4
  address a5
  store a4, v11
  store a5, v12
  address a1
  address a2
  copy a1, a2, v1
  jump b4(v2, v2)
b4(v13: u64, v14: u64):
  v15: bool = lt v13, v5
  branch v15, b5, b6
b5:
  address a3
  v16: u8 = read a3
  v17: u64 = widen v16
  v18: u64 = const 257
  v19: u64 = wrapping_mul v14, v18
  v20: u64 = wrapping_add v19, v17
  v21: u64 = wrapping_add v13, v3
  jump b4(v21, v20)
b6:
  address a4
  address a5
  v22: u8 = read a4
  v23: u8 = read a5
  dead s0
  return v14, v22, v23
}}"
        )
        .unwrap();
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut runs = Vec::new();
    for size in 0..=129u32 {
        let mut bytes = [0u8; 131];
        bytes[0] = 97;
        bytes[130] = 101;
        for i in 0..size as usize {
            bytes[i + 1] = i as u8 + 1;
        }
        let hash = bytes
            .into_iter()
            .fold(0u64, |h, b| h.wrapping_mul(257).wrapping_add(u64::from(b)));
        for function in [FuncId(size), FuncId(130)] {
            let report = eval::run(&module, function, &[u64::from(size)], 1000);
            assert_eq!(report.outcome, Outcome::Returned(vec![hash, 97, 101]));
            runs.push((function, vec![u64::from(size)]));
        }
    }
    for optimize in [false, true] {
        execute(&module, &runs, optimize);
    }
}

