---
name: claim-wire
description: Agent skill for bootstrapping, wiring, and repairing Claim Wire (cw) prose-to-evidence bindings across Markdown prose, GFM tables, and fenced code blocks. Activate when discovering quantitative claims in Markdown, creating .claimwire/evidence.yaml or .claimwire/claims.yaml, wiring cw:table / cw:block / cw:... directives, or resolving CW1xxx-CW4xxx compiler diagnostics.
---

# Claim Wire (`cw`) Agent Skill

Claim Wire is a two-phase verification and synchronization system for quantitative engineering claims in Markdown documentation:
1. **Phase 1 (Probabilistic Discovery — Agent Skill + Human Review):** Scan existing Markdown files (`README.md`, `docs/**/*.md`) for empirical metrics, deduplicate repeated measurements into parameterized **Claim Families**, author `.claimwire/evidence.yaml` and `.claimwire/claims.yaml`, place lightweight lenses (`<!-- cw:table <id> -->`, `<!-- cw:block <id> -->`, `<!-- cw:<ref> -->`), and run `cw adopt` to initialize `.claimwire/evidence.lock.json` with zero byte diffs.
2. **Phase 2 (Deterministic Maintenance — `cw` Rust Compiler, Zero LLM):** The `cw` binary deterministically checks, evaluates, audits, and patches Markdown from immutable run manifests.

---

## Core Invariants (Never Violate)

1. **Idempotence:** Running `cw sync` twice in a row with unchanged evidence is a strict byte-for-byte no-op (`0` files written).
2. **Non-Interference:** Bytes outside Claim Wire's owned spans, mapped table cells, and block pattern slots are never modified.
3. **No Silent Weakening:** Never weaken a `const` bound, `assert` invariant, or surrounding prose context (`CW4002`) to hide a regression (`CW3002`). Always surface regressions to the human maintainer.
4. **Single-Rounding Determinism:** All derivations (`speedup`, `throughput`, `share`, `prefix_sum`) are evaluated in full `f64` / `i64` precision from canonical units (`ns`, `B`, `cycles`) and rounded (`half_to_even`) once at the display boundary.
5. **Units Inside Braces:** Every format specification for `Duration`, `Bytes`, and `Throughput` must place the scaling unit **inside** the braces (`{prove_wall:.2f s}`, `{proof_size:, B}`, `{throughput:.0f k cycles/s}`). Putting a unit outside the braces is a `CW2005` error.

---

## Phase 1: Bootstrapping & Discovery Workflow

### Step 1: Classify Candidate Numbers
When auditing a Markdown repository, classify every number into one of four buckets:
- **`const` (Specification / Paper Bound):** Fixed protocol parameters or external paper figures (`paper.t22.proof_size_max: { type: Bytes, value: "599 KiB" }`).
- **`source` / `family` (Measured Observation):** Empirical outputs from benchmarks, test runners, or validation scripts (`guest_bench[workload, profile]`, `phase_bench[phase]`, `inventory.rust_tests`).
- **`derive` (Pure Formula):** Values computed from other claims or epochs (`cycles / prove_wall`, `sum(phase_bench[*].duration)`, `prefix_sum(duration)`). Never store derived ratios as independent raw sources if their operands are already tracked.
- **Ignore (Structural Prose):** Section numbers, RFC numbers, dates, bit-widths (`GF(2^128)`, `SHA-256`, `RV64I`), and CLI flags.

### Step 2: Deduplicate into Parameterized Claim Families
Whenever a table or set of tables reports metrics over structured entities (e.g., workloads × profiles, prover phases, test vectors, soundness ledger rows), define a **Family** in `.claimwire/claims.yaml` instead of flat scalar claims:
```yaml
families:
  guest_bench:
    producer: om.benchmarks
    dimensions: [workload, profile]
    dimension_aliases:
      workload:
        "fib(19)": ["fibonacci, fib(19) = 4181"]
        "sha256(\"abc\")": ["sha256 of \"abc\""]
    metrics:
      cycles:      { type: Cycles,   format: "{:,}" }
      prove_wall:  { type: Duration, format: "{:.2f s}", deadband: 2.5% }
      proof_size:  { type: Bytes,    format: "{:, B}" }
    derivations:
      throughput:
        type: Throughput
        expr: "cycles / prove_wall"
        format: "~{:.0fk cycles/s}"
```

### Step 3: Select the Right Markdown Lens
1. **GFM Pipe Tables (`<!-- cw:table <lens-id> -->`):**
   Place a single `<!-- cw:table <id> -->` comment immediately above the table header. Do **not** inject inline HTML comments inside mapped table cells.
   - Use `mode: records` (default) when rows are entities (`[Guest, Profile]`) and columns are metrics. Use `default_scope: { profile: test }` if a dimension column is omitted.
   - Use `mode: transposed` when rows are metrics (`T (cycles, padded)`, `Prove (wall)`, `Peak memory`) and columns are scopes/epochs (`provable-120`, `test`, `Initial`).
   - Use `mode: checklist` when each row binds to a heterogeneous scalar or family claim (`bench.test_suites`, `bench.paper_comparison`, `bench.phase_breakdown`).
2. **Fenced Code Blocks & Clean Prose (`<!-- cw:block <lens-id> -->`):**
   Never place `<!-- cw:... -->` inside a fenced code block (```` ```sh ````) because CommonMark renders HTML comments inside code blocks as literal text. Place `<!-- cw:block <id> -->` immediately above the code fence and declare invertible line templates in `blocks:`.
3. **Inline Prose Spans (`<!-- cw:<ref> -->...<!-- /cw -->`):**
   Use inline spans for numbers embedded in sentences (`<!-- cw:inventory.rust_crates -->16<!-- /cw -->`). Keep outer bold markers (`**...**`) outside the span when using default formats.

### Step 4: Bootstrap with `cw adopt` and Verify with `cw check`
1. Run `cw adopt` to parse existing Markdown values via your bidirectional format templates and populate `.claimwire/evidence.lock.json` without modifying any Markdown bytes.
2. Run `cw check` to verify that all bindings, dimensional types, and assertions pass—or to surface existing live drift (`CW3003`) across documents.

---

## Phase 2: Repairing `CWxxxx` Diagnostics

| Code | Category | Agent Repair Action |
|---|---|---|
| `CW1001..CW1006` | Syntax & Lens Placement | Fix malformed `<!-- cw:... -->` tags, close unclosed spans (`<!-- /cw -->`), or place `<!-- cw:table -->` / `<!-- cw:block -->` directly above the target table or block. |
| `CW1007` | Non-Invertible Format Pattern | Separate adjacent placeholders with a non-numeric literal character or unit inside braces so the pattern has a unique reverse parse. |
| `CW1008` | Pipe Splits Code Span in Table | Escape literal pipes inside backtick code spans in GFM table cells (`\|`, e.g. `` `2k/\|K\|` ``) so GitHub does not split the row into extra cells. |
| `CW2001..CW2005` | Type, Cycle, Scope, or Unit Error | Align dimensional types (`Cycles / Duration -> Throughput`), add `sum`/`min`/`max` over wildcard/subset selectors, and move units inside braces (`{:.2f s}`). |
| `CW2104` | Missing Evidence Key | Either add the missing scope/metric to the producer's `@cw:record` output or add the cell's placeholder string (`—`, `N/A`, `exact`) to the table lens's `sentinels` list. |
| `CW3001` | Stale Evidence | Re-run the producer via `cw capture --producer <id> -- <cmd>` (or inspect impacted claims with `cw impact`), then run `cw sync`. |
| `CW3002` | Assertion Failed | **Stop and report to the user.** Do not weaken the assertion or constant bound in `.claimwire/claims.yaml` without explicit maintainer approval. |
| `CW3003` | Out of Sync | Run `cw sync` to deterministically patch the outdated Markdown spans/tables/blocks from current evidence. |
| `CW3004` | Non-Power-of-Two Value | Either switch the format spec from `:pow2` to `{:,}` or fix the upstream cycle count. |
| `CW4001` | Manual Edit Conflict | Inspect whether the human intentionally edited the number in Markdown. Run `cw sync --force` to restore from evidence, or update the upstream constant/evidence if intentional. |
| `CW4002` | Surrounding Context Drift | Inspect the surrounding sentence/row edit. If the prose change altered the claim's meaning (e.g. `T = 2^22` $\rightarrow$ `T = 2^24`), update the claim binding; if it was a harmless copy-edit, run `cw sync --ack-context`. |
