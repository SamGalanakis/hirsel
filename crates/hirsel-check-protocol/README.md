# hirsel-check-protocol

Shared phase decisions and declaration conventions for signature normalization,
body checking, source auditing and TIR lowering. This small data crate keeps
hirsel-signature, hirsel-check and hirsel-lower independent compilation units.

Allowed dependencies: hirsel-span, hirsel-intern, hirsel-syntax and hirsel-types. No phase,
driver, interpreter or validator dependency, including in tests.

These records and helpers moved intact from hirsel-check. Import their actual
module directly; there are no exports through the implementation crates. The
frozen syntax/types/TIR public types and semantic encodings are unchanged.

ForProtocol::Bytes selects the existing indexed execution plan with a checked bytes.len call and a plain u8 index result. It carries no new TIR operation.
