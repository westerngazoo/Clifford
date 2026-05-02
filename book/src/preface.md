# Preface

This is a book about a language that does not exist yet, written in parallel with the compiler that will eventually make it exist. The language is **Clifford**, and its claim is that *concurrent shared-state safety* — the part of systems programming that gets people killed in medical devices and grounds aircraft — is mathematically reducible to a question about geometric algebra.

If that sentence sounds extravagant, you are reading the right book. The job of these pages is to convince you it is also true.

## What this book is

A *narrative companion* to the Clifford compiler. The companion to:

- `docs/CLIFFORD_SPEC.md` — the technical specification (the *what*)
- `docs/DECISIONS.md` — the locked design decisions, indexed (the *which*)
- `docs/adr/*.md` — the architecture decision records (the *how we got here*)
- `crates/*` — the compiler itself (the *how*)

This book is the *why*. Each language design choice is documented twice in this project: once in `DECISIONS.md` as a single locked entry with rationale, and once here as a chapter that walks through the conventional answer (what mainstream systems languages do), Clifford's answer (what we do), and the literature trail behind both. The two documents are deliberately redundant; `DECISIONS.md` is for the author maintaining the language, this book is for the reader trying to understand it.

## What this book is not

It is not a tutorial. It assumes you can already read a systems language — Rust, C++, Zig, or comparable. It will not teach you what a `struct` is or how a stack frame works.

It is not a reference. The spec is the reference. Where this book disagrees with the spec, the spec wins.

It is not a finished work. As of this writing the compiler is mid-Phase-1 (parser + resolver + partial type checker + layer-boundary checker shipped to `main`); the GA orthogonality engine, LLVM codegen, and standard library are not yet built. Chapters describing those phases are stubs that will fill in as the work lands. The bibliography (Part VI) is, however, already complete and citable.

## How the book is organized

**Part I — Why Clifford** sets up the problem. Why is concurrency safety in systems languages still hard in 2026? What did the conventional answers (Rust's borrow checker, seL4's capability proofs, Tock's capsule isolation, Hubris's static task tables) get right, and where do they leave gaps? What does geometric algebra offer that these did not?

**Part II — The Language** is the longest section. One chapter per locked design decision (Decisions #1–#21), each following the same template: *the question*, *the conventional answer*, *what Clifford does instead*, *why*, *trade-offs*, *literature*. If you only want to understand a single design choice, read its chapter directly — they are individually self-contained.

**Part III — The Mathematics** is the foundation. Three primer chapters (type theory, category theory, geometric algebra) cover what a reader from a systems-programming background might not have on hand. The remaining chapters in this part build the mathematical narrative of the language: the two categorical layers (control-flow + Kleisli), the orthogonality theorem, the mixed-metric extension for shared state, the rotor formulation for same-priority lock disambiguation. This is the part that will become the published paper.

**Part IV — The Compiler** is implementation-shaped. One chapter per crate, walking through the design of the lexer, parser, AST, resolver, type checker, layer-boundary checker, GA orthogonality engine, and LLVM IR emitter. These chapters double as onboarding documentation for new compiler contributors.

**Part V — Practice** is the application layer. How to write a Clifford program. Embedded firmware patterns. Kernel patterns (specifically the Wari kernel — the user's own RISC-V OS, against which Clifford is being co-designed). Migration paths from Rust and from C.

**Part VI — Reference** holds the annotated bibliography, glossary, and cross-references. The bibliography is the heart of this book for academic use: every literature reference informing every design decision, organized by topic, with notes on which chapter and which design choice each citation supports.

## Why this book exists

Two reasons.

**The first is preservation.** The compiler will eventually be feature-complete and the language will, with luck, find users. Those users will ask the same questions you might ask now: *Why a sigil-based syntax?* *Why a state monad over the field tuple instead of effect handlers?* *Why a Clifford algebra at all when you could have used separation logic?* The answers are not obvious from the spec alone. They live in the design discussions that produced the spec, which were synchronous and thus invisible to anyone who came afterward. This book makes those discussions persistent.

**The second is publication.** The mathematical contribution of Clifford — that *concurrent shared-state safety reduces to a wedge-product non-zero check in a mixed-metric Clifford algebra over priority-graded basis vectors* — is novel work. It does not exist in the published literature, in either the geometric algebra community (which mostly applies GA to graphics, robotics, and physics) or the programming-languages community (which uses separation logic, type systems, and game semantics). The intersection has not been mined. The chapters in Parts II.21 and III collectively are the draft of the paper that will mine it.

## A note on tone

This book is a working document. It will be revised, sometimes severely. Where the language design has evolved since the chapter was written, you will find footnotes pointing at the change. Where a design choice is still contested or open, the chapter will say so explicitly rather than pretend the answer is settled. The honest book is more useful than the polished one.

Where chapters draw on other authors' work, citations are by **[Author Year]** in the chapter text and resolved through the bibliography. The bibliography is comprehensive within the scope of work that informed Clifford; it is not comprehensive across the broader fields it cites.

## Acknowledgements

Clifford is a one-person language project. The author thanks the implementers of every system that informed it — Rust, seL4, Tock, Hubris, Wari (his own predecessor), Iris, GhostCell, Stanley's combinatorial commutative algebra, Dorst-Fontijne-Mann's geometric algebra, Pierce's type theory, Mac Lane's categories — with particular thanks to GhostCell and seL4 for being the proof-points that the underlying property *is* checkable, even if their checking machinery differs from what Clifford ultimately uses. The work of those implementers is what makes Clifford possible.

The compiler co-author is an AI agent (Anthropic's Claude). Claude wrote substantial portions of the compiler under direction of the human author, and authored substantial portions of this book under similar direction. The technical decisions are the human author's; the prose and the implementation are collaborative. Where attribution matters for academic citation, the human author is the responsible party.

## Reading paths

This book is not strictly linear. Several paths through it are valid:

- **The systems engineer path.** Read Part I, then skim the Decision chapters in Part II that interest you, then go directly to Part V (Practice) for patterns.
- **The compiler implementer path.** Read Part I.3 for the GA orientation, Part III chapters 22–24 for the mathematical primer, then Part IV for implementation specifics.
- **The academic path.** Read Part III in full for the mathematical theory, then Part II.21 (Decision #21 — the marquee chapter) for the algebraic core, then the bibliography. The published paper extracts its narrative from these chapters.
- **The skeptic path.** Read Part I.2 (conventional answers) and Part I.3 (the GA angle) and decide whether the rest is worth your time. We are confident it is, but we acknowledge the right of the reader to disagree.

— *Goose, May 2026*
