# Agent brief contract

An agent brief is the authoritative, durable specification posted when an
issue becomes `ready-for-agent`. The issue discussion remains context; the
brief defines completion.

## Principles

- Describe observable behavior and public contracts, not an edit sequence.
- Name stable interfaces and configuration shapes when useful.
- Avoid file paths, line numbers, transient branch state, and assumptions about
  the current implementation layout.
- Give independently verifiable acceptance criteria.
- State explicit scope boundaries.
- Include manual or external gates and identify their human owner.

## Template

```markdown
## Agent brief

**Category:** bug or enhancement
**Summary:** One-line behavioral outcome.

**Current behavior:**
What happens now and the evidence for it.

**Desired behavior:**
What must happen after the work, including relevant edge cases.

**Key interfaces:**
- Stable interface or contract and the required behavior

**Acceptance criteria:**
- [ ] Specific independently verifiable condition

**Out of scope:**
- Adjacent behavior that must not change
```

Before posting, confirm that a fresh agent could determine completion without
private conversation context or access to the author's local workstation.
