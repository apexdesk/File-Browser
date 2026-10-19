## read-fonts 0.41.0 (a budget for CFF charstrings)

A copy of `read-fonts` 0.41.0 from crates.io, used through
`[patch.crates-io]` in the workspace `Cargo.toml`.

**Why.** The charstring interpreter limits how deep subroutines nest (10)
but not how many times they are called, so a subroutine that calls the
next one ten times, ten levels down, runs ten billion times: a few bytes
of a font embedded in a document hang the program and fill the memory
with outline.

**What changed** (`src/ps/cs.rs`, marked "ApexDesk patch"): the evaluator
counts the operators and operands it runs for a glyph, subroutines and
seac accents included, and stops with an error past 2^20.
