# F22 — remove retired Task test infrastructure

Recommend, low cleanup priority, high confidence. C27 worker F2. Coordinator opened the 181-line e2e/lib/harness.mjs and historical e2e/external-model-smoke/runbook.md, checked current app package scripts and Thread rules, and reran hidden-source caller/retired-command queries excluding only the files themselves and .git: both zero results.

The unused harness exposes Task selectors/port families; its only associated historical runbook links deleted runners and reports. Current Thread/artifact scripts have their own owned gates. Under the authorized wholehog cutover, remove these two obsolete files instead of keeping a second apparent gate surface. Do not replace with new shared harness, regenerate retired external-model testing, or run a paid/live smoke. Check current tracked CI/scripts references and documentation links before deletion; unknown external automation is not inspected in this read-only audit. No tests/source changes performed.

Independent materiality priority: low. This is a bounded display/wholehog cleanup correction, not peer severity to durable admission or captured identity defects.

Later integration qualification (root-reported, outside audited3ee): a new blob browser gate acquired an active harness consumer after this snapshot. Root preserved that later consumer using minimal local helpers while removing the retired harness. The zero-caller proof applies only to3ee; do not infer the current integration tree has no callers without reopening it. Implementation handoff: /tmp/hirsel-mock-gates-handoff.md, maintained by root outside this audit.
