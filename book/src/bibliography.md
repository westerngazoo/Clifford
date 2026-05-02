# Annotated Bibliography

> The full literature trail informing the Clifford language and compiler. Entries are organized by topic. Each entry includes a citation, a *Used in* note pointing at the chapter and decision it informs, and a *Recommended for* tag (`starter` for first reading; `deeper read` for follow-up; `reference only` for cite-but-don't-need-to-read).

This bibliography is the answer to "what should I read to understand the basis of every design decision in Clifford?" It is not comprehensive across the cited fields; it is comprehensive across what informed Clifford specifically.

---

## §1 Compiler Implementation

What we use: lexer with sigil dispatch, recursive-descent + Pratt expression parser, AST design with `#[non_exhaustive]` enums, symbol tables, scope chains, span-keyed binding maps, walker patterns over visitor traits.

### Books

1. **Aho, Lam, Sethi, Ullman.** *Compilers: Principles, Techniques, and Tools.* 2nd ed., Pearson, 2007. (The "dragon book.")
   - **Used in:** Ch. 30 (lexer), Ch. 31 (parser), Ch. 32 (AST design).
   - **Recommended for:** starter / reference.

2. **Appel, A. W.** *Modern Compiler Implementation in ML.* Cambridge University Press, 1998. (Or the C/Java editions.)
   - **Used in:** Ch. 32–35 — especially clean treatment of typed AST + IR lowering.
   - **Recommended for:** starter.

3. **Cooper, K. & Torczon, L.** *Engineering a Compiler.* 3rd ed., Morgan Kaufmann, 2022.
   - **Used in:** Ch. 31, Ch. 36 (dataflow analysis).
   - **Recommended for:** deeper read.

4. **Muchnick, S. S.** *Advanced Compiler Design and Implementation.* Morgan Kaufmann, 1997.
   - **Used in:** Ch. 37 (codegen / optimization).
   - **Recommended for:** reference only.

### Papers

5. **Pratt, V. R.** "Top down operator precedence." *POPL 1973*, 41–51.
   - **Used in:** Ch. 31. Direct cite — Clifford's expression parser is Pratt's algorithm.
   - **Recommended for:** starter (only ~10 pages, foundational).

### Online resources

6. **Bendersky, E.** *Let's Build a Compiler* blog series (eli.thegreenplace.net).
   - **Used in:** Ch. 30, 31. Best modern walkthrough of recursive descent + Pratt.
   - **Recommended for:** starter.

---

## §2 Type Theory & PL Semantics

What we use: Hindley-Milner inference, algebraic data types, structural trait satisfaction (§5.3 of the spec), bidirectional / ascription-driven typing in slice T1, `Type` as a sum type with display + classification helpers.

### Books

1. **Pierce, B. C.** *Types and Programming Languages.* MIT Press, 2002. (TAPL.)
   - **Used in:** Ch. 22 (type-theory primer), Ch. 34 (type checker), Decisions #1, #2, #4, #19.
   - **Recommended for:** starter — the essential textbook.

2. **Pierce, B. C.** (ed.) *Advanced Topics in Types and Programming Languages.* MIT Press, 2005. (ATTAPL.)
   - **Used in:** Ch. 14 (Decision #13 / substructural types), Ch. 27.
   - **Recommended for:** deeper read.

3. **Harper, R.** *Practical Foundations for Programming Languages.* 2nd ed., Cambridge, 2016. (PFPL.)
   - **Used in:** Ch. 22, Ch. 25 (effect typing).
   - **Recommended for:** deeper read — judgment-style, modern.

### Papers

4. **Damas, L. & Milner, R.** "Principal type-schemes for functional programs." *POPL 1982*, 207–212.
   - **Used in:** Ch. 34 — the original Hindley-Milner inference paper.
   - **Recommended for:** starter (foundational, brief).

5. **Pierce, B. C. & Turner, D. N.** "Local type inference." *TOPLAS 2000*, 22(1).
   - **Used in:** Ch. 34. Bidirectional typing — Clifford's slice T1 uses ascription-driven typing inspired by this work.
   - **Recommended for:** deeper read.

6. **Cardelli, L. & Wegner, P.** "On understanding types, data abstraction, and polymorphism." *Computing Surveys 1985*, 17(4).
   - **Used in:** Ch. 22 (foundational type-theory framing).
   - **Recommended for:** starter.

---

## §3 Category Theory + Monadic Computation

What we use: small categories (per Decision #5), product categories, Kleisli categories of State monad (per the categorical-layers framing), eventually premonoidal / operadic structures for Decision #21.

### Books

1. **Mac Lane, S.** *Categories for the Working Mathematician.* 2nd ed., Springer, 1998.
   - **Used in:** Ch. 23, Ch. 25, Ch. 26, Ch. 27.
   - **Recommended for:** reference. The textbook, but dense — try Awodey first.

2. **Awodey, S.** *Category Theory.* 2nd ed., Oxford, 2010.
   - **Used in:** Ch. 23 (the primer chapter).
   - **Recommended for:** starter — best modern intro for non-specialists.

3. **Riehl, E.** *Category Theory in Context.* Dover, 2016. (Free PDF at math.jhu.edu/~eriehl/context.pdf.)
   - **Used in:** Ch. 23.
   - **Recommended for:** starter (free!), excellent on adjoints / Yoneda.

4. **Riehl, E.** *Categorical Homotopy Theory.* Cambridge, 2014.
   - **Used in:** reference for higher structure if Decision #21 work goes that direction.
   - **Recommended for:** reference only.

### Papers (the critical ones for Clifford)

5. **Moggi, E.** "Notions of computation and monads." *Information and Computation 1991*, 93(1). (Conference version: LICS 1989.)
   - **Used in:** Ch. 25 (categorical layers — the State-monad framing of mutators). **Direct cite for the paper extracted from this book.**
   - **Recommended for:** starter — foundational for the whole monadic-effects view.

6. **Wadler, P.** "Monads for functional programming." *Lecture Notes in Computer Science 925*, Springer, 1995.
   - **Used in:** Ch. 25.
   - **Recommended for:** starter — pedagogical companion to Moggi.

7. **Power, J. & Robinson, E.** "Premonoidal categories and notions of computation." *Mathematical Structures in Computer Science 1997*, 7(5).
   - **Used in:** Ch. 25, Ch. 27. **Critical for Decision #21 categorical framing.**
   - **Recommended for:** deeper read.

8. **Plotkin, G. & Power, J.** "Notions of computation determine monads." *FoSSaCS 2002*.
   - **Used in:** Ch. 25.
   - **Recommended for:** deeper read.

9. **Hyland, M., Plotkin, G., & Power, J.** "Combining effects: sum and tensor." *Theoretical Computer Science 2006*, 357(1–3).
   - **Used in:** Ch. 25 (effect composition).
   - **Recommended for:** deeper read.

10. **Bauer, A. & Pretnar, M.** "Programming with algebraic effects and handlers." *Journal of Logic and Algebraic Programming 2015*, 84(1).
    - **Used in:** Ch. 25 — algebraic effects vs the monadic view.
    - **Recommended for:** starter.

---

## §4 Geometric Algebra & Clifford Algebra

What we use: Cl(0,0,n) restricted form (current §7), Cl(p,0,n) mixed-metric extension (Decision #21), wedge product, exterior/Grassmann algebra, rotors as `cos(θ/2) + sin(θ/2)·B`, Koszul signs.

### Books

1. **Dorst, L., Fontijne, D., & Mann, S.** *Geometric Algebra for Computer Science: An Object-Oriented Approach to Geometry.* Morgan Kaufmann, 2007.
   - **Used in:** Ch. 24 (GA primer), Ch. 26 (orthogonality), Ch. 27 (mixed-metric), Ch. 28 (rotors), Decision #4, Decision #21. **The textbook for Clifford's GA foundation.**
   - **Recommended for:** starter — most accessible, algorithm-oriented.

2. **Doran, C. & Lasenby, A.** *Geometric Algebra for Physicists.* Cambridge, 2003.
   - **Used in:** Ch. 24, Ch. 27, Ch. 28. The physicist's treatment with richer mathematical depth.
   - **Recommended for:** deeper read.

3. **Hestenes, D.** *New Foundations for Classical Mechanics.* 2nd ed., Springer, 1999. (Original 1986.)
   - **Used in:** Ch. 24 — foundational GA framing.
   - **Recommended for:** deeper read (Hestenes invented modern GA).

4. **Hestenes, D. & Sobczyk, G.** *Clifford Algebra to Geometric Calculus.* Reidel, 1984.
   - **Used in:** reference.
   - **Recommended for:** reference only.

5. **Lounesto, P.** *Clifford Algebras and Spinors.* 2nd ed., Cambridge, 2001.
   - **Used in:** Ch. 24, Ch. 27 — particularly mixed-metric coverage. **Critical for Decision #21.**
   - **Recommended for:** deeper read.

6. **Garling, D. J. H.** *Clifford Algebras: An Introduction.* Cambridge, 2011.
   - **Used in:** reference.
   - **Recommended for:** reference only — brief, clean, citable.

7. **Vaz, J. & da Rocha, R.** *An Introduction to Clifford Algebras and Spinors.* Oxford, 2016.
   - **Used in:** Ch. 27.
   - **Recommended for:** deeper read — modern, mathematically careful.

### Software

8. **garust** — Rust GA library, [github.com/aweinstock314/garust](https://github.com/aweinstock314/garust).
   - **Used in:** Ch. 36 (orthogonality engine implementation). Structurally identical to our bitmask representation.
   - **Recommended for:** reference.

---

## §5 Concurrency, Locks, and Race-Detection

What we use: disjoint-mutation safety via §7, lock-coupled access for Decision #21, priority-ordered acquisition, RCU patterns, lockdep-style diagnostics, separation-logic intuitions.

### Books

1. **Hoare, C. A. R.** *Communicating Sequential Processes.* Prentice-Hall, 1985. (Free PDF at usingcsp.com.)
   - **Used in:** Ch. 1 (the problem), Ch. 25.
   - **Recommended for:** starter (free).

2. **Milner, R.** *Communication and Concurrency.* Prentice-Hall, 1989.
   - **Used in:** Ch. 25 (CCS / π-calculus background).
   - **Recommended for:** reference only.

3. **Reisig, W.** *Petri Nets: An Introduction.* Springer, 1985.
   - **Used in:** Ch. 1 — reference for liveness / deadlock formalism.
   - **Recommended for:** reference only.

### Papers

4. **Lamport, L.** "Time, clocks, and the ordering of events in a distributed system." *CACM 1978*, 21(7).
   - **Used in:** Ch. 1 — foundational.
   - **Recommended for:** starter.

5. **Reynolds, J. C.** "Separation logic: A logic for shared mutable data structures." *LICS 2002*.
   - **Used in:** Ch. 25, Decision #21.
   - **Recommended for:** deeper read.

6. **O'Hearn, P. W.** "Resources, concurrency, and local reasoning." *CONCUR 2007 / TCS 2007*, 375(1–3).
   - **Used in:** Ch. 25, Decision #21. Concurrent separation logic.
   - **Recommended for:** deeper read.

7. **Chess, B. et al.** "On the correctness of lock-free algorithms." (informal write-up; not formally published — referenced for the property characterization).
   - **Used in:** Ch. 21 (Decision #21).
   - **Recommended for:** reference.

### Practical references

8. **McKenney, P.** "What is RCU?" — LWN.net article series.
   - **Used in:** Decision #21, Ch. 21.
   - **Recommended for:** starter (free, applied).

9. **The Linux kernel lockdep documentation.** `Documentation/locking/lockdep-design.txt`.
   - **Used in:** Ch. 1, Ch. 21. **The property the §7.9 mixed-metric algebra is designed to prove statically.**
   - **Recommended for:** starter.

---

## §6 Substructural Types & Body-Scoped References

What we use: Decision #13's body-scoped references with provenance — directly inspired by linear / affine types and Rust's borrow checker.

### Papers

1. **Wadler, P.** "Linear types can change the world!" *Programming Concepts and Methods 1990*, 561.
   - **Used in:** Ch. 14 (Decision #13). Foundational.
   - **Recommended for:** starter.

2. **Walker, D.** "Substructural type systems." Chapter 1 of *ATTAPL* (Pierce, ed.).
   - **Used in:** Ch. 14, Ch. 22.
   - **Recommended for:** deeper read.

3. **Bernardy, J.-P., Boespflug, M., Newton, R., Peyton Jones, S., & Spiwack, A.** "Linear Haskell: Practical linearity in a higher-order polymorphic language." *POPL 2018*.
   - **Used in:** Ch. 14.
   - **Recommended for:** deeper read.

4. **Tov, J. A. & Pucella, R.** "Practical affine types." *POPL 2011*.
   - **Used in:** Ch. 14.
   - **Recommended for:** deeper read.

5. **Crary, K., Walker, D., & Morrisett, G.** "Typed memory management in a calculus of capabilities." *POPL 1999*.
   - **Used in:** Ch. 14, Decision #16.
   - **Recommended for:** deeper read.

6. **Grossman, D., Morrisett, G., Jim, T., et al.** "Region-based memory management in Cyclone." *PLDI 2002*.
   - **Used in:** Ch. 14 — pre-Rust region types.
   - **Recommended for:** deeper read.

### Resources

7. **The Rustonomicon.** [doc.rust-lang.org/nomicon](https://doc.rust-lang.org/nomicon/) — particularly the Stacked Borrows / Tree Borrows sections.
   - **Used in:** Ch. 14. Direct prior art for Clifford's reference rules.
   - **Recommended for:** starter (free, online).

8. **Yanovski, J., Dang, H.-H., Jung, R., & Dreyer, D.** "GhostCell: Separating permissions from data in Rust." *ICFP 2021*.
   - **Used in:** Ch. 14, Ch. 21 (Decision #21). **Closest cousin to Decision #21's algebraic approach.**
   - **Recommended for:** starter — short, beautifully written, directly relevant.

---

## §7 Algebraic Effects & Effect Systems

What we use: Decision #2's `$ [TraitList]` markers as effect annotations, the Kleisli-category framing for `#> proc()`, eventual algebraic-effects-style `$ [State<A>]` annotations.

### Papers

1. **Plotkin, G. & Pretnar, M.** "Handlers of algebraic effects." *ESOP 2009*.
   - **Used in:** Ch. 25 (algebraic effects + handlers foundation).
   - **Recommended for:** starter.

2. **Pretnar, M.** "An introduction to algebraic effects and handlers." *MFPS 2015*.
   - **Used in:** Ch. 25.
   - **Recommended for:** starter (pedagogical).

3. **Leijen, D.** "Type directed compilation of row-typed algebraic effects." *POPL 2017*.
   - **Used in:** Ch. 25 — the Koka language.
   - **Recommended for:** deeper read.

4. **Lindley, S., McBride, C., & McLaughlin, C.** "Do be do be do." *POPL 2017*.
   - **Used in:** Ch. 25 — the Frank language.
   - **Recommended for:** deeper read.

5. **Brachthäuser, J. I., Schuster, P., & Ostermann, K.** "Effects as capabilities: effect handlers and lightweight effect polymorphism." *OOPSLA 2020*.
   - **Used in:** Ch. 25, Ch. 18 (Decision #17 narrow primitives).
   - **Recommended for:** deeper read.

6. **Tate, R.** "The sequential semantics of producer effect systems." *POPL 2013*.
   - **Used in:** Ch. 25 (effect ordering / commutativity).
   - **Recommended for:** deeper read.

---

## §8 Separation Logic & Program Verification

What we use: Iris-style proof obligations as the conceptual model for Decision #21's lock-coverage discharge.

### Papers

1. **Jung, R., Swasey, D., Sieczkowski, F., Svendsen, K., Turon, A., Birkedal, L., & Dreyer, D.** "Iris: Monoids and invariants as an orthogonal basis for concurrent reasoning." *POPL 2015*.
   - **Used in:** Ch. 21 (Decision #21).
   - **Recommended for:** deeper read.

2. **Jung, R., Krebbers, R., Birkedal, L., & Dreyer, D.** "Higher-order ghost state." *ICFP 2016*.
   - **Used in:** Decision #21.
   - **Recommended for:** deeper read.

3. **Jung, R., Jourdan, J.-H., Krebbers, R., & Dreyer, D.** "RustBelt: Securing the foundations of the Rust programming language." *POPL 2018*.
   - **Used in:** Ch. 1, Ch. 14, Ch. 21. **Direct cite for the paper.**
   - **Recommended for:** starter.

4. **Krebbers, R., Timany, A., & Birkedal, L.** "Interactive proofs in higher-order concurrent separation logic." *POPL 2017*.
   - **Used in:** Decision #21.
   - **Recommended for:** deeper read.

5. **Brookes, S.** "A semantics for concurrent separation logic." *CONCUR 2004 / TCS 2007*.
   - **Used in:** Ch. 25, Decision #21.
   - **Recommended for:** deeper read.

6. **Pottier, F. & Protzenko, J.** *Mezzo: A typed language for safe effectful concurrent programs.* ICFP 2013.
   - **Used in:** Ch. 14, Ch. 25.
   - **Recommended for:** reference.

---

## §9 Capability-Based Security & seL4

What we use: structural inspiration for Decision #16 (`#interface` + `#impl`) and for Wari's capability subsystem.

### Papers

1. **Klein, G., Elphinstone, K., Heiser, G., et al.** "seL4: Formal verification of an OS kernel." *SOSP 2009*.
   - **Used in:** Ch. 1, Ch. 21, Decision #16. **Direct cite — proof-of-concept that this property is checkable.**
   - **Recommended for:** starter.

2. **Sewell, T., Myreen, M. O., & Klein, G.** "seL4 enforces integrity." *ITP 2011*.
   - **Used in:** Decision #21.
   - **Recommended for:** deeper read.

3. **Sewell, T. et al.** "seL4: from general purpose to a proof of information flow enforcement." *S&P 2013*.
   - **Used in:** Decision #21.
   - **Recommended for:** deeper read.

### Books

4. **Miller, M. S.** *Robust Composition: Towards a Unified Approach to Access Control and Concurrency Control.* PhD thesis, Johns Hopkins, 2006.
   - **Used in:** Decision #16, Ch. 17.
   - **Recommended for:** starter — the capability-systems bible.

5. **Levy, H. M.** *Capability-Based Computer Systems.* Digital Press, 1984. (Available free online.)
   - **Used in:** Decision #16.
   - **Recommended for:** reference (historical foundation).

### Hardware

6. **Watson, R. N. M. et al.** "Capability hardware enhanced RISC instructions: CHERI instruction-set architecture." *UCAM-CL-TR-907*, University of Cambridge.
   - **Used in:** Decision #19, future hardware-codegen work.
   - **Recommended for:** reference.

---

## §10 Embedded Systems & RISC-V

What we use: register-block automata (Decision #6), interrupt priorities, MMIO via narrow unsafe primitives, the eventual RISC-V codegen target.

### Books

1. **Patterson, D. A. & Hennessy, J. L.** *Computer Organization and Design: RISC-V Edition.* 2nd ed., Morgan Kaufmann, 2020.
   - **Used in:** Ch. 39, Ch. 40, Decision #6, Decision #10.
   - **Recommended for:** starter.

2. **Yiu, J.** *The Definitive Guide to ARM Cortex-M3/M4.* Newnes, 2014.
   - **Used in:** Ch. 39, Decision #10. NVIC priority comparison.
   - **Recommended for:** reference.

3. **Quinn, S. et al.** *Programming Embedded Systems.* O'Reilly, various editions.
   - **Used in:** Ch. 39.
   - **Recommended for:** reference.

### Specifications

4. **Waterman, A.** *Design of the RISC-V Instruction Set Architecture.* PhD thesis, Berkeley, 2016.
   - **Used in:** Ch. 40.
   - **Recommended for:** reference.

5. **The RISC-V Specifications.** Free PDFs at riscv.org.
   - *RISC-V Unprivileged ISA*
   - *RISC-V Privileged ISA*
   - *Supervisor Binary Interface (SBI) Specification*
   - **Used in:** Ch. 39, Ch. 40, Ch. 37 (codegen). **Direct citations.**
   - **Recommended for:** starter (free).

### Papers / OS

6. **Levy, A. et al.** "Multiprogramming a 64 kB computer safely and efficiently." *SOSP 2017*. (Tock OS paper.)
   - **Used in:** Ch. 40, Decision #16.
   - **Recommended for:** starter.

---

## §11 Combinatorial Commutative Algebra (Stanley–Reisner)

What we use: alternative algebraic encoding of the lock-discipline structure (the road not taken — Option C in ADR 0002's design space).

### Books

1. **Stanley, R. P.** *Combinatorics and Commutative Algebra.* 2nd ed., Birkhäuser, 1996.
   - **Used in:** Ch. 29 (the road not taken). **The reference for Stanley–Reisner rings.**
   - **Recommended for:** deeper read.

2. **Miller, E. & Sturmfels, B.** *Combinatorial Commutative Algebra.* Springer, 2005.
   - **Used in:** Ch. 29.
   - **Recommended for:** deeper read.

3. **Hibi, T.** *Algebraic Combinatorics on Convex Polytopes.* Carslaw, 1992.
   - **Used in:** Ch. 29.
   - **Recommended for:** reference.

4. **Bruns, W. & Herzog, J.** *Cohen-Macaulay Rings.* 2nd ed., Cambridge, 1998.
   - **Used in:** Ch. 29.
   - **Recommended for:** reference.

---

## §12 Programming Languages We Draw From / Contrast With

What we use: design comparisons for the related-work section.

### Resources / repositories

1. **The Rustonomicon** + the **Rust Reference**. doc.rust-lang.org.
   - **Used in:** Ch. 2, Ch. 14, Ch. 17, Ch. 41 (migration from Rust). Closest comparison; many design choices defined-by-contrast.
   - **Recommended for:** starter (free, online).

2. **Hubris** (Oxide Computer). [github.com/oxidecomputer/hubris](https://github.com/oxidecomputer/hubris) and the *On the Trail of a Strange Bug* blog series.
   - **Used in:** Ch. 1, Ch. 21, Ch. 40. Static-task-table OS contrast.
   - **Recommended for:** starter.

3. **Tock OS.** github.com/tock/tock.
   - **Used in:** Ch. 40. Capsule isolation contrast.
   - **Recommended for:** reference.

4. **Wari.** github.com/westerngazoo/wari. *(The author's own RISC-V kernel; co-designed with Clifford.)*
   - **Used in:** Ch. 40 — the canonical kernel-patterns target.
   - **Recommended for:** starter for anyone working through Ch. 40.

### Papers

5. **Clebsch, S., Drossopoulou, S., Blessing, S., & McNeil, A.** "Deny capabilities for safe, fast actors." *AGERE 2015*. (Pony language.)
   - **Used in:** Ch. 25 — reference-capability concurrency contrast.
   - **Recommended for:** deeper read.

6. **Verdagon (Vale language) blog posts.** verdagon.dev.
   - **Used in:** Ch. 14. Mutable-value-semantics contrast.
   - **Recommended for:** reference.

7. **Racordon, D. & Abrahams, D.** "Implementation strategies for mutable value semantics." (Hylo / Val.)
   - **Used in:** Ch. 14. MVS contrast.
   - **Recommended for:** reference.

8. **Carbon language design docs.** github.com/carbon-language.
   - **Used in:** Ch. 41.
   - **Recommended for:** reference.

9. **Verona language.** github.com/microsoft/verona. Microsoft Research.
   - **Used in:** Ch. 21 — region-based concurrent capability contrast.
   - **Recommended for:** reference.

---

## §13 Real-World Failure Case Studies

What we use: motivating case studies for Chapter 1.

1. **Leveson, N. G. & Turner, C. S.** "An investigation of the Therac-25 accidents." *IEEE Computer 1993*, 26(7), 18–41.
   - **Used in:** Ch. 1.
   - **Recommended for:** starter (sobering).

2. **NASA Engineering and Safety Center.** *Technical Assessment of Toyota Electronic Throttle Control (ETC) Systems.* NASA, 2011.
   - **Used in:** Ch. 1.
   - **Recommended for:** reference.

3. **U.S.–Canada Power System Outage Task Force.** *Final Report on the August 14, 2003 Blackout.* April 2004. Section on the GE XA/21 alarm system race condition.
   - **Used in:** Ch. 1.
   - **Recommended for:** reference.

---

## §14 Proof Assistants & Verification Tooling

What we use: cited as alternatives to Clifford's compile-time approach.

1. **Nipkow, T., Paulson, L. C., & Wenzel, M.** *Isabelle/HOL: A Proof Assistant for Higher-Order Logic.* Springer LNCS 2283, 2002.
   - **Used in:** Ch. 21, §9 above (seL4 verification language).
   - **Recommended for:** reference.

2. **The Software Foundations series.** Pierce et al., free at softwarefoundations.cis.upenn.edu.
   - **Used in:** Ch. 22, Ch. 25.
   - **Recommended for:** starter (free, excellent).

3. **The Coq Reference Manual.** coq.inria.fr/documentation.
   - **Used in:** §8 above (separation logic verification).
   - **Recommended for:** reference.

4. **The Lean 4 Theorem Prover.** leanprover.github.io.
   - **Used in:** future formalization-of-orthogonality-theorem work.
   - **Recommended for:** reference.

---

## What I'd buy first if budget were limited

Six books that cover ~80% of what the published paper will cite:

1. Pierce, *Types and Programming Languages* — §2.1
2. Dorst, Fontijne, Mann, *Geometric Algebra for Computer Science* — §4.1
3. Awodey, *Category Theory* — §3.2
4. Appel, *Modern Compiler Implementation in ML* — §1.2
5. Stanley, *Combinatorics and Commutative Algebra* — §11.1
6. Patterson & Hennessy, *Computer Organization and Design: RISC-V Edition* — §10.1

Plus three free PDFs that round it out:

- Riehl, *Category Theory in Context* — §3.3 (free)
- *Software Foundations* series — §14.2 (free)
- The RISC-V Specifications — §10.5 (free)

---

## What's missing from the literature

**The intersection has not been published.** Geometric algebra + concurrent shared-state safety + a real systems-language implementation has no published prior art that the author has been able to find. The closest single point of contact is GhostCell [Yanovski et al. 2021], which uses brand-types (a categorical move) for concurrent shared-state safety in Rust, but does not extend to GA.

That gap — between the GA community on one side and the PL community on the other, with no one circling the systems-language application — is what makes Decision #21 (Chapter 21 of this book) the basis of a publishable paper. The bibliography above is the literature that surrounds the gap; the chapters in Part II.21 + Part III collectively are the draft of the paper that fills it.
