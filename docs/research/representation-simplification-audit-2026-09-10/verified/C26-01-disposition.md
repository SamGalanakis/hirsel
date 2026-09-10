# C26-01 blanket locked Cargo policy — defer

Coordinator reopened the Cargo invocations and reran the worker query (16 matching lines). Workspace builds indeed omit --locked, but the fixed manifest/lock graph is synchronized and no unexpected resolution/build artifact was demonstrated. The worker's proposed high-priority rewrite of every build/check/run/metadata developer recipe is a prospective reproducibility policy, not a present invalid-state witness. Developer dependency updates intentionally need lock regeneration.

A narrowly chosen CI/release --locked policy is reasonable separate maintenance, but not promoted as a material schema/simplification finding or expanded into all development commands here. F23 separately deletes the existing duplicate Android producer; it does not require changing dependency resolution policy.
