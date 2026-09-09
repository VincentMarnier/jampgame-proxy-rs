Jampgame Proxy Rust Rewrite
Project goal

Rewrite jampgame_proxy from C/C++ to Rust while preserving its existing behavior and binary compatibility where required.

After compatibility is established, the Rust implementation may introduce improvements and new features.

Repository structure
original/ — original Jedi Academy and jampgame_proxy source code. Reference material only.
rust/ — new Rust implementation.
docs/ — reverse-engineering results, specifications, architecture documentation, and decisions.
tests/ — compatibility, differential, integration, and regression tests.
tools/ — scripts and tools used to analyze the original implementation and validate the Rust implementation.
.opencode/ — OpenCode-specific agents, skills, commands, and tools.
Critical rules
Original source is immutable

Never modify anything under original/.

The original implementation is a reference and source of evidence.

Do not "fix" the original implementation unless explicitly requested.

Evidence over assumptions

When analyzing the original implementation, distinguish between:

observed facts
source-code evidence
binary-analysis evidence
runtime observations
hypotheses
verified conclusions

Never present an inference as a fact.

When uncertainty exists, document it in docs/ and propose an experiment that could resolve it.

ABI preservation

ABI compatibility is critical.

Never change any of the following without explicitly documenting and verifying the consequences:

exported symbol names
calling conventions
function signatures
integer widths
pointer representation
structure layout
structure alignment
enum representation
symbol visibility
global data layout
ownership assumptions at FFI boundaries

Prefer #[repr(C)] for C-compatible structures.

Keep unsafe code isolated and documented.

Rust implementation

The Rust implementation should preserve observable behavior during the compatibility phase.

Do not make behavior changes merely because the Rust version can be made more idiomatic.

Prefer small, reviewable changes.

Run formatting, compilation, linting, and relevant tests after modifications.

Testing

Tests are authoritative.

Never remove or weaken a test merely to make an implementation pass.

When possible, compare the Rust implementation against the original implementation.

Prefer reproducible tests over manual verification.

Reverse engineering

Use the least expensive tool that can answer the question.

Prefer:

source-code search
nm
readelf
objdump / llvm-objdump
Ghidra (if available in `tools/`)
GDB/runtime instrumentation

Do not use Ghidra when simple symbol or disassembly information is sufficient.

Generated information

Generated reverse-engineering output should be reproducible.

Do not manually edit generated analysis artifacts.

Record conclusions and interpretations in docs/.

Changes

Before making a significant change:

Understand the existing behavior.
Identify relevant source and binary evidence.
Identify ABI implications.
Define or update tests.
Implement the smallest appropriate change.
Run the relevant verification.
Document important discoveries.
AI behavior

When uncertain, investigate rather than guess.

If evidence conflicts, report the conflict.

Do not silently invent missing information.

Do not claim that behavior is verified unless there is evidence supporting the claim.