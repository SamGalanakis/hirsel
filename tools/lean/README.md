# Lean proofs

`hirsel lean extract MODULE --inputs MANIFEST --store DIRECTORY -o FILE -- FUNCTION...`
checks a CorpusManifest and emits Lean for the listed functions and their explicitly
listed, same-file dependencies. The extractor joins syntax spans with checked CoreIR.
Unsupported instructions, finalizers, indirect calls and unmodelled host operations
refuse extraction before it replaces the output.

Use the same arguments with `hirsel lean status` to compare the current extraction with
a saved file. Different bytes or a missing file report `stale` with exit status 0.
`hirsel lean probe` takes one full definition path and runs the same extraction over that
function and the functions of its file it calls. It lists every refusal, not just the first:
excluded operations, types with no Lean type, and calls with no Lean binding. Extraction's
report lists them the same way. A probe with no refusals extracts.

Pass the built `hirsel` and `hirsel-core-contract-eval` binaries to the opt-in suite.

```sh
python3 -I -B tools/lean/check.py --hirsel PATH --evaluator PATH
```

The suite uses the Lean and Lake version in `lean-toolchain`. It generates the Lean
definitions in scratch space under `~/.cache/hirsel/lean` by default. Use `--scratch`
to choose another directory outside the checkout. Normal builds and CI have no Lean
dependency.

The proofs apply to the extracted definitions. The evaluator comparisons test the
extractor and its prelude against Hirsel:

- **Triggers.** Five theorems cover stale attempts, the next issued attempt, and
  stability of terminal outcomes over arbitrarily long committed histories. They
  cover the trigger operations exercised by `check_trigger_model`. `Store.lean` names
  the transaction and codec assumptions. The simulator, scheduler, and host effects
  stay outside the proof.
- **Ordered maps.** The extracted read-only operations preserve the projected root
  across any number of snapshots. The model retains the tree contents and root
  metadata. Holder counts, allocation charges, capacity and ownership-sensitive
  update operations are outside this projection.
- **History register.** The extracted claim readback predicate rejects a different
  length or a changed prefix, for arbitrary byte lists. The evaluator fixture also
  covers the saved spans, compared slices, and tail. Object transport and retries
  stay outside the proof.
