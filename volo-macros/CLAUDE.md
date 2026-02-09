# CLAUDE.md - volo-macros

## Overview

`volo-macros` is a **reserved placeholder** crate for future procedural macros. It contains no active functionality.

Actual macros used in the Volo ecosystem live elsewhere:

- `#[service]` macro: provided by `motore`, re-exported via `volo`
- Declarative macros (`volo_unreachable!`, `new_type!`): defined in `volo/src/macros.rs`

## Release Order

`volo-macros` must be published **first** when releasing new versions (see root CLAUDE.md).
