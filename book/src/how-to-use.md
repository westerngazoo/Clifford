# How to use this book

## Three modes

This book is written to support three reading modes simultaneously. The chapters are constructed so that any of the three can be performed without reading the others.

### Mode 1 — Linear read

Start at the Preface, end at the Bibliography. Each part is self-contained but they build on each other. **Estimated reading time: 8–12 hours** for the full book once finished; the substantive math is concentrated in Part III and adds another ~6 hours if you work through the proofs.

### Mode 2 — Targeted lookup

Jump directly to the chapter for a specific design decision. The Decision chapters (Part II) are individually self-contained: each names its question, surveys the conventional answer, presents Clifford's choice, and cites the relevant literature. You do not need to have read prior chapters to understand any one of them.

The **decision index** at the end of the book lets you jump directly from a `DECISIONS.md` entry number (e.g. *Decision #21*) to the corresponding chapter.

### Mode 3 — Bibliography mining

Read the annotated bibliography (Part VI) standalone. Each entry includes a *Used in* note pointing at the chapters and design decisions it informed. If you are surveying the literature for a related project, the bibliography is the most efficient way to do so.

## Conventions

- **Code in `monospace`** is concrete syntax — either Clifford source code, Rust source code, or shell commands depending on context.
- **Code with `// Clifford` or `// Rust` annotations** disambiguates when both languages appear in the same example.
- **Citations as `[Author Year]`** resolve through the bibliography. Multiple authors are cited as `[FirstAuthor et al Year]`.
- **Cross-references as `Ch. N`** point to chapters in this book. **`§A.B`** points to sections of `docs/CLIFFORD_SPEC.md`. **`Decision #N`** points to entries in `docs/DECISIONS.md`. **`ADR NNNN`** points to entries in `docs/adr/`.
- Sections marked **[OPEN]** are unresolved design questions where the chapter's author has not committed to a final answer.
- Sections marked **[INFORMATIVE]** are background; **[NORMATIVE]** is reserved for the spec, not this book.

## Pairing with the spec

The spec (`docs/CLIFFORD_SPEC.md`) is the **normative** document. This book is the **explanatory** document. Where they appear to disagree, the spec wins, and you should file an issue against this book to fix the discrepancy.

Specifically:

| Information you want | Where it lives |
|---|---|
| Exact grammar | Spec §2 |
| Exact type-checking algorithm | Spec §5 |
| Exact orthogonality theorem | Spec §7 |
| Why we chose the syntax we chose | Book Ch. 4–20 |
| Why GA at all | Book Ch. 1–3, 24, 26–28 |
| Why Decision #N takes the form it does | Book Ch. (N + 3) |
| What other systems do this differently | Book Ch. 2 + per-decision chapters |

## Pairing with the compiler

When a chapter in Part IV (The Compiler) describes implementation details, it references specific files and modules in the `crates/` tree. As of book v0 (May 2026), these chapters reflect the state of `main` at the time of writing; later compiler revisions will gradually invalidate parts of these chapters until they are re-revised. Look at the chapter's last-revised date to gauge currency.

The compiler's own test suite is the ground truth for behavior. Where this book says "the lexer does X," the relevant `#[test]` in `crates/lexer/src/lib.rs` is the verifier.

## Versioning

This book is versioned alongside the spec. The current spec version is `v0.6.0-draft` (as of 2026-05-02) and this book corresponds. When the spec bumps to `v0.7.0-draft` (Decision #21 implementation), corresponding chapters will revise.

The decision-by-decision version reservation table at the start of each Part II chapter calls out which spec version locked the decision and which version implements it (when the two differ — Decision #21, for example, is locked at v0.6 but will not be implemented until v0.7).

## Issues and contributions

Errors in this book — typos, factual mistakes, citations that don't resolve, claims that don't match the implementation — are bugs and should be filed against the [Clifford repository](https://github.com/westerngazoo/Clifford) under the `book` label. Substantive disagreements with technical claims are also welcome; those are likely to surface real design questions worth addressing in the spec.

The book is dual-licensed CC-BY-SA 4.0 (text) / MIT (code samples). See the project root `LICENSE` files.
