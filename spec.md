# Claim Wire (`cw`) — Architecture & Technical Specification (`v0.4`)

> **Positioning:** An incremental compiler for evidence-backed repository documentation.<br/>
> **Core Principle:** Markdown is a rendering target, not the authoritative store for machine-maintained facts.<br/>
> **Status:** Draft. No implementation exists yet. The key words MUST, SHOULD, and MAY are used as in RFC 2119.

<details>
<summary><b>Changes since v0.3</b></summary>

- **§5.2.1:** The table lexer is now GFM-conformant (pipes split cells even inside code spans), plus a new `CW1008` lint. The v0.3 backtick-aware lexer disagreed with GitHub's renderer.
- **§4.3:** Selector grammar fixed. Point, subset (`{a, b}`), and wildcard selectors are now distinct. Dimension binding, quoting, and reserved words are specified. Hyphenated keys are legal.
- **§4.5 (new):** Format-pattern mini-language. Units live inside braces, whitespace is significant, invertibility is checked (`CW1007`), and freshness uses rendered-bytes equality. New codes `CW2005` and `CW3004`.
- **§6.2:** Fingerprinting soundness rule, per-language normalization requirements, racy stat-cache guard, and non-file inputs.
- **§6.3:** Deadband consistency. Held values propagate to derived renderings, and assertions always see fresh evidence.
- **§6.4–6.5:** Captures write immutable per-run manifests. `evidence.yaml` schema added.
- **§7–8:** Example formats normalized to §4.5. Missing `soundness_ledger` family and transposed-table `column_overrides` added. Lockfile values are now in canonical units.
- **§12 (new):** Open questions.
- Corrected cross-references (§1.1) and the `15-testing.md` line citation (`:19`, not `:12`). Fixed a broken nested code fence in §5.3.

</details>

---

## 1. First-Principles Problem Analysis

Technical repositories continuously assert empirical and structural facts in Markdown (`README.md`, `docs/BENCHMARKS.md`, `docs/VALIDATION.md`, `docs/spec/*.md`): benchmark latencies, memory footprints, proof/binary sizes, throughput rates, speedup ratios across optimization epochs, test/pass ratios, and mathematical soundness bounds.

Existing approaches fail because they misidentify the abstraction boundary:
1. **Manual synchronization** treats Markdown as the source of truth, leading to silent drift and cross-document contradictions (for example, in `open-magnetar`, `README.md:18` and `docs/BENCHMARKS.md:185` report `227` Python reference tests, while `docs/spec/15-testing.md:19` still says `192 tests`).
2. **Unconstrained AI doc-rewriters** treat documentation maintenance as a text-generation problem. An LLM cannot deterministically answer what code change invalidated which number, and worse, it can silently weaken a claim or acceptance criterion to make prose match a regression.
3. **Naive template generators** destroy readable Markdown by replacing ordinary files with templates (`README.md.tera`) or littering every table cell with hundreds of characters of inline metadata.

### 1.1 The Nine Real-World Bottlenecks ("The Salt")
We stress-tested our specification against **146 tables (1,064 rows) and prose/code blocks across four real repositories** (`open-magnetar` in Rust/Python/C, `cuda-metal` in C++/CUDA/Metal, `arturo-compiler` in Arturo/C++26, and `BatteryUDE.jl` in Julia). Designing Claim Wire from first principles requires solving the nine concrete bottlenecks exposed by those repositories:

| # | Bottleneck | Real-World Failure Case | Claim Wire First-Principles Solution |
|---|---|---|---|
| **1** | **Combinatorial YAML Explosion** | `magnetar` has 15 workloads × 2 profiles × 9 metrics + 12 prover phases across 3 epochs = 300+ values. Scalar YAML declarations would exceed 1,500 lines. | **Parameterized Claim Families (§4.2):** Declare a family once over scope dimensions (`[workload, profile]`); address any cell or derivation as `guest_bench[radix-sort-u64, test].throughput`. |
| **2** | **Markdown Table Clutter & Three Table Orientations** | Wrapping 60 table cells in `<!-- cw:... -->` makes raw Markdown 350 columns wide. Worse, assuming `rows = entities, cols = metrics` breaks on transposed pivot tables (`BENCHMARKS.md:142, 211`) and heterogeneous checklists (`BatteryUDE.jl/README.md:37`). | **Multi-Mode Table Lenses (§5.2):** A single `<!-- cw:table <id> -->` comment above a clean GFM pipe table supports three orientations (`records`, `transposed`, `checklist`) with zero inline HTML inside mapped cells. |
| **3** | **Claims Inside Fenced Code Blocks** | `magnetar/README.md:18` asserts `(362 Rust, 227 Python, 46 ISA tests)` inside a ```` ```sh ```` code block. HTML comments `<!-- cw:... -->` inside ```` ```sh ```` render as visible literal text on GitHub. | **Block Pattern Lenses (§5.3):** Placing `<!-- cw:block <id> -->` *above* a fenced code block (or paragraph/list) binds interior substrings via an invertible line template without injecting HTML comments inside the block. |
| **4** | **GFM Pipe-Table Lexing Traps** | `04-pcs.md:245` has *unescaped* pipes inside backticks (`` `2k/\|K\|` ``), which GitHub splits into extra cells and silently truncates; `cuda-metal` and `arturo-compiler` have lone `$` signs (`'$L__BB19_1'`, `` `$` ``) in table cells. | **GFM-Conformant Row Lexer + Lint (§5.2.1):** Split rows exactly as GitHub does (only `\|` escapes a pipe, even inside code spans), never let `$` mask a delimiter, and flag code spans broken by an unescaped pipe as `CW1008` instead of silently disagreeing with the renderer. |
| **5** | **Non-Uniform Row Keys & Omitted Scope Columns** | `fib(19)` is written as `` `fib(19)` `` (`README.md:44`), `fibonacci, fib(19) = 4181` (`BENCHMARKS.md:18`), and `` `radix-sort-u64` (`A = 17`) `` (`README.md:50`). `BENCHMARKS.md:33` omits the `Profile` column entirely (`test` is implicit). | **Two-Step Key Canonicalization + `default_scope` (§5.2.2):** Strip all inline decorators (`` ` ``, `*`, `_`), strip whitespace-preceded parenthetical notes (`\s+\([^)]*\)$`), apply dimension aliases, and fill omitted dimensions from `default_scope`. |
| **6** | **Cross-Epoch Baselines, Prefix Sums & Subset Sums** | `BENCHMARKS.md:152` compares `@current`, `@prev`, and `@initial` epochs; all 36 `Ends at` cells are cumulative prefix sums of `duration`; `BENCHMARKS.md:222` sums subsets of phases (`register_twist + ram_twist + bytecode_shout = 2.38 s`). | **Named Evidence Epochs (§6.1) + Ordered Family Reductions (§4.4):** Native `@current`/`@prev`/`@initial` epoch addressing, `prefix_sum(duration)`, and subset selectors (`sum(phase_bench[{k1, k2}].duration)`). |
| **7** | **Sentinel Cells & Prose-Column Numbers** | `BENCHMARKS.md:172, 199` has `—` for unmeasured rows; `13-security.md:56` has `exact`; `BENCHMARKS.md:224` has prose column `Notes on Remaining Gap` citing `(0.79 s)`, `(1.39 s)`, `(0.20 s)`. | **Sentinel Preservation (§5.2.3) + Composable Prose Columns (§5.2.4):** Preserve declared sentinels (`—`, `N/A`, `exact`) when a metric is `null`, and allow inline spans or pattern templates inside unmapped prose columns and headers. |
| **8** | **False Invalidation & Thermal Churn** | Editing a comment in `crates/om-spartan/src/lib.rs` invalidates a 10-minute benchmark; $\pm 2\%$ wall-clock noise causes pointless Git diffs. | **Semantic Token Fingerprinting (§6.2) + Deadband Hysteresis (§6.3):** Strip comments/whitespace before hashing code inputs (`<10 ms` via stat cache); suppress Markdown churn within declared deadband (`2.5%`). |
| **9** | **Surrounding Prose Drift** | A span renders `~491k cycles/s` (`T = 2^22, test`), but someone edits the surrounding sentence to `At T = 2^24 (provable-120)` without touching the span. | **Context Window Anchoring (§8.3):** Lockfile records a normalized `context_digest` of the enclosing sentence/row outside owned spans and raises `CW4002 (Context Drift)` when surrounding qualifiers change. |

---

## 2. System Model & Core Invariants

### 2.1 The Claim Tuple & Dual Graphs
Every tracked statement in a repository is modeled as a five-tuple:

```text
Claim = (Identity, Type, Scope, Derivation | Authority, Policy)
```

Claim Wire maintains two dual graphs over these claims:
1. **Provenance Graph (Backward / Audit):**
   ```text
   Rendered Bytes → Invertible Format Pattern → Claim Node → Scoped Evidence Record(s) → Run Manifest + Git SHA + Input Digests
   ```
2. **Invalidation Graph (Forward / Incremental Build):**
   ```text
   Modified Source File → Semantic Input Digest → Producer (@current) → Claim Family → Derived Claims & Assertions → Markdown Spans, Table Cells & Code Blocks
   ```

> [!IMPORTANT]
> **Avoiding the Git SHA Circularity Trap:**
> Run manifests record the Git commit SHA for **provenance**, but **freshness** is computed exclusively from the `blake3` digest of the producer's declared input files. Committing an updated `README.md` changes `HEAD` without invalidating any benchmark evidence.

### 2.2 Five Non-Negotiable Invariants
1. **Idempotence:** Running `cw sync` twice with unchanged inputs and evidence produces zero byte changes.
2. **Non-Interference:** `cw` never modifies bytes outside explicitly owned inline spans (`<!-- cw:... -->...<!-- /cw -->`), mapped cells of a `<!-- cw:table ... -->` pipe table, or matched slots of a `<!-- cw:block ... -->` lens. All unregistered prose, whitespace, and line endings are preserved byte-for-byte.
3. **No Silent Weakening:** When an assertion (`assert`) fails—such as a speedup dropping below `1.0×`, a pass ratio dropping from `83/83` to `82/83`, or a proof size exceeding a paper bound—`cw` MUST refuse to publish the broken value or weaken the predicate, halting with diagnostic `CW3002`.
4. **Single-Rounding Determinism:** Intermediate `derive` expressions evaluate in unrounded canonical precision (`i128` scaled rationals / IEEE 754-2008 `f64`), with explicit support for `round(expr, decimals)` when a table derives from rounded display columns. Final formatting rounds (`half_to_even`) strictly at the display boundary.
5. **Crash-Safe Transactionality:** Multi-file Markdown patches and `evidence.lock.json` updates are staged and committed via an atomic write-ahead journal in `.claimwire/.staging/`.

---

## 3. Three-Layer Architecture & Two-Phase Workflow

```mermaid
flowchart TB
    subgraph Phase1["Phase 1: Probabilistic Discovery (Agent Skill + Human Review)"]
        AGENT["claim-wire Agent Skill<br/>(Discovers claims across prose/tables/code blocks,<br/>deduplicates families, proposes .claimwire/*.yaml)"]
    end

    subgraph Authoring["1. Authoring Surface (Git-Tracked)"]
        MD["Ordinary Markdown<br/>(README.md, docs/*.md, docs/spec/*.md)"]
        YAML[".claimwire/evidence.yaml<br/>.claimwire/claims.yaml"]
    end

    subgraph Compiler["Phase 2: Deterministic Maintenance (cw Rust Compiler, Zero LLM)"]
        SYN["cw-syntax<br/>Lossless Markdown CST, GFM Table Lexer & Block Lens Parser"]
        IR["cw-ir<br/>Dimensional Type Checker, Family Expander & DAG"]
        EVID["cw-evidence<br/>Two-Tier Semantic Fingerprinter & Epoch Selector"]
        ENG["cw-engine<br/>Topological Evaluator, Bidirectional Formatter & Patcher"]
    end

    subgraph Store["3. Materialization & Provenance (Git-Tracked)"]
        RUNS[".claimwire/runs/*.json<br/>Immutable Run Manifests (@current, @prev, @initial)"]
        LOCK[".claimwire/evidence.lock.json<br/>Pinned Values, Span & Context Digests"]
    end

    AGENT -.->|"bootstraps / repairs"| MD
    AGENT -.->|"bootstraps / repairs"| YAML
    MD --> SYN
    YAML --> IR
    RUNS --> EVID
    SYN --> ENG
    IR --> ENG
    EVID --> ENG
    ENG -->|"transactional patch"| MD
    ENG -->|"commits state"| LOCK
```

---

## 4. Normative Specification: Type System, Families & Reductions (`cw-ir`)

### 4.1 Dimensional Type System & Engineering Formats
Every value in Claim IR has a static `ValueType`. Combining incompatible types (e.g., `Duration + Count`) is a compile-time error (`CW2001`).

| `ValueType` | Canonical Storage | Supported Unit Suffixes & Format Modifiers | Dimensional Algebra |
|---|---|---|---|
| `Duration` | `f64` nanoseconds (`ns`) | `ns`, `us`/`µs`, `ms`, `s`, `min`, `auto_time` | `Duration ± Duration -> Duration`<br>`Duration / Duration -> Ratio` |
| `Bytes` | `u64` / `f64` bytes (`B`) | `B`, `KiB` ($2^{10}$), `MiB` ($2^{20}$), `GiB` ($2^{30}$), `KB`, `MB`, `GB`, `auto_iec` | `Bytes ± Bytes -> Bytes`<br>`Bytes / Bytes -> Ratio` |
| `Cycles` | `u64` cycles (`cycles`) | `cycles`, `k cycles` ($10^3$), `M cycles` ($10^6$), `pow2` (`2^22`) | `Cycles / Duration -> Throughput` |
| `Throughput` | `f64` cycles/sec (`cycles/s`) | `cycles/s`, `k cycles/s` ($10^3$), `M cycles/s` ($10^6$) | `Throughput / Throughput -> Ratio` |
| `Count` | `i64` integer | raw (`362`), comma-grouped (`4,194,304`) | `Count ± Count -> Count` |
| `PassRatio` | `(u64, u64)` (`passed, total`) | `{passed}/{total}` (`83/83`, `50/52`), `{rate:.1f}%` | `passed == total -> Bool` |
| `Ratio` / `Float` | `f64` dimensionless | `×` / `x` (`12.5×`), `%` (`27%`), scientific (`{:.3e}`, `1.166e-15`), `neg_pow2` (`2^-120`) | `Ratio * Ratio -> Ratio` |
| `Quantity<U>` | `(f64, UnitTag)` | User-declared domain units (`V`, `mV`, `Ah`, `bits`, `bits/cycle`) | Same-unit `±`, `/` $\rightarrow$ `Ratio` |
| `Bool` | `bool` | `true`/`false`, or mapped (`Yes\|No`, `Accepted\|Rejected`) | `and`, `or`, `not` |
| `String` / `Digest` | UTF-8 / Hex string | literal, `hex8` (`ba7816bf…`) | `==`, `!=` |

### 4.2 Epistemic Primitives & Parameterized Claim Families
`.claimwire/claims.yaml` expresses all tracked facts using four primitives at either the **scalar** or **family** level:
1. **`const` (Declaration):** Maintainer-approved external constant or specification bound (`paper.t22.proof_size_max = 599 KiB`).
2. **`source` / `family` (Observation):** Extracts typed metrics from a producer's run manifest, indexed by structured `dimensions` (`[workload, profile]`, `[phase]`, `[case]`).
3. **`derive` (Derivation):** Pure mathematical expression over claims, family metrics, or cross-epoch references (`@current`, `@prev`, `@initial`).
4. **`assert` (Verification):** Boolean invariant that must evaluate to `true` for dependent bindings to verify or sync. A binding *depends* on an assertion when they read the same evidence: the producers and constants an expression reaches, following `derive` expressions and family derivations transitively. A failing assertion therefore holds back only those bindings; `cw sync` still patches unrelated locations, reports the held ones, and exits `1`.

### 4.3 Claim Addressing Grammar
```ebnf
ClaimRef     ::= ScalarRef | FamilyRef ;
ScalarRef    ::= Identifier ("." Identifier)* ;                       (* inventory.rust_tests *)
FamilyRef    ::= Identifier ("@" EpochId)? "[" SelectorList "]" "." Identifier ;
EpochId      ::= "current" | "prev" | "initial" | Identifier ;
SelectorList ::= Selector ("," " "* Selector)* ;                      (* exactly one Selector per family dimension *)
Selector     ::= "*" | Key | "{" Key ("," " "* Key)* "}" ;            (* wildcard | point | subset *)
Key          ::= BareKey | QuotedString | Integer ;
BareKey      ::= [A-Za-z0-9_] [A-Za-z0-9_.+-]* ;                      (* radix-sort-u64, provable-120 *)
Identifier   ::= [A-Za-z_] [A-Za-z0-9_]* ;
QuotedString ::= '"' ( [^"\\|] | '\\' ["\\] )* '"' ;                  (* "fib(19)", "sha256(\"abc\")" *)
```

**Resolution rules:**
- **Positional selectors.** A family with dimensions `[workload, profile]` takes exactly two selectors. A selector count mismatch is `CW2003`.
- **Points, subsets, and wildcards are syntactically distinct.** `phase_bench[ram_twist]` is one record. `phase_bench[{register_twist, ram_twist}]` is a two-record subset. `guest_bench[*, test]` is every workload at `profile = test`. A reduction (`sum`, `min`, `max`) is required whenever a selector yields more than one record, otherwise `CW2004` is raised.
- **Dimension binding inside family derivations.** Within a family's `derivations`, a `BareKey` that equals one of the *enclosing* family's dimension names binds to the current record's value for that dimension (`guest_bench@initial[workload, test]` means "this row's workload, `test` profile"). To select a literal key that collides with a dimension name, quote it (`"workload"`).
- **Keys that are not `BareKey`s must be quoted**, e.g. `guest_bench["fib(19)", test].cycles`. Pipes are never allowed in keys, so a reference can always appear inside a GFM table cell.
- **Reserved words.** `table` and `block` followed by whitespace are lens directives (`<!-- cw:table … -->`), so neither may be the first segment of a `ScalarRef`.
- **Canonical serialization.** Lockfile keys and diagnostics print references with a single space after each comma and quotes only where required: `guest_bench[radix-sort-u64, test].throughput`.

### 4.4 Family Reductions & Ordered Aggregations
Real benchmark tables frequently aggregate across rows of a family (verified on `magnetar/docs/BENCHMARKS.md` L152 and L222):
- **Total Aggregation (`sum`, `min`, `max`):**
  - `sum(phase_bench[*].duration)` $\rightarrow$ sums `duration` across all 12 phases (`8.76 s` at `@current`, `20.62 s` at `@prev`).
  - `min(soundness_ledger[*].per_round_bits)` $\rightarrow$ `120.0` (used to assert `>= 120.0` across all 40 ledger rows in `13-security.md`).
  - Reductions skip records whose metric is `null` (sentinel rows such as `exact`). A reduction over an empty set is `CW2104`, never a silent `0`.
- **Subset Aggregation (`sum(family[{k1, k2, ...}].metric)`):**
  - `sum(phase_bench[{register_twist, ram_twist, bytecode_shout}].duration)` $\rightarrow$ `0.79 s + 1.39 s + 0.20 s = 2.38 s` (`Twist & Shout` in `BENCHMARKS.md:224`).
  - `sum(phase_bench[{execute, packed_witness, component_witnesses, build_g, routers}].duration)` $\rightarrow$ `1.69 s` (`Execute + Witness + Routers` in `BENCHMARKS.md:226`).
- **Ordered Prefix Sum (`prefix_sum(metric)`):**
  - Evaluates the running cumulative sum of `metric` in the manifest's record order (the order in which `@cw:record` frames were emitted). In `BENCHMARKS.md:152`, `ends_at: "prefix_sum(duration)"` automatically computes all 36 `Ends at` cells across `@current` (`0.76 s, 0.89 s, ..., 8.76 s`), `@prev` (`0.75 s, ..., 20.62 s`), and `@initial` (`0.8 s, ..., 107.0 s`).
  - Because record order is semantic for `prefix_sum`, a run whose record order differs from the epoch it replaces is reported as a **Definition Changed** event by `cw diff`, never silently re-summed.

### 4.5 Format Patterns (Bidirectional Rendering)
Every rendered value passes through a **format pattern**. Patterns are bidirectional: `cw sync` uses them to render, and `cw adopt` / `cw check` use them to parse existing Markdown back into structured slots.

```ebnf
Pattern     ::= (Literal | Placeholder)* ;
Placeholder ::= "{" ClaimRefOrField? (":" Spec)? "}" ;
Spec        ::= Grouping? Precision? Kind? Unit? | NamedFormat ;
Grouping    ::= "," ;                                  (* 4,194,304 *)
Precision   ::= "." Digit+ ;
Kind        ::= "f" | "e" ;                            (* fixed | scientific *)
Unit        ::= <remaining text up to "}", whitespace significant> ;
NamedFormat ::= "auto_iec" | "auto_time" | "pow2" | "neg_pow2" | "hex8" ;
Literal     ::= <any text; "{{" and "}}" escape braces> ;
```

1. **Units live inside the braces.** The unit selects the scaling from canonical storage (§4.1), so `{:.2f s}` renders a `Duration` stored as `8.55e9 ns` as `8.55 s`. Text *after* the closing brace is a plain literal and never scales anything. A unit in square brackets scales without being rendered: `{:.3f [ms]}` renders `406.266` for the bare-number cells of a `Time (ms)` column, and is type-checked like any other unit. For `Duration`, `Bytes`, `Throughput`, and `Quantity<U>`, a numeric spec without a unit, or with a unit invalid for the type, is a compile-time error (`CW2005`). `Count`, `Cycles`, `Ratio`, and `PassRatio` may omit the unit and render in canonical units.
2. **Whitespace inside the spec is rendered verbatim.** `{:.0f k cycles/s}` renders `476 k cycles/s` (the `BENCHMARKS.md` style) and `{:.0fk cycles/s}` renders `491k cycles/s` (the `README.md` style). The same claim can feed both documents with different patterns.
3. **Scaling units.** `%` multiplies a `Ratio` by 100 and `×`/`x` by 1. `k`, `M`, `KiB`, `MiB`, and `GiB` are fixed scale factors, as listed in §4.1.
4. **Named formats.** `auto_iec` picks the largest IEC unit with magnitude ≥ 1 and renders two significant digits without ever truncating the integer part (`16 MiB`, `8.3 GiB`, `112 MiB`). `pow2` renders `2^k` and raises `CW3004` if the value is not an exact power of two.
5. **Rounding.** `half_to_even` is applied exactly once, at the display boundary (Invariant 4).
6. **Invertibility (`CW1007`).** Each placeholder compiles to a regular language: an optionally signed digit run, where `,` and `.` continue the number only when a digit follows them, and `e`/`E` only when an optionally signed digit follows, then its unit (a fixed string, or a finite prefix-free set for `auto_iec`/`auto_time`). A whole pattern is accepted only if every matching string has exactly one parse. In practice, the text after a numeric portion (its own unit, or the next literal) must not be able to continue it. So `{passed}/{total}`, `{n}-file`, `{p}/{t}.` and `{verify_wall:.0f ms} ({verify_rss:.0f MiB})` are valid, while `{a}{b}`, `{a}.{b}` and `{n}5` are rejected.
7. **Declared formats are inherited.** A placeholder written without a spec (`{matmul.tile_vs_article}`, or a bare family field `{share}` in a family table) renders and parses with that claim's declared `format:`, including the format's literals (`~`, `×`), exactly as an inline span of the claim would.
8. **Equality is rendered-bytes equality.** A location is *in sync* if and only if rendering the current claim value produces exactly the bytes in the file (after decorator stripping, §5.2.2). Parsed numbers are used only for `cw adopt` bootstrapping and diff reporting, never to decide freshness. This avoids false "out of sync" reports caused by display rounding (`8.55 s` parsed back is not equal to `8.5512 s` evidence).

---

## 5. Normative Specification: Lossless Markdown, Table Lenses & Block Lenses (`cw-syntax`)

### 5.1 Inline Span Grammar (`<!-- cw:... -->...<!-- /cw -->`)
For numbers embedded in ordinary prose paragraphs, Claim Wire uses standard HTML comments with a universal short closing tag `<!-- /cw -->`. Markers that appear inside fenced or indented code blocks, or inside inline code spans, are plain text and never directives, so documentation *about* Claim Wire can quote its own syntax safely:
```markdown
That made <!-- cw:inventory.table1_proved -->11<!-- /cw --> of the <!-- cw:inventory.table1_workloads -->15<!-- /cw --> Table 1 workloads proved end to end.

At `T = 2^22`, throughput is **<!-- cw:guest_bench[radix-sort-u64, test].throughput -->~491k cycles/s<!-- /cw -->** (`test`)
and **<!-- cw:guest_bench[radix-sort-u64, provable-120].throughput -->~476k cycles/s<!-- /cw -->** (`provable-120`).
```

### 5.2 Multi-Mode Table Lenses (`<!-- cw:table <lens-id> -->`)
Placing `<!-- cw:table <lens-id> -->` immediately above a GFM pipe table binds the table without injecting HTML comments inside mapped cells. Claim Wire supports three table orientations:

1. **`mode: records` (Row-per-Entity Relational Table — Default):**
   - Rows identify scope keys (`[Guest, Profile]` or `[Phase]`); columns map to metric templates (`Prove`, `Peak RSS`, `Verify`, `Proof Size`).
   - Supports `default_scope: { profile: test }` when a dimension column is omitted from the table (e.g., `BENCHMARKS.md:33` and `BENCHMARKS.md:52`).
2. **`mode: transposed` (Metric-per-Row Pivot Table):**
   - Rows identify distinct metrics (`T (cycles, padded)`, `Prove (wall)`, `Peak memory`, `Verify`, `Proof size`), each with its own type and template; columns identify scopes or epochs (`provable-120`, `test`, `Previous`, `Initial`), solving `BENCHMARKS.md:142` and `BENCHMARKS.md:211`.
   - Per-cell exceptions are declared with `row_overrides` (records mode, keyed by row scope) or `column_overrides` (transposed mode, keyed by column header, then row).
3. **`mode: checklist` (Heterogeneous Verification Table):**
   - Each row key maps to an independent scalar/family claim and assertion template (solving `BENCHMARKS.md:182` `Test suites`, `VALIDATION.md:30`, `cuda-metal/docs/status.md`, and `BatteryUDE.jl/README.md:37`).

#### 5.2.1 Normative GFM Table Row Lexer State Machine
Claim Wire MUST see exactly the cells a reader sees on GitHub. If `cw` split a row differently from the renderer, it would verify and patch cells that do not exist on the rendered page. Row splitting therefore follows the [GFM tables extension](https://github.github.com/gfm/#tables-extension-) precisely: cells are split **before** inline parsing, so a code span does not protect a pipe.
- Strip one leading and one trailing border pipe (`|`) and surrounding whitespace.
- Scan UTF-8 bytes left-to-right:
  - On `\|`: consume both bytes as part of the current cell (the escaped pipe renders as `|`, including inside code spans).
  - On any other `\` followed by a character: consume both bytes as part of the cell.
  - On `|`: end the current cell and start the next one, **regardless of backticks**.
  - Dollar signs (`$`) and math delimiters NEVER mask `|` delimiters (so assembly labels like `$L__BB19_1` and symbols like `` `$` `` cannot corrupt column counts).
- Cells beyond the header's column count are dropped and cells missing from a short row are empty, as in GFM.
- **Lint `CW1008` (Pipe Splits Code Span):** If a backtick run is still open when an unescaped `|` ends a cell, the author almost certainly meant a literal pipe. `cw check` reports `CW1008` with a suggested `\|` fix and refuses to bind any lens to that row. (`docs/spec/04-pcs.md:245` in `magnetar` currently renders broken on GitHub for exactly this reason.)

#### 5.2.2 Two-Step Key Canonicalization & Decorator Preservation
When resolving a key cell (e.g., `` `radix-sort-u64` (`A = 17`) `` or `fibonacci, fib(19) = 4181`) against a dimension (`workload`) or row map:
1. **Step 1 (Decorator Strip):** Remove all inline Markdown emphasis/code characters (`` ` ``, `*`, `_`) and trim whitespace $\rightarrow$ `radix-sort-u64 (A = 17)`. Match against canonical keys and declared `aliases`.
2. **Step 2 (Parenthetical Qualifier Strip):** If still unresolved, strip trailing parenthetical qualifiers preceded by at least one whitespace character (`\s+\([^)]*\)$`) $\rightarrow$ `radix-sort-u64` (preserving function-call identifiers like `fib(19)` and `sha256("abc")` that have no space before `(`), then match against canonical keys and `aliases`.
3. **Decorator Preservation on Write:** When updating a mapped value cell, if the existing cell is wrapped in outer bold (`**8.55 s**`) or inline code ticks (`` `2^22` ``) and the template does not explicitly include `*` or `` ` ``, the engine preserves the existing outer decorator automatically.

#### 5.2.3 Sentinel Cell Handling (`sentinels`)
Benchmark and specification tables frequently contain sentinel markers for unmeasured or exact entries (`—` in `BENCHMARKS.md:172, 199–201`, `exact` in `13-security.md:56`, `N/A ($t = 8$)` in `BENCHMARKS.md:218`).
- When a table lens declares `sentinels: ["—", "exact", "N/A ($t = 8$)"]` (the default set includes `"—"` and `"N/A"`), if the underlying evidence record is absent (`null`) or flagged `exact`, `cw` verifies that the cell contains an allowed sentinel and preserves it without emitting `CW2104`.

#### 5.2.4 Composable Prose Columns & Header Templates
Inside a `<!-- cw:table <id> -->` table:
- Any cell in an **unmapped column** (such as `Notes on Remaining Gap` in `BENCHMARKS.md:224` or `Gap / Status` in `BENCHMARKS.md:213`) and any **header cell** (`Previous (20.62 s)`) is still scanned for inline `<!-- cw:... -->` spans OR matched against optional `cell_patterns` declared on the table lens.

### 5.3 Block Pattern Lenses (`<!-- cw:block <id> -->`) for Code Blocks & Clean Prose
Putting `<!-- cw:... -->` inside a fenced code block (```` ```sh ````) is forbidden because CommonMark renders HTML comments inside code blocks as visible text.
To wire numbers inside fenced code blocks (such as `README.md:18`) or keep prose paragraphs/lists completely free of inline HTML comments:
````markdown
<!-- cw:block readme.test_suite_comment -->
```sh
# Run Rust, Python, and RV64I conformance + fuzz suites (362 Rust, 227 Python, 46 ISA tests)
cargo test --workspace --release
```
````
In `.claimwire/claims.yaml`:
```yaml
blocks:
  readme.test_suite_comment:
    file: README.md
    lines:
      - "# Run Rust, Python, and RV64I conformance + fuzz suites ({inventory.rust_tests} Rust, {inventory.python_tests} Python, {inventory.isa_tests} ISA tests)"
```
The compiler compiles each line pattern into an unambiguous prefix/infix/suffix matcher (`"# Run Rust, Python, and RV64I conformance + fuzz suites ("` ... `" Rust, "` ... `" Python, "` ... `" ISA tests)"`), locates the matching line inside the block immediately following `<!-- cw:block readme.test_suite_comment -->`, and updates `362`, `227`, and `46` in place.
- "The block" is the single CommonMark block (fenced code block, paragraph, list, or blockquote) that begins on the first non-blank line after the directive.
- Each line pattern MUST match exactly one line in that block. Zero matches or more than one match is `CW1006`.
- Line patterns obey the same invertibility rules as format patterns (§4.5).

---

## 6. Normative Specification: Evidence Store, Epochs & Fingerprinting (`cw-evidence`)

### 6.1 Named Evidence Epochs (`@current`, `@prev`, `@initial`)
Producers bind immutable run manifests to named epochs in `.claimwire/evidence.yaml`:
- **`@current`:** Verified against the working tree's semantic `input_set_digest` during `cw check`. If tracked source inputs change, `@current` is flagged `StaleInputs` (`CW3001`).
- **Historical Epochs (`@prev`, `@initial`):** Pinned snapshots of prior optimization milestones (`107 s`, `20.7 s`). Exempt from current-tree input staleness checks, while providing full provenance for historical comparison columns and speedup derivations (`12.5×`, `6.06×`).
- **Re-binding an epoch is an explicit, reviewable edit.** `cw capture` only ever moves `@current`. Promoting a run to `@prev` or `@initial` is a one-line change to `evidence.yaml` that shows up in code review, never a side effect.

### 6.2 Fast Two-Tier Semantic Input Fingerprinting
**Soundness rule:** normalization MAY over-invalidate (treat a harmless edit as a change) but MUST NEVER under-invalidate (treat a behavior-changing edit as harmless). When in doubt, a normalizer keeps the bytes.

1. **Tier 1 (Stat Cache):** Look up `(rel_path, mtime_ns, byte_len)` in `.claimwire/.cache/fingerprints.bin`. On hit, return cached `semantic_blake3` in $O(1)$ without reading the file (target: `< 10 ms` for a full-workspace `cw check`).
   - **Racy-entry guard:** As in Git's index, an entry whose `mtime_ns` is not strictly older than the cache file's own write time is treated as a miss, because a same-tick edit cannot be distinguished by stat data alone.
2. **Tier 2 (Lexical Token Normalizer):** On cache miss, stream the file through a language-aware lexer that strips comments and canonicalizes whitespace *outside* string and character literals, then hash the token stream with `blake3`. Fixing a typo in a doc-comment or running `cargo fmt` never invalidates benchmark runs. The per-language rules required by the soundness rule are:

   | Language | Must handle |
   |---|---|
   | Rust (`.rs`) | Nested block comments (`/* /* */ */`), raw strings (`r#"…"#`), byte/char literals, lifetimes vs. char literals (`'a` vs `'a'`). Doc comments are stripped (they do not affect codegen). |
   | C/C++ (`.c`, `.h`, `.cc`, `.cpp`, `.hpp`) | Newlines are significant on preprocessor lines and `\` line continuations are honored. Runs of whitespace collapse to a single separator but are never deleted. |
   | Python (`.py`) | Indentation and logical-line boundaries are significant and kept verbatim. Only `#` comments and blank lines are dropped. Docstrings are string literals and **are** hashed. |
   | Julia (`.jl`) | Whitespace runs collapse but are never removed (`[1 -2]` and `[1 - 2]` differ). Nested `#= =#` comments are handled. |
   | Any other file | Hashed as raw bytes (linker scripts, ELFs, test vectors, `Cargo.lock`, `rust-toolchain.toml`). |

3. **Non-file inputs.** Toolchain versions, build profiles, and environment variables that affect results (for example `RUSTFLAGS`) are declared as producer inputs (§6.5) and folded into the `input_set_digest`, since they change behavior without changing any tracked file.

### 6.3 Statistical Hysteresis (`deadband`)
- **Deterministic Metrics (`Bytes`, `Cycles`, `Count`, `PassRatio`, `Digest`):** Default `deadband: 0%`. Any 1-byte proof size change or 1-test count change is caught immediately.
- **Empirical Timing / Memory Metrics (`Duration`, and `Bytes` metrics such as `peak_rss`):** Can declare `deadband: 2.5%`. When a new run is within $\pm 2.5\%$ of the **locked** value (not the previous run, so slow drift cannot accumulate unseen) and all `assert` guards pass, `cw` avoids churning the Markdown file unless `--exact` is passed.
- **Held values propagate (internal consistency):** When a metric is held inside its deadband, every derived claim *rendered* from it uses the held value too. A document can therefore never show `8.55 s` next to a throughput computed from a fresh `8.60 s`. Deadband decisions are made per evidence record, and if any metric of a record leaves its band, the whole record and its derived cone re-render together.
- **Assertions never see held values:** `assert` predicates are always evaluated against fresh, unrounded evidence. A regression hidden inside a display deadband still fails `CW3002`. This is what makes "No Silent Weakening" hold under hysteresis.

### 6.4 The `@cw:` Stdout Wire Protocol (`cw capture`)
Any benchmark or test binary can emit single-line telemetry frames to stdout/stderr without linking an SDK:
```text
@cw:record family="guest_bench" workload="radix-sort-u64" profile="test" cycles=4194304 prove_wall="8.5487s" peak_rss="8912057139B" verify_wall="10.4ms" proof_size="60806B"
```
- Producers SHOULD emit full-precision values. Rounding is the format pattern's job (§4.5), and pre-rounded evidence compounds rounding error in derivations.
- Frames are validated against the producer's declared families: an unknown family, an unknown metric, a missing dimension, or a unit incompatible with the metric's type rejects the whole capture. Nothing is written.
- `cw capture --producer om.benchmarks -- <cmd>` intercepts `@cw:` lines and records OS/CPU/RAM metadata, the Git commit SHA plus a `git_dirty` flag, and the semantic input digests. It then writes a **new, immutable** manifest at `.claimwire/runs/<producer>/<run-id>.json` (for example `runs/om.benchmarks/bench-20261016-m4pro.json`) and atomically re-points `@current` to it. Existing manifests are never overwritten.

### 6.5 Producer Declarations (`.claimwire/evidence.yaml`)
```yaml
schema: claimwire/v1

producers:
  om.benchmarks:
    command: ["cargo", "run", "-p", "om-bench", "--release", "--", "--emit-cw"]
    families: [guest_bench, phase_bench]
    inputs:
      files:
        - "crates/**/*.rs"
        - "guest/**/*.{c,h,ld}"
        - "Cargo.lock"
        - "rust-toolchain.toml"
      env: [RUSTFLAGS]
    on_stale: error        # error | warn — see §12
    epochs:
      current: bench-20261016-m4pro
      prev:    bench-20261015-m4pro
      initial: bench-20261014-m4pro

  om.inventory:
    command: ["python3", "tools/inventory.py", "--emit-cw"]
    families: []           # scalar claims only (claims.yaml `kind: source`)
    inputs:
      files: ["crates/**/*.rs", "reference/tests/**/*.py", "tests/vectors/**"]
    epochs:
      current: inventory-20261016

  om.soundness:
    command: ["python3", "reference/openmagnetar/params.py", "--emit-cw"]
    families: [soundness_ledger]
    inputs:
      files: ["reference/openmagnetar/params.py"]
    epochs:
      current: soundness-20261016
```
`inputs.files` globs are evaluated relative to the repository root. Their sorted `(rel_path, semantic_blake3)` pairs, the declared `env` values, and the producer's own definition are hashed together into `input_set_digest`. Editing the `command` or `inputs` of a producer therefore invalidates it, as it should.

---

## 7. Complete Example: `.claimwire/claims.yaml` for `magnetar`

```yaml
schema: claimwire/v1

aliases:
  workload:
    "fib(19)": ["fibonacci, fib(19) = 4181"]
    "sha256":  ['sha256("abc")', 'sha256 of "abc"']
  phase:
    "ram_twist":     ["RAM Twist"]
    "ligerito_open": ["ring switching + Ligerito open"]

constants:
  paper.t22.proof_size_max: { type: Bytes, value: "599 KiB" }
  paper.t22.verify_ms:      { type: Duration, value: "13 ms" }
  paper.t22.soundness_bits: { type: Quantity<bits>, value: "120.0 bits" }

families:
  guest_bench:
    producer: om.benchmarks
    dimensions: [workload, profile]
    metrics:
      cycles:      { type: Cycles,   format: "{:,}" }
      prove_wall:  { type: Duration, format: "{:.2f s}", deadband: 2.5% }
      prove_cpu:   { type: Duration, format: "{:.1f s}", deadband: 2.5% }
      peak_rss:    { type: Bytes,    format: "{:auto_iec}", deadband: 2.0% }
      heap_peak:   { type: Bytes,    format: "{:auto_iec}", deadband: 2.0% }
      verify_wall: { type: Duration, format: "{:.0f ms}", deadband: 5.0% }
      verify_cpu:  { type: Duration, format: "{:.0f ms}", deadband: 5.0% }
      verify_rss:  { type: Bytes,    format: "{:.1f MiB}", deadband: 2.0% }
      proof_size:  { type: Bytes,    format: "{:, B}" }
    derivations:
      throughput:
        type: Throughput
        expr: "cycles / prove_wall"
        format: "~{:.0fk cycles/s}"
      speedup_vs_initial:
        type: Ratio
        expr: "guest_bench@initial[workload, test].prove_wall / prove_wall"
        format: "{:.1f×}"
      paper_size_gap:
        type: Bytes
        expr: "paper.t22.proof_size_max - proof_size"
        format: "~{:.0f KiB}"

  phase_bench:
    producer: om.benchmarks
    dimensions: [phase]
    metrics:
      duration:  { type: Duration, format: "{:.2f s}", deadband: 2.5% }
      heap_peak: { type: Bytes,    format: "{:.1f GiB}", deadband: 2.0% }
    derivations:
      ends_at:
        type: Duration
        expr: "prefix_sum(duration)"
        format: "{:.2f s}"
      share_of_total:
        type: Ratio
        expr: "duration / sum(phase_bench[*].duration)"
        format: "{:.0f%}"
      speedup_vs_prev:
        type: Ratio
        expr: "phase_bench@prev[phase].duration / duration"
        format: "{:.2f×}"

  soundness_ledger:
    producer: om.soundness
    dimensions: [row]
    metrics:
      # `null` on rows whose error term is exact; rendered as the `exact` sentinel (§5.2.3)
      per_round_bits: { type: Quantity<bits>, format: "{:.1f bits}" }

claims:
  inventory.rust_crates:      { kind: source, producer: om.inventory, metric: rust_crates, type: Count }
  inventory.vector_files:     { kind: source, producer: om.inventory, metric: vector_files, type: Count }
  inventory.table1_workloads: { kind: source, producer: om.inventory, metric: table1_workloads, type: Count }
  inventory.rust_tests:       { kind: source, producer: om.inventory, metric: rust_tests, type: Count }
  inventory.python_tests:     { kind: source, producer: om.inventory, metric: python_tests, type: Count }
  inventory.isa_tests:        { kind: source, producer: om.inventory, metric: isa_tests, type: Count }
  inventory.oracle_cases:     { kind: source, producer: om.inventory, metric: oracle_cases, type: Count }

assertions:
  check.rs_faster_than_initial:
    expr: "guest_bench[radix-sort-u64, test].speedup_vs_initial > 1.0"
  check.proof_ahead_of_paper:
    expr: "guest_bench[radix-sort-u64, provable-120].proof_size <= paper.t22.proof_size_max"
  check.soundness_120:
    expr: "min(soundness_ledger[*].per_round_bits) >= paper.t22.soundness_bits"

blocks:
  readme.test_suite_comment:
    file: README.md
    lines:
      - "# Run Rust, Python, and RV64I conformance + fuzz suites ({inventory.rust_tests} Rust, {inventory.python_tests} Python, {inventory.isa_tests} ISA tests)"

tables:
  readme.benchmarks:
    file: README.md
    mode: records
    family: guest_bench
    key_columns: { Guest: workload, Profile: profile }
    columns:
      "Cycles (`T`)": "{cycles:pow2}"
      "Prove": "{prove_wall:.2f s}"
      "Peak RSS": "{peak_rss:auto_iec}"
      "Verify": "{verify_wall:.0f ms}"
      "Proof Size": "{proof_size:, B}"
    row_overrides:
      "[radix-sort-u64, test]":
        "Verify": "{verify_wall:.0f ms} ({verify_rss:.0f MiB})"
      "[radix-sort-u64, provable-120]":
        "Verify": "{verify_wall:.0f ms} ({verify_rss:.0f MiB})"
        "Proof Size": "{proof_size:, B} ({proof_size:.1f KiB})"

  bench.radix_sort_deep_dive:
    file: docs/BENCHMARKS.md
    mode: transposed
    key_column: Quantity
    column_scopes:
      "`provable-120` (`t = 177, gs = 2`)": { family: guest_bench, epoch: current, scope: [radix-sort-u64, provable-120] }
      "`test` (`t = 8`)":                   { family: guest_bench, epoch: current, scope: [radix-sort-u64, test] }
      "Initial `test` baseline":            { family: guest_bench, epoch: initial, scope: [radix-sort-u64, test] }
    rows:
      "T (cycles, padded)": "{cycles:,}"
      "Prove (wall)":       "**{prove_wall:.2f s}** (≈ {throughput:.0f k cycles/s}; {prove_cpu:.1f s} CPU)"
      "Peak memory":        "{peak_rss:.1f GiB} footprint ({heap_peak:.1f GiB} heap peak)"
      "Verify":             "{verify_wall:.0f ms} wall, {verify_cpu:.0f ms} CPU, {verify_rss:.1f MiB} RSS"
      "Proof size":         "{proof_size:, B}"
    # Real tables are irregular: the historical column uses coarser precision and no bold,
    # and only provable-120 shows the KiB figure. Overrides keep adoption zero-diff.
    # The unmapped "Previous (…)" column is treated as prose and preserved byte-for-byte.
    column_overrides:
      "`provable-120` (`t = 177, gs = 2`)":
        "Proof size":   "{proof_size:, B} ({proof_size:.1f KiB})"
      "Initial `test` baseline":
        "Prove (wall)": "{prove_wall:.0f s} (≈ {throughput:.0f k cycles/s}; {prove_cpu:.0f s} CPU)"
        "Peak memory":  "{peak_rss:.1f GiB} ({heap_peak:.1f GiB} heap peak)"
        "Verify":       "{verify_wall:.2f s} wall, {verify_rss:.0f MiB} RSS"
```

---

## 8. Provenance Lockfile & Conflict / Context Detection (`evidence.lock.json`)

### 8.1 Lockfile Schema
The lockfile lives at `.claimwire/evidence.lock.json` and is Git-tracked. Values are stored unrounded in canonical units (§4.1). Keys use canonical reference serialization (§4.3).
```json
{
  "schema": "claimwire-lock/v3",
  "producers": {
    "om.benchmarks": {
      "epochs": {
        "current": {
          "run_id": "bench-20261016-m4pro",
          "git_commit": "9872a7e",
          "git_dirty": false,
          "input_set_digest": "blake3:7f83b165..."
        },
        "prev": { "run_id": "bench-20261015-m4pro", "git_commit": "412c90a", "input_set_digest": "blake3:2c91a402..." },
        "initial": { "run_id": "bench-20261014-m4pro", "git_commit": "109a8b2", "input_set_digest": "blake3:88e1f019..." }
      }
    }
  },
  "evaluated_claims": {
    "guest_bench[radix-sort-u64, test].prove_wall": { "value": 8548700000, "unit": "ns", "epoch": "current" },
    "guest_bench[radix-sort-u64, test].throughput": { "value": 490636.4710423807, "unit": "cycles/s" },
    "guest_bench[radix-sort-u64, test].speedup_vs_initial": { "value": 12.516522980102238, "unit": "ratio" }
  },
  "locations": {
    "README.md::block:readme.test_suite_comment#0": {
      "rendered": "# Run Rust, Python, and RV64I conformance + fuzz suites (362 Rust, 227 Python, 46 ISA tests)",
      "rendered_digest": "blake3:5f2d9c...",
      "context_digest": "blake3:11a08e..."
    },
    "README.md::table:readme.benchmarks::[radix-sort-u64, test]::Prove": {
      "rendered": "**8.55 s**",
      "rendered_digest": "blake3:31a9e0...",
      "context_digest": "blake3:90f4b2..."
    },
    "README.md::span:guest_bench[radix-sort-u64, test].throughput#0": {
      "rendered": "~491k cycles/s",
      "rendered_digest": "blake3:a44c12...",
      "context_digest": "blake3:71e80d..."
    }
  }
}
```

### 8.2 Manual Edit Conflict Detection (`CW4001`)
Before `cw sync` overwrites any inline span, table cell, or block pattern line, it hashes the current bytes in the Markdown file and compares against `locations[loc].rendered_digest` in `evidence.lock.json`:
- If `current_markdown_digest != lockfile.rendered_digest`, a human has hand-edited the owned value in Markdown outside Claim Wire.
- `cw` aborts with `CW4001 (Manual Edit Conflict)` and prints the diff, requiring `--force` to restore from evidence.
- **Precedence:** `CW4001` means *the file disagrees with the lock* (a human edited it). `CW3003` means *the file matches the lock but the claim has moved* (evidence changed and `cw sync` has not run). When both apply, only `CW4001` is reported, because the human edit must be resolved first.

### 8.3 Context Window Anchoring (`CW4002`)
For every inline span and table row, `context_digest` hashes the normalized text of the enclosing sentence/line (excluding owned spans). For a table cell, the context is the cell's column header plus every cell of its row that no lens location owns, so renaming a row, editing a `Notes` cell, or changing a header unit (`Time (ms)` → `Time (s)`) drifts it. Table drift is reported once per table, listing the affected rows and columns.
- If a developer edits `At T = 2^22, throughput is ... (test)` to `At T = 2^24, throughput is ... (test)`, `context_digest` changes while the span binding still points to `T = 2^22`.
- `cw check` emits `CW4002 (Surrounding Context Drift)` so the author either updates the binding or acknowledges the prose edit (`cw sync --ack-context`).

### 8.4 Coverage Auditing (`CW2003`, `CW4003`)
A binding that silently disappears is as dangerous as a wrong number: the verified set shrinks while `cw check` stays green. `cw check` therefore fails when:
- a lens declared in `claims.yaml` has no directive in its declared `file` (or its directive sits in another file) — `CW2003`, located at the lens's YAML key;
- a declared checklist/transposed row matches no Markdown row, or a declared column is not a header of the table — `CW2003`;
- a location recorded in `evidence.lock.json` (a deleted inline span, a removed row) no longer exists — `CW4003`. `cw sync` prunes such entries and reports each one, so the removal shows up in review as a lockfile change.

Markdown discovery covers every Markdown file in the work tree (`git ls-files`, honouring `.gitignore`), not only `README.md` and `docs/`.

---

## 9. Compiler Pipeline, CLI & Diagnostics (`cw-cli`)

### 9.1 Seven-Stage Deterministic Pipeline
1. **01 Lex (`cw-syntax`):** Scan Markdown files into tokens (`Text`, `CodeFence`, `InlineCode`, `InlineSpan`, `TableLens`, `BlockLens`, `PipeTable`), respecting fenced code boundaries and the GFM-conformant row lexer (§5.2.1).
2. **02 Parse (`cw-syntax`):** Build lossless byte-span CSTs for Markdown files and deserialize `.claimwire/*.yaml` with line/column source locations.
3. **03 Expand & Resolve (`cw-ir`):** Expand parameterized claim families across evidence records, canonicalize table keys (§5.2.2), resolve cross-epoch references and family reductions (`sum`, `prefix_sum`, `min`, `max`), and topologically sort the DAG (rejecting cycles with `CW2002`).
4. **04 Type & Scope Check (`cw-ir` + `cw-evidence`):** Enforce dimensional algebra (`CW2001`), scope compatibility (`CW2004`), and semantic input fingerprint freshness (`CW3001`).
5. **05 Evaluate (`cw-engine`):** Evaluate `source`, `derive`, and `assert` nodes in topological order; check deadband hysteresis against `evidence.lock.json`.
6. **06 Lower & Audit (`cw-engine`):** Format values through bidirectional format patterns (preserving sentinels and outer decorators); verify `rendered_digest` (`CW4001`) and `context_digest` (`CW4002`).
7. **07 Transactional Patch (`cw-engine`):** Write updated Markdown files and `evidence.lock.json` via `.claimwire/.staging/journal.json`.

### 9.2 CLI Subcommands

| Command | Purpose |
|---|---|
| `cw check [--format human\|json\|github\|sarif]` | Read-only CI/local check. Validates syntax, types, scopes, input freshness, assertions, and byte-exact agreement between Markdown and `evidence.lock.json`. |
| `cw sync [--exact] [--force] [--ack-context]` | Recomputes claims and updates owned Markdown spans, table lens cells, block lenses, and `evidence.lock.json`. |
| `cw capture --producer <id> -- <cmd...>` | Runs `<cmd>`, captures `@cw:record` telemetry lines, fingerprints inputs, and writes a new run manifest. |
| `cw impact [<path...> \| --git-diff <rev>]` | Traces modified source files forward through the invalidation graph to list affected producers, claims, and exact `file:line` Markdown locations. |
| `cw explain <location-or-claim>` | Prints the full provenance tree for any claim, span, or `file:line` coordinates. |
| `cw diff <rev1>..<rev2>` | Compares claim states across Git revisions, classifying changes into **Value Changed**, **Definition Changed**, **Scope Changed**, **Evidence Refreshed**, or **Assertion Regressed**. |
| `cw adopt [<file...>]` | Reverse-parses existing Markdown spans, table cells, and block lenses using bidirectional format strings to initialize `evidence.lock.json` without modifying Markdown bytes. |
| `cw merge-driver %O %A %B` | Custom Git merge driver that resolves `evidence.lock.json` and wired Markdown conflicts structurally. |

**Exit codes:** `0` means everything verified. `1` means at least one diagnostic of severity `error` (including `CW3001`, `CW3002`, `CW3003`, `CW4001`, `CW4002`). `2` means a usage, I/O, or internal error. Warnings never change the exit code unless `--deny-warnings` is passed.

### 9.3 Diagnostic Code Taxonomy

| Range | Category | Key Codes |
|---|---|---|
| `CW1xxx` | Syntax & CST | `CW1001` Malformed marker · `CW1002` Nested span · `CW1003` Mismatched close tag · `CW1004` Unclosed span · `CW1005` Malformed GFM table under `cw:table` · `CW1006` Unmatched `cw:block` line pattern · `CW1007` Ambiguous (non-invertible) format pattern · `CW1008` Unescaped pipe splits a code span in a GFM table |
| `CW2xxx` | Types & Scope | `CW2001` Dimensional unit mismatch · `CW2002` Dependency cycle · `CW2003` Unresolved claim/table key · `CW2004` Incompatible scope in derivation · `CW2005` Missing or invalid unit in format spec · `CW2104` Missing evidence record for non-sentinel cell |
| `CW3xxx` | Verification & Freshness | `CW3001` Stale evidence (semantic input digest changed) · `CW3002` Assertion failed (`assert` is `false`) · `CW3003` Markdown value out of sync with claim · `CW3004` Value not representable in format (e.g. `pow2` of a non-power of two) |
| `CW4xxx` | Conflict & Context | `CW4001` Unapproved manual edit inside owned span/cell/block · `CW4002` Surrounding prose context drifted from locked `context_digest` · `CW4003` Locked location no longer present in Markdown |

---

## 10. First Test Environment & 10-Point Acceptance Suite (`tests/fixtures/magnetar`)

The first test environment copies `../magnetar`'s Markdown files (`README.md`, `docs/BENCHMARKS.md`, `docs/VALIDATION.md`, `docs/spec/13-security.md`, `docs/spec/15-testing.md`) into `tests/fixtures/magnetar/` and wires five complete claim families:
1. **`guest_bench[workload, profile]`** across all 15 RV64I workloads and 3 epochs (`@initial`, `@prev`, `@current`), feeding `README.md` (`readme.benchmarks` table + throughput prose) and `docs/BENCHMARKS.md` (5 tables including the transposed deep-dive and paper comparison tables).
2. **`phase_bench[phase]`** across all 12 `OM_TRACE` prover phases, computing all 36 `Ends at` cells via `prefix_sum(duration)` and deriving the 6 grouped prover phase shares (`27%`, `19%`, `19%`, `18%`, `14%`, `3%`) and speedups (`1.55×`, `6.06×`, `8.19×`).
3. **`validation_vec[case]`** across the RFC 6234 SHA-256 public vectors (`empty`, `abc`, `message56`) in `docs/VALIDATION.md`.
4. **`soundness_ledger[row]`** across the 40-row RBR soundness ledger in `docs/spec/13-security.md` (with `exact` sentinel preservation on rows 5 and 19, and `min(per_round_bits) >= 120.0`).
5. **`inventory.*`** across Rust tests (`362`), Python tests (`227`), ISA conformance tests (`46`), oracle cases (`47`), test vector files (`27`), Rust crates (`16`), and Table 1 workloads (`15`).

### 10.1 Real-World Catches in `magnetar`
Both findings were re-verified against `magnetar` commit `f8382f3`.

**Drift (`CW3003`).** Micro-testing `magnetar` revealed an actual live documentation drift:
- `README.md:18` (via `cw:block`) and `docs/BENCHMARKS.md:185` (via `cw:table`) state **`227`** Python reference tests.
- `docs/spec/15-testing.md:19` states `(192 tests, about one minute)`.
Wiring `docs/spec/15-testing.md:19` to `inventory.python_tests` immediately demonstrates `cw check` catching the `192 != 227` (`CW3003`) discrepancy and `cw sync` repairing it to `227`.

**Rendering bug (`CW1008`).** `docs/spec/04-pcs.md:245` writes `` `2k/|K|` `` with unescaped pipes in a three-column table. GitHub splits that row into extra cells and drops everything past the third, so the rendered page silently loses part of the soundness table. A GFM-conformant lexer (§5.2.1) surfaces this before a lens is ever bound to the table.

### 10.2 10-Point Automated Verification Matrix
1. **Zero-Diff Adoption & Live Drift Detection (`cw adopt` + `cw check`):** Bidirectional format parsing verifies all synced `magnetar` tables/blocks/spans with 0 byte diffs on existing prose, while immediately flagging the real `192 vs 227` drift in `docs/spec/15-testing.md:19`.
2. **Multi-Table, Transposed & Multi-Doc Fan-Out (`cw sync`):** Updating `guest_bench[radix-sort-u64, test].prove_wall` from `8.55 s` to `7.90 s` updates `README.md` (`records` table + prose) and `docs/BENCHMARKS.md` (`transposed` deep-dive table + paper comparison table + prose), and recomputes throughput (`~491k` $\rightarrow$ `~531k cycles/s`) and speedup (`12.5×` $\rightarrow$ `13.5×`) everywhere.
3. **Ordered Prefix-Sum & Subset-Sum Cascade:** Updating `phase_bench[virtual_spartan].duration` from `1.54 s` to `1.20 s` automatically updates all 7 downstream `Ends at` cells in `BENCHMARKS.md:159–165`, the total wall time (`8.76 s` $\rightarrow$ `8.42 s`), and all 6 phase shares and speedups in `BENCHMARKS.md:222–229`.
4. **Idempotence:** A second `cw sync` is a strict no-op (`0` files written).
5. **Semantic Fingerprint Immunity:** Adding a `// comment` or formatting whitespace in `crates/om-spartan/src/lib.rs` leaves the semantic input digest unchanged (`cw check` passes in `< 10 ms` with `0` stale claims).
6. **True Code Invalidation (`cw impact`):** Modifying code in `crates/om-spartan/src/lib.rs` triggers `CW3001 (Stale Evidence)` on `om.benchmarks` and lists every affected table, block, and span across `README.md` and `docs/BENCHMARKS.md`.
7. **Deadband Hysteresis & Sentinel Preservation:** Recording `8.60 s` ($+0.6\%$ vs. the locked `8.5487 s`, within `deadband: 2.5%`) passes `cw check` without churning `README.md`, `—` and `exact` sentinel cells are preserved untouched, and changing deterministic `proof_size` by 1 byte (`603,942 B` $\rightarrow$ `603,943 B`, `deadband: 0%`) triggers an immediate update.
8. **Assertion Guard Refusal (`CW3002`):** Setting `provable-120` proof size to `620,000 B` ($> 599\text{ KiB}$) blocks `cw sync` with `CW3002` and refuses to publish the broken `"Ahead of paper"` comparison.
9. **Manual Edit Conflict (`CW4001`) & Context Drift (`CW4002`):** Hand-editing `8.55 s` to `5.00 s` in `README.md` triggers `CW4001`; editing `At T = 2^22` to `At T = 2^24` around the throughput span in `README.md:53` triggers `CW4002`.
10. **Full Provenance Trace (`cw explain`):** `cw explain README.md:53` prints the complete derivation and multi-epoch provenance chain (`4,194,304 cycles / 8.5487 s`, run `bench-20261016-m4pro`, commit `9872a7e`).

---

## 11. Workspace & Crate Layout

Planned layout. As of spec `v0.4`, only `spec.md` exists.

```text
claim-wire/
├── Cargo.toml
├── spec.md
├── crates/
│   ├── cw-syntax/       # Lossless Markdown CST, GFM-conformant table lexer, block lens & bidirectional format parser
│   ├── cw-ir/           # Dimensional type checker, family/reduction expander (sum, prefix_sum), scope & epoch DAG resolver
│   ├── cw-evidence/     # Two-tier semantic fingerprinter, epoch manifest loader, @cw: wire protocol capture
│   ├── cw-engine/       # Topological evaluator, deadband/sentinel handler, context/edit conflict auditor, atomic patcher
│   └── cw-cli/          # `cw` binary (`check`, `sync`, `capture`, `impact`, `explain`, `diff`, `adopt`, `merge-driver`)
├── skills/
│   └── claim-wire/
│       └── SKILL.md     # Agent skill for discovery, family deduplication, table/block lens wiring, and CWxxxx repair
└── tests/
    └── fixtures/
        └── magnetar/    # Full end-to-end test environment built from ../magnetar's Markdown files
```

---

## 12. Open Questions (Pre-Implementation)

These are unresolved design decisions that should be settled before or during the first implementation milestone. Feedback is welcome.

1. **Stale evidence in CI.** Full benchmark suites are too expensive to rerun on every pull request, but `CW3001` is an error by default. Is `on_stale: warn` with a required re-capture before release the right default? Should staleness be scoped per family rather than per producer?
2. **Cross-machine evidence.** Timings captured on an Apple M4 Pro are not comparable to a CI runner. Should hardware identity be part of an epoch's identity, so that `@current` on different hardware is a different epoch rather than a deadband comparison?
3. **Frame spoofing.** Any subprocess output that happens to contain `@cw:record` is ingested. Is family/metric validation (§6.4) enough, or should `cw capture` inject a per-run nonce that frames must echo?
4. **Context-digest granularity.** Hashing the whole enclosing line makes typo fixes trigger `CW4002`. Sentence-level segmentation is better for prose but is ill-defined across languages. Which normalization is good enough?
5. **Merge-driver semantics.** When two branches refresh the same epoch with different runs, should `cw merge-driver` pick by timestamp, refuse, or require re-capture?
6. **Line endings.** Non-Interference requires byte preservation. When `cw sync` patches a file with mixed `\r\n` / `\n` endings, inserted bytes should copy the line ending of the line being patched. Is that sufficient?
