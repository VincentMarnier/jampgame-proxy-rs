# Uncertainties

This file tracks questions whose answers have not yet been verified.

## U-001 — Complete proxy surface

### Question

Which functions and symbols does the original proxy actually intercept or replace?

### Current status

Unknown.

### How to investigate

- Inspect the original proxy source.
- Inspect exported symbols.
- Inspect hook registration code.
- Compare against the original `jampgame` binary.

---

## U-002 — Runtime addresses

### Question

Which addresses are required at runtime, and which are merely static ELF symbol values?

### Current status

Unknown.

### How to investigate

- Inspect the existing proxy implementation.
- Inspect ELF symbols and relocations.
- Run the original proxy under a debugger.
- Compare static addresses with runtime load addresses.

---

## U-003 — ABI requirements

### Question

Which functions and structures require exact C ABI compatibility?

### Current status

Unknown.

### How to investigate

- Inspect exported functions.
- Inspect function declarations.
- Inspect structures crossing the proxy boundary.
- Verify calling conventions and layouts.