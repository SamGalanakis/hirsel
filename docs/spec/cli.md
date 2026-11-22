# Command-line interface

Native binaries built with `--backend llvm` include `.eh_frame` unwind
information. `--debug-lines on` requests native source locations.
Failure positions are available in either mode.
The native unwind records mark where a stack walk ends at a fiber boundary.
