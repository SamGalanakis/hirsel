# C02 local inspection notes (not completed worker coverage)

Snapshot 3ee0621, tree a4aac830. Source read only.

Confirmed F01 CLI restart FIFO blockage. Complete report beside this note.

Rejected early lead: run_thread_turn returns unchanged terminal turns and CLI execute does not inspect its return state. This alone does NOT prove a cancelled queued turn launches: ThreadToolBridge::start calls bind_thread_execution, which explicitly requires Running, and subsequent context reads validate the execution caller again. Do not promote that simplified cancellation-race claim without independently establishing a complete interleaving through all barriers.

No recommendation for RuntimeTasks' Option<Vec<JoinHandle>>: Option records generation open/stopped; stopped spawn is discarded and stop aborts then drains all owned tasks before history reset. Retain local ownership; no proposed abstraction removes invalid state here.

No recommendation for ThreadExecution::Host/Cli enum: accepted configurations are tagged, deny unknown fields, separate host model metadata from CLI model/variant/cwd. This is a useful existing representation, not a stringly flag bundle.

OwnerTurn.turn_id Option currently validates Some at stored_turn; admission and cancellation have duplicate fallback branches, but actual producers include preaccept JSON drafts and broadening this into an invasive input-type cutover requires complete producer coverage. Not accepted merely for replacing an Option.

CLI durable terminal retry is freshly integrated from current #13 correction: provider is retired independently, output and failure activity commit together and retry with generation ownership. Do not repeat existing delivery issue #6 or newly fixed current-source behavior.
