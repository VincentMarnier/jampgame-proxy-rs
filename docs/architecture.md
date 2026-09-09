# Architecture

> Status: Initial investigation

## Components

### Jedi Academy

Location:

`original/jedi-academy/`

Purpose:

Reference source for the Jedi Academy engine/game code and APIs used by the proxy.

### Original jampgame\_proxy

Location:

`original/jampgame-proxy/`

Purpose:

Reference implementation of the existing proxy.

### Rust implementation

Location:

`rust/`

Purpose:

New implementation.

## Current understanding

This document will be updated as the original implementation is investigated.

## Unknowns

- Exact runtime loading sequence.
- Exact exported ABI.
- Complete list of hooks.
- Runtime address requirements.
- Which behavior is implemented by the proxy versus the original game code.
- Which binary interfaces must remain compatible.