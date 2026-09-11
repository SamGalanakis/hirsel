# Native Lash worker review finding classes

Focused scratchpad for the independent review of issue #56. These classes are
kept here so later runtime changes can search for the same failure shapes.

- Cleanup-induced cancellation can erase the provenance and diagnostic of an
  ordinary execution failure.
- Backend inheritance can bypass backend-specific input rejection or expansion
  when policy runs before the effective backend is resolved.
- A reusable session without a per-conversation watermark can omit the first
  handoff or conversation written while another backend owned the Task.
- Capability metadata keyed by an Owner-chosen provider label can be granted to
  a lookalike route that was never verified.
- Partial-line continuation can advance beyond bytes that were never returned.
- Completed shell calls can lose ownership of still-running descendants before
  worker shutdown.
