---
date_created: 2026-03-29
date_modified: 2026-03-29
status: active
audience: both
cross_references:
  - AGENTS.md
  - docs/001-architecture.md
---

# Documentation Standards — Chronicle

## Overview

This document defines how documentation is structured, created, and maintained in this project. These standards apply to both human contributors and AI agents.

## Directory Structure

```
chronicle/
├── README.md              # Project overview (audience: human)
├── AGENTS.md              # Agent conventions (audience: agent)
├── CLAUDE.md              # Claude-specific rules (audience: agent)
├── CHANGELOG.md           # Version history (audience: both)
└── docs/
    ├── NNN-*.md           # Root: foundational, cross-cutting docs
    ├── specs/             # Feature specifications and design docs
    │   └── NNN-*.md
    ├── adrs/              # Architecture Decision Records
    │   └── NNN-*.md
    ├── references/        # API docs, glossary, external references
    │   └── NNN-*.md
    ├── tasks/             # Work items, backlogs, sprint plans
    │   └── NNN-*.md
    └── research/          # Spikes, investigations, POC write-ups
        └── NNN-*.md
```

### Directory Purposes

| Directory | Purpose | Typical Content |
|-----------|---------|-----------------|
| `docs/` (root) | Foundational docs that span all categories | Architecture, dev guide, doc standards |
| `docs/specs/` | Feature design before or during implementation | Feature specs, API designs, data models |
| `docs/adrs/` | Significant technical decisions with rationale | ADRs with context, decision, consequences, alternatives |
| `docs/references/` | Stable reference material for lookup | CLI reference, config reference, glossary |
| `docs/tasks/` | Trackable work units for humans and agents | Task breakdowns, implementation plans |
| `docs/research/` | Exploratory work and investigations | Spike results, benchmarks, tool evaluations |

### Choosing the Right Directory

```
Is it a cross-cutting concern (architecture, dev workflow, standards)?
  → docs/ root

Is it about WHAT to build (feature design, API shape)?
  → docs/specs/

Is it about WHY we chose something (technology, pattern, approach)?
  → docs/adrs/

Is it about HOW something works (stable interface, lookup material)?
  → docs/references/

Is it about WHAT TO DO next (work breakdown, implementation steps)?
  → docs/tasks/

Is it about WHAT WE LEARNED (investigation, evaluation, benchmark)?
  → docs/research/
```

## Front-Matter Schema

**Every markdown file in `docs/` and its subdirectories must include this YAML front-matter:**

```yaml
---
date_created: 2026-03-29        # Set once, never change
date_modified: 2026-03-29       # Update on substantive edits
status: draft                    # draft | active | review | deprecated
audience: both                   # human | agent | both
cross_references:                # Related documents
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
---
```

### Field Definitions

| Field | Type | Rules |
|-------|------|-------|
| `date_created` | `YYYY-MM-DD` | Set at creation, immutable |
| `date_modified` | `YYYY-MM-DD` | Bump on substantive edits only |
| `status` | enum | `draft` → `active` → `deprecated`; use `review` for pending approval |
| `audience` | enum | `human` (conversational), `agent` (terse/structured), `both` (balanced) |
| `cross_references` | list | Relative paths from repo root to related docs |

### When to Bump `date_modified`

**DO bump:**
- Content changes (new sections, updated information)
- Structural changes (reordering, splitting sections)
- Adding/removing cross-references

**DO NOT bump:**
- Typo corrections
- Whitespace/formatting fixes
- Link URL updates (same destination)

### Status Lifecycle

```
draft ──▶ active ──▶ deprecated
  │         │
  └──▶ review ──┘
```

- `draft`: Initial creation, may be incomplete
- `review`: Ready for review/approval
- `active`: Approved and current
- `deprecated`: Superseded or no longer applicable (keep for history; add note pointing to replacement)

## Numbered Filename Convention

### Format

```
NNN-kebab-case-title.md
```

- 3-digit zero-padded prefix
- Kebab-case descriptive title
- `.md` extension

### Numbering Rules

- Numbers are **scoped per directory** — `specs/001-*.md` and `adrs/001-*.md` are independent
- Sequential within each directory (001, 002, 003...)
- **Leave gaps of 5–10** between files to allow future insertions (e.g., 001, 005, 010, 015)
- If inserting between `005` and `010`, use `007` or `008`
- **Never renumber existing files** — this breaks cross-references
- If you need to insert but no gap exists, use the next number after the last file

### Examples

```
docs/
├── 001-architecture.md
├── 002-development-guide.md
├── 003-documentation-standards.md
├── specs/
│   └── 001-initial-delivery.md
├── adrs/
│   ├── 001-use-rust.md
│   └── 002-cron-over-launchd.md
├── references/
│   ├── 001-cli-reference.md
│   └── 005-config-reference.md
├── tasks/
│   ├── 001-project-setup.md
│   └── 005-canon-module.md
└── research/
    └── 001-git2-vs-cli.md
```

## Writing Style Guide

### For `audience: human`
- Conversational but professional tone
- Explain "why" not just "what"
- Include examples and context
- OK to use first/second person

### For `audience: agent`
- Terse, structured, imperative
- Use tables and lists over prose
- Front-load critical information
- Include exact file paths and commands
- Avoid ambiguity — be explicit about constraints

### For `audience: both`
- Balanced tone — clear and professional
- Structure with headers, tables, and lists
- Include both context (for humans) and precise instructions (for agents)

### General Rules (All Audiences)
- One topic per document
- Keep docs under 500 lines (split if larger, add cross-references)
- Use relative links between docs (from repo root)
- Code examples must be runnable
- Tables for structured data; prose for narrative

## Document Type Templates

### Feature Spec (`docs/specs/`)

```markdown
---
date_created: YYYY-MM-DD
date_modified: YYYY-MM-DD
status: draft
audience: both
cross_references:
  - docs/001-architecture.md
---

# Spec: Feature Title

## Summary
One-paragraph description of the feature.

## Motivation
Why is this needed? What problem does it solve?

## Design

### User-Facing Behavior
What the user sees/does.

### Technical Approach
How it works under the hood.

### Data Model Changes
Any new or modified data structures.

## Acceptance Criteria
- [ ] Criterion 1
- [ ] Criterion 2

## Open Questions
- Question 1?

## Out of Scope
- Explicitly excluded item
```

### Architecture Decision Record (`docs/adrs/`)

```markdown
---
date_created: YYYY-MM-DD
date_modified: YYYY-MM-DD
status: proposed | accepted | deprecated | superseded
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/research/NNN-related-spike.md
---

# ADR-NNN: Decision Title

## Status
Proposed | Accepted | Deprecated by [ADR-NNN](NNN-*.md) | Superseded by [ADR-NNN](NNN-*.md)

## Context
What is the issue or question? Why does a decision need to be made?

## Decision
What was decided?

## Consequences

### Positive
- Benefit 1

### Negative
- Tradeoff 1

## Alternatives Considered

### Alternative A
- Pros: ...
- Cons: ...
- Why rejected: ...
```

### ADR Rules

- Never delete an ADR — deprecate or supersede it
- Number sequentially — never reuse numbers
- Link to superseding ADR when deprecating
- Keep ADRs concise — one decision per record

### Task (`docs/tasks/`)

```markdown
---
date_created: YYYY-MM-DD
date_modified: YYYY-MM-DD
status: draft | active | review | deprecated
audience: agent
cross_references:
  - docs/specs/NNN-related-spec.md
---

# Task: Title

## Objective
What this task accomplishes.

## Acceptance Criteria
- [ ] Criterion 1
- [ ] Criterion 2

## Implementation Steps
1. Step 1
2. Step 2
3. Step 3

## Files to Touch
- `path/to/file1`
- `path/to/file2`

## Dependencies
- Depends on: [Task NNN](NNN-other-task.md)
- Blocks: [Task NNN](NNN-other-task.md)

## Notes
Any additional context for the implementer.
```

### Research/Spike (`docs/research/`)

```markdown
---
date_created: YYYY-MM-DD
date_modified: YYYY-MM-DD
status: draft | active | deprecated
audience: both
cross_references:
  - docs/adrs/NNN-related-decision.md
---

# Research: Topic Title

## Goal
What question are we trying to answer?

## Approach
How we investigated.

## Findings

### Option A
- Description
- Pros: ...
- Cons: ...

### Option B
- Description
- Pros: ...
- Cons: ...

## Recommendation
What we recommend and why.

## Links
- [External resource 1](url)
- [External resource 2](url)
```

## Cross-Referencing Between Directories

Use repo-root-relative paths for cross-references:

```yaml
cross_references:
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
  - docs/adrs/001-use-rust.md
  - docs/research/001-git2-vs-cli.md
  - docs/tasks/001-project-setup.md
```

Common cross-reference patterns:

| From | To | When |
|------|----|------|
| `specs/` | `adrs/` | Spec references a decision that shaped it |
| `adrs/` | `research/` | ADR was informed by a spike |
| `tasks/` | `specs/` | Task implements part of a spec |
| `references/` | `specs/` | Reference docs a specified feature |
| `research/` | `adrs/` | Research led to a decision |

## Document Creation Checklist

When creating a new doc:

- [ ] Correct subdirectory for the document's purpose
- [ ] Front-matter with all required fields
- [ ] `date_created` set to today
- [ ] `status` set appropriately (usually `draft`)
- [ ] `audience` set correctly
- [ ] `cross_references` populated with related docs (using repo-root-relative paths)
- [ ] Next available number in the directory (respecting gaps)
- [ ] Under 500 lines
