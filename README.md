# Claim Wire (`cw`)

> **An incremental compiler for evidence-backed repository documentation.**

Suppose you maintain a systems or cryptography repository. Somewhere in your `README.md` and `docs/BENCHMARKS.md`, you make a few concrete statements:

1. Proving `radix-sort-u64` at $T = 2^{22}$ takes **8.55 s** (`test`) and **8.82 s** (`provable-120`), achieving **~491k cycles/s**—a **12.5× speedup** over the 107 s baseline.
2. The resulting proof is **589.8 KiB (603,942 B)**, which is **ahead of the paper's ≤ 599 KiB figure**.
3. The workspace passes **362 Rust tests**, **227 Python reference tests**, and **46 RV64I conformance tests** across all **27 test vector files**.
4. Round-by-round soundness is **≥ 120.0 bits** across all 40 ledger rows.

Over the next three weeks, you optimize the Virtual Spartan sum-check, add 35 Python tests, and tweak a guest linker script. What happens to the documentation?

Usually, one of two bad things:
- **Silent drift:** You update the primary table in `docs/BENCHMARKS.md`, forget that the same latency appears in two other tables and a throughput sentence in `README.md`, and leave `docs/spec/15-testing.md` claiming `192 tests` while `README.md` says `227`.
- **Unconstrained AI rewriting:** You hook an LLM up to CI and ask it to "keep the docs updated." Because an LLM has no formal notion of *what a good update is*, it either commits noisy thermal fluctuations as real changes or—much worse—responds to a benchmark regression by quietly rewriting *"ahead of the paper's 599 KiB bound"* into *"comparable to the paper's bound"* so the check passes.

The core thesis of **Claim Wire** is that you do not need an AI that rewrites prose on every commit, and you do not need to turn readable Markdown into a template language. Instead, you can treat factual claims in ordinary Markdown as **first-class build targets** backed by a typed dependency and provenance graph.

---

## Provenance vs. Invalidation

To make documentation mechanically maintainable without turning it into mush, a tool has to answer three narrow questions for every tracked statement:

1. **Provenance:** *Where did this rendered value come from, and how was it computed?*
2. **Invalidation:** *Which upstream inputs (source files, guest ELFs, test vectors, benchmark protocols) changed in a way that requires re-evaluating it?*
3. **Policy:** *When the underlying evidence changes—or fails a condition—is the tool allowed to rewrite the number in place, or must it halt and flag a broken invariant?*

Notice that provenance and invalidation look similar on the surface, but they travel in opposite directions:

```text
Provenance (backward / audit):
  README span ("~491k cycles/s")
    → derivation (cycles / prove_wall)
    → scoped observation (workload=radix-sort-u64, profile=test, value=8.55s)
    → immutable run manifest (bench-20261016-m4pro.json, git SHA 9872a7e, Apple M4 Pro)

Invalidation (forward / incremental):
  Modified source file (crates/om-spartan/src/lib.rs)
    → semantic input fingerprint mismatch
    → affected producer (om.benchmarks@current)
    → stale claim family (guest_bench[*, *])
    → affected Markdown tables and spans across README.md and docs/BENCHMARKS.md
```

A benchmark result attached to a Git commit SHA gives you provenance, but it does not by itself give you invalidation. If you key freshness on `HEAD` SHA alone, committing an updated `README.md` changes the commit hash and immediately makes your benchmark look stale again. Conversely, fixing a typo in a Rust doc-comment changes file bytes without changing the compiled binary's behavior.

Claim Wire resolves this by recording the Git SHA for **provenance**, while computing **freshness** from a **token-normalized semantic `blake3` fingerprint** of the producer's declared inputs (stripping comments and non-semantic whitespace).

---

## Probabilistic Discovery, Deterministic Maintenance

A recurring trap in developer tooling is using probabilistic models (LLMs) inside loops that require deterministic invariants, or forcing humans to write thousands of lines of brittle YAML for relationships an agent could infer in ten seconds.

Claim Wire splits the workflow into two explicit phases:

| Phase | Mechanism | Role |
|---|---|---|
| **1. Discovery (`claim-wire` Agent Skill)** | Probabilistic, semantic, human-reviewed | Scans existing `.md` files and benchmark harnesses; deduplicates numbers shared across documents; proposes `.claimwire/*.yaml` declarations and Markdown lenses. |
| **2. Maintenance (`cw` Rust CLI)** | Deterministic, typed, zero LLM dependency | Losslessly parses Markdown, type-checks dimensional expressions, verifies input fingerprints and assertions, and patches owned spans transactionally. |

Once a claim is wired, `cw check` and `cw sync` run in milliseconds in local pre-commit hooks or CI with zero network or model calls.

---

## How Claim Wire Avoids the Metadata Trap

If wiring a repository replaces:
> `code + benchmarks + manually synced docs`

with:
> `code + benchmarks + docs + 1,500 lines of fragile sidecar YAML and unreadable HTML comments`

then you have only moved the maintenance burden around.

Claim Wire is designed around six mechanisms (detailed in [`spec.md`](spec.md)) that keep both the Markdown and the sidecar small:

### 1. Relational Table Lenses (Zero HTML Clutter Inside Tables)
In most technical repositories, 80% of quantitative claims live inside Markdown pipe tables. Wrapping 60 table cells in individual `<!-- cw:... -->` comments makes raw Markdown 350 columns wide and painful to read in a text editor.

Instead, Claim Wire places a **single comment header** (`<!-- cw:table <lens-id> -->`) above an ordinary, completely clean GitHub Flavored Markdown table:

```markdown
<!-- cw:table readme.benchmarks -->
| Guest | Cycles (`T`) | Profile | Prove | Peak RSS | Verify | Proof Size |
|---|---:|---|---:|---:|---:|---:|
| `fib(19)` | `2^8` | test | 0.02 s | 16 MiB | 10 ms | 23,526 B |
| `fib(19)` | `2^8` | provable-120 | 0.02 s | 23 MiB | 20 ms | 83,654 B |
| `radix-sort-u64` (`A = 17`) | `2^22` | test | **8.55 s** | 8.3 GiB | 10 ms (27 MiB) | 60,806 B |
| `radix-sort-u64` (`A = 17`) | `2^22` | provable-120 | **8.82 s** | 8.4 GiB | 50 ms (48 MiB) | 603,942 B (589.8 KiB) |
```

The lossless CST parser understands three table orientations out of the box:
- **`records`** — Standard relational tables where rows are scope keys (`[Guest, Profile]`) and columns are metrics.
- **`transposed`** — Pivot tables where rows are heterogeneous metrics (`Cycles`, `Prove (wall)`, `Peak memory`, `Proof size`) and columns are profiles or historical epochs.
- **`checklist`** — Verification tables where each row binds an independent claim and target bound.

Unmapped prose columns, outer bold decorators (`**8.55 s**`), inline code ticks (`` `2^22` ``), and unmeasured sentinel cells (`—`, `exact`, `N/A`) are preserved automatically.

### 2. Block Lenses for Fenced Code Blocks & Clean Prose
You cannot put `<!-- cw:... -->` inside a ```` ```sh ```` fenced code block because CommonMark renders HTML comments inside code blocks as visible text. Placing `<!-- cw:block <id> -->` *above* a code block or paragraph binds interior numbers via an invertible line pattern:

````markdown
<!-- cw:block readme.test_suite_comment -->
```sh
# Run Rust, Python, and RV64I conformance + fuzz suites (362 Rust, 227 Python, 46 ISA tests)
cargo test --workspace --release
```
````

And for inline numbers in prose, spans reference claim expressions directly and close with a short `<!-- /cw -->` tag:

```markdown
At `T = 2^22`, throughput is **<!-- cw:guest_bench[radix-sort-u64, test].throughput -->~491k cycles/s<!-- /cw -->** (`test`)
and **<!-- cw:guest_bench[radix-sort-u64, provable-120].throughput -->~476k cycles/s<!-- /cw -->** (`provable-120`).
```

### 3. Parameterized Claim Families & Ordered Reductions
Instead of writing 200 scalar YAML stanzas for 15 workloads × 2 profiles × 7 metrics, you declare a **Claim Family** once in `.claimwire/claims.yaml`:

```yaml
families:
  guest_bench:
    producer: om.benchmarks
    dimensions: [workload, profile]
    metrics:
      cycles:      { type: Cycles,   format: "{:,}" }
      prove_wall:  { type: Duration, format: "{:.2f} s", deadband: 2.5% }
      peak_rss:    { type: Bytes,    format: "{:auto_iec}", deadband: 2.0% }
      verify_wall: { type: Duration, format: "{:.0f} ms", deadband: 5.0% }
      proof_size:  { type: Bytes,    format: "{:,} B" }
    derivations:
      throughput:
        type: Throughput
        expr: "cycles / prove_wall"
        format: "~{:.0f k cycles/s}"
      speedup_vs_initial:
        type: Ratio
        expr: "guest_bench@initial[workload, test].prove_wall / prove_wall"
        format: "{:.1f}×"
```

Claim IR includes dimensional type checking (`Cycles / Duration -> Throughput`, rejecting nonsensical expressions like `Duration + Count`), ordered family reductions (`prefix_sum(duration)` for cumulative phase tables), and subset aggregations (`sum(phase_bench[register_twist, ram_twist, bytecode_shout].duration)`).

### 4. Named Evidence Epochs (`@current`, `@prev`, `@initial`)
Engineering docs rarely report only the latest run; they tell an optimization story comparing `@current` (`8.55 s`) against `@prev` (`20.7 s`) and `@initial` (`107 s`). Producers bind immutable run manifests to named epochs so historical columns and speedup ratios (`12.5×`, `6.06×`) are derived directly across runs rather than hardcoded as magic constants.

### 5. No Silent Weakening (`assert`) & Deadband Hysteresis
Not every update should be published automatically:
- **Executable assertions (`assert`):** A statement like *"Ahead of the paper's 599 KiB figure"* is guarded by `guest_bench[radix-sort-u64, provable-120].proof_size <= paper.t22.proof_size_max`. If a code change inflates the proof to `620 KiB`, `cw sync` refuses to publish the broken row and exits with `CW3002 (Assertion Failed)`.
- **Deadband hysteresis:** Empirical timings (`Duration`) can declare `deadband: 2.5%` so minor thermal run-to-run variance does not churn `git log`, while deterministic metrics (`Bytes` proof size, `Cycles`, test `Count`s) enforce an exact `0%` deadband.

### 6. Context Window Anchoring (`CW4002`)
What if someone edits a sentence in `README.md` from `At T = 2^22, throughput is ...` to `At T = 2^24, throughput is ...` without touching the span inside it? The number is still fresh for $T = 2^{22}$, but now contradicts its surrounding sentence. `evidence.lock.json` stores a normalized `context_digest` of the enclosing line outside the span and flags `CW4002 (Surrounding Context Drift)` when qualifiers around a claim change.

---

## CLI Overview

```sh
cw check                  # Verify syntax, types, scopes, input freshness, assertions, and lockfile agreement
cw sync                   # Re-evaluate claims and transactionally patch owned spans, tables, and blocks
cw capture -- <cmd...>    # Capture @cw:record stdout frames + input fingerprints into an immutable run manifest
cw impact <path...>       # Trace modified source files forward to affected claims and Markdown file:line locations
cw explain <claim|loc>    # Print the full provenance tree from rendered Markdown bytes back to run manifests
cw diff <rev1>..<rev2>    # Compare claim states across Git revisions (Value vs. Definition vs. Scope changes)
cw adopt [<file...>]      # Reverse-parse existing Markdown via bidirectional formats to bootstrap evidence.lock.json
```

Honest coverage reporting is a core requirement. `cw check` reports only what it actually verified:

```text
$ cw check
Checking workspace: magnetar (5 documents, 3 producers)

  48 registered claims & families evaluated
   3 assertions passed (check.rs_faster_than_initial, check.proof_ahead_of_paper, check.soundness_120)
  92 Markdown locations verified across README.md, docs/BENCHMARKS.md, docs/VALIDATION.md, docs/spec/*

Status: OK (all 92 registered locations are fresh and verified)
Note: Unregistered prose outside Claim Wire spans, tables, and blocks was not assessed.
```

---

## Repository Structure & Specification

See [**`spec.md`**](spec.md) for the complete architecture, formal grammars, GFM table lexer state machine, dimensional type rules, diagnostic taxonomy (`CW1xxx`–`CW4xxx`), and the 10-point verification suite.

```text
claim-wire/
├── spec.md              # Normative technical specification (v0.3)
├── crates/
│   ├── cw-syntax/       # Lossless Markdown CST, backtick-aware GFM table lexer, block & format parser
│   ├── cw-ir/           # Dimensional type checker, family/reduction expander, scope & epoch DAG
│   ├── cw-evidence/     # Two-tier semantic fingerprinter, epoch manifest loader, @cw: wire capture
│   ├── cw-engine/       # Topological evaluator, deadband/sentinel handler, conflict auditor, patcher
│   └── cw-cli/          # `cw` command-line interface
├── skills/
│   └── claim-wire/      # Coding-agent skill for claim discovery, classification, and CWxxxx repair
└── tests/
    └── fixtures/
        └── magnetar/    # End-to-end test environment built from open-magnetar's Markdown files
```

---

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
