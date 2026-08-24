---
target: the dashboard
total_score: 30
max_score: 32
na_heuristics: 7,10
p0_count: 0
p1_count: 2
timestamp: 2026-08-22T01-10-24Z
slug: dashboard-src-app-tsx
---
# Critique: arbkit results dashboard (dashboard/src/App.tsx)

Method: dual-agent (A: design review · B: deterministic detector)

## Design Health Score: 30/32 (Excellent, 94%)

Heuristics 7 (Flexibility/Efficiency) and 10 (Help/Documentation) scored n/a (read-only persuade surface; inline self-evidencing methodology).

| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 4 | Excellent loading/error states, role=status, live counter |
| 2 | Match System / Real World | 3 | Raw enums leak ("brokenLeg"); jargon assumes engineers |
| 3 | User Control and Freedom | 3 | Silent restore-all on last filter deselect |
| 4 | Consistency and Standards | 3 | Pill-radius chips break Square Instrument Rule; Recharts legend drift |
| 5 | Error Prevention | 3 | Bounds handled; silent filter reset unexplained |
| 6 | Recognition Rather Than Recall | 3 | Legend vs select label mapping held in memory |
| 8 | Aesthetic and Minimalist Design | 4 | Every element earns its rule |
| 9 | Error Recovery | 4 | Distinct empty/error states, retry, copper error rule |

## Design Specificity Verdict

Authored for arbkit, not category-interchangeable. Ledger world survives implementation: binder rail, continuous hairline records, Newsreader/Plex Mono pairing, log budget ruler. Slips: Recharts default legend markers/hover cursors; `.trade-chip`/`.trade-badge` border-radius:999px pills violating the Square Instrument Rule (styles.css:1280, 1330).

Deterministic scan: clean (0 findings across App.tsx, main.tsx, BudgetRuler.tsx, Charts.tsx, TradeLedger.tsx). Browser overlay skipped: no browser automation exposed this session.

## Strengths

1. BudgetRuler.tsx — truly logarithmic ns→ms scale, authored ticks, semantic color assignment, role=img with title/desc.
2. TradeLedger state honesty — three distinct non-fabricating empty/error states, live-region counter.
3. Continuous record layout — collapses 4→2→1 across breakpoints without becoming cards.

## Priority Issues

1. [P1] Raw classification enums as UI copy (TradeLedger.tsx:157–167, 260–264): brokenLeg/proportional/phantom/clean verbatim. Fix: render-time label map, enum as data-*. Command: clarify
2. [P1] Missing table equivalents for ThroughputChart and ExpectedVsRealizedChart — violates PRODUCT.md accessibility commitment. Fix: replicate data-table details pattern. Command: harden
3. [P2] Pill-shaped chips/badges violate Square Instrument Rule (styles.css:1280, 1330). Fix: radius 0. Command: polish
4. [P2] Trades-section decision overload: 8+ options in one band; five summary cards break chunking. Fix: four cards, fold clean share, demote toggle to chip. Command: distill
5. [P2] Sub-11px type floor: scope stamp 0.62rem (~10px) mobile; labels 0.66–0.68rem uppercase mono. Contrast passes AA. Fix: floor labels 0.72rem, scope stamp ≥0.7rem mobile. Command: typeset

## Persona Red Flags

Sam (a11y): no table equivalents for throughput/scatter; aria-label on plain divs (hero-facts, financial-ledger, trade-summary — need role=group); sort button labeled only "Δ"; touch targets <44px (history date buttons 5×7px padding, chips ~27px).
Jordan (first-timer): "phantom" unglossed in evidence rail; comparator host choice unexplained.
Casey (mobile): min-height:620px hero pushes log ruler below fold; tables min-width:900px in bare overflow wrappers, no scroll affordance; scope stamp 0.62rem.

## Minor Observations

- .method-note margin-top uses !important
- Hover-only title tooltips on execution track segments; data duplicated below — drop
- downloadSnapshot revokes object URL synchronously after click() — defer revoke (Firefox)
- Recharts legend swatches library-default; use thin square markers
- Keep the faint 12.5%-interval vertical body rules and the loading copy
