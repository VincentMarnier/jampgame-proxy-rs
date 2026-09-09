# Documentation

This directory contains knowledge derived from the original implementation and the development of the Rust rewrite.

## Categories

- `architecture.md` — high-level architecture and component relationships (includes source authority: SDK authoritative for game ABI).
- `reverse-engineering.md` — discoveries about symbols, ABI, hooks, binaries, and runtime behavior.
- `uncertainties.md` — unresolved questions and hypotheses.
- `runtime-environment.md` — how to build and run the original binaries in a container for runtime investigation (Dockerfiles, layout, verified behaviour).
- `decisions/` — important architectural decisions once the project becomes more mature.

## Evidence levels

Use explicit evidence labels where useful:

- **SOURCE** — directly confirmed by source code.
- **STATIC** — confirmed through binary/static analysis.
- **RUNTIME** — observed during execution.
- **TESTED** — confirmed through an automated test.
- **INFERRED** — a reasonable interpretation but not yet verified.

Do not treat inferred information as verified behavior.