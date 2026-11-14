# ADR-0003: Aggregation algorithm and numbers

- Status: proposed (needs maintainer review, AGENTS.md D6)
- Date: 2026-11-14
- Code: `crates/eatsights-worker/src/aggregate/`, config `crates/eatsights-worker/aggregation.toml`
- Version: `agg-2026.11-1` (stamped on every fact as `algorithm_version`)

## Context

Facts (`schema.json#/$defs/fact`) are computed from public observations by a pure, deterministic
function `aggregate(prop, subject, obs, now, cfg)`. AGENTS.md 9.1 fixes the rules; this ADR picks the
numbers. They answer SCHEMA.md open question 1 (weight per trust level) for aggregation only.
Open question 2 (who may grant `trusted`) does not affect this algorithm: it only consumes the class
already assigned to each observation. Chain depth, attestation expiry and vouching penalties are trust
policy (M4) and not set here.

All numbers live in `aggregation.toml` (embedded default; override path via
`EATSIGHTS_AGGREGATION_CONFIG`). Any change must bump `algorithm_version`.

## Core ideas

1. **One vote per pseudonym.** Each pseudonym's *latest* backing observation counts (a `value`, a
   `confirm` pointing at a value, or a `dispute` carrying a counter-value). Repeating a submission, or
   changing your mind, never stacks support. A pseudonym is per place/dish, so this is "one person, one
   vote" at that place.
2. **Weight = class weight × recency decay**, decay = `0.5^(age_hours / half_life_hours)` using the
   property's registry half-life. Future timestamps count as age 0.
3. **Confirmation needs independence.** `confirmed` requires at least `min_distinct_to_confirm`
   (2, and the code refuses anything lower) distinct pseudonyms backing the winning value. Weight never
   substitutes for count, so trust raises weight but never confirms alone.
4. **Deterministic.** Inputs are sorted by `(observed_at, id)`; values compared by canonical JSON
   (sorted keys); ties broken by the lowest canonical string. Confidence is rounded to 6 decimals.

## Class weights

| Class | Weight | Why |
|---|---|---|
| unverified | 0.5 | Open registration (D5): cheap keys must count, but less, so sybils need twice as many. |
| member | 1.0 | Baseline: vouched by someone in person. |
| trusted | 1.5 | Track record; modest boost so a trusted voice cannot drown two members. |
| verifier | 2.0 | Highest human trust, still equal to just two members. |
| vendor | 1.5 | Knows the menu best, but has an interest; equal to trusted. |
| import | 0.5 | Bulk data, unknown freshness; same as unverified. |

The spread is deliberately narrow (4×). With wide spreads a single trusted key would de facto decide
values, which the governance rules aim to avoid.

## Thresholds

| Name | Value | Meaning and reason |
|---|---|---|
| `min_distinct_to_confirm` | 2 | Smallest number that rules out a single key. Raising it slows confirmation in sparse areas. |
| `confirm_confidence` | 0.6 | Confidence also needed for `confirmed`. Two agreeing members with no dissent score ≈0.67. |
| `confidence_prior` | 1.0 | `confidence = share × S/(S + prior)`, where `share` = winner's net support / (all support + disputes) and `S` = total support. One member's fresh vote alone gives 0.5. |
| `contested_ratio` | 0.5 | `contested` when weighted disputes, or the runner-up value's support, reach half of the winner's support. |
| `dispute_factor` | 1.0 | One unit of dispute weight cancels one unit of support. |
| `stale_half_lives` | 3 | `stale` when the newest supporting observation is older than 3 half-lives (decay 12.5%): 6 h for `availability.observed`, 90 min for `queue.length`, 90 days for prices. |
| `stale_min_support` | 0.1 | Or when the winner's decayed support drops below 0.1 (e.g. one unverified vote after ~2.3 half-lives). |
| `median_tolerance` | 0.1 | For `median`, values within ±10 % of the median (minimum ±0.1) "agree". |
| `basis_cap` | 20 | At most 20 observation ids in `basis` (sorted), to keep facts small. |

Status precedence: `stale` > `contested` > `confirmed` > `unconfirmed`.

## Strategies

- **latest_weighted**: group votes by value; net = Σ decayed votes − disputes against that value
  (from pseudonyms not backing it). Highest net wins.
- **mode**: as above but votes are *not* decayed for choosing the winner (stable properties like
  milk type); staleness still uses decayed support.
- **median**: one vote per pseudonym, newest `window` votes (registry `aggregation.window`, default 10).
  Votes are partitioned by the rest of the value (e.g. `unit`, `stage`, `day_type`) and the heaviest
  partition is used, so ml and cups never mix. Weighted lower median of `field`; `HH:MM` strings are
  treated as minutes and formatted back; integer fields are rounded. The rest of the value comes from
  the newest vote in the winning partition. Disagreement outside the tolerance counts toward `contested`.
- **union**: each set element is included if its net support (pseudonyms listing it, minus disputes of
  observations listing it) is positive. Confidence is the mean per-element confidence. `confirmed`
  additionally requires every included element to be backed by ≥2 pseudonyms. A pseudonym's latest
  set replaces its earlier ones, so removing an element is possible.
- **schedule**: the newest declared rule wins (vendors may declare). Status and confidence are computed
  as for latest_weighted, so an unconfirmed vendor rule stays `unconfirmed` until someone confirms it.
- **derived**: returns no fact in the MVP (`availability.declared_reliability` needs a cross-property
  join; to be designed in M3+).

## Vendors

Vendor votes are weighted like everyone else's. `vendor_value_differs` is `true` when the vendor's
latest value differs from the value with the most *customer* support (for median: outside tolerance or
in another partition; for union: a different set). Vendors therefore never silently override customers,
and a vendor alone is never `confirmed`.

## Counts

- `confirmations`: distinct pseudonyms backing the current value (value or confirm).
- `disputes`: distinct pseudonyms disputing it.
- `distinct_contributors`: distinct pseudonyms among all inputs.
- `trusted_confirmations`: backers whose class is `trusted` or `verifier`.
- `last_confirmed_at`: newest `confirm` backing the value, or the newest backing vote if `confirmed`.

## Open for review

1. Class weights (open question 1). 2. Whether `confirmed` should need 3 pseudonyms for
safety-relevant properties (`dish.allergens`). 3. Whether vendor weight should drop when
`vendor_value_differs` persists. 4. Pseudonym salt rotation would split one person into two
pseudonyms and could let one person reach `confirmed` (open question 3); rotation must be designed
with this in mind. 5. Availability layering (SCHEMA.md) is combined at read time across facts, not in
this per-property function.
