# The Clifford Compendium

A narrative companion to the Clifford language: design rationale, mathematical foundations, and an annotated bibliography of every literature reference informing the compiler.

## Reading online

Once mdBook is installed (`cargo install mdbook`), build and serve:

```bash
mdbook serve book/
# open http://localhost:3000
```

For static HTML output:

```bash
mdbook build book/
# output in book/book/
```

## Reading offline

Every chapter is a plain markdown file under `src/`. Browse them in any order; chapters are individually self-contained. Start with [`src/preface.md`](src/preface.md) and [`src/SUMMARY.md`](src/SUMMARY.md) for the table of contents.

## Status

**Book v0.1 — May 2026.** Substantively written:

- Preface
- How to use this book
- Chapter 1: The problem
- Chapter 21: Decision #21 — Shared automata via mutator multivectors (the marquee chapter)
- Annotated bibliography (Part VI — complete)
- Decision Index

Stubbed (full chapters pending future revisions):

- Chapter 2 (conventional answers), Chapter 3 (the GA angle)
- Decision chapters 4–20 (Decisions #1–#20 each have a stub linking to `DECISIONS.md`)
- Part III (Mathematics primers + theorem chapters)
- Part IV (Compiler walkthroughs)
- Part V (Practice patterns)
- Glossary, cross-references

## Companion documents

- `../docs/CLIFFORD_SPEC.md` — the technical specification (normative)
- `../docs/DECISIONS.md` — the locked design decisions, indexed
- `../docs/adr/` — architecture decision records (full-detail design discussions)
- `../crates/` — the compiler itself

Where this book disagrees with the spec, the spec wins; please file an issue.

## License

Text: CC-BY-SA 4.0. Code samples within the text: MIT. See the project root `LICENSE` files.
