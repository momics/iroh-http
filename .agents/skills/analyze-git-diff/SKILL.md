---
name: analyze-git-diff
description: Analyze any Git diff or revision range and generate a self-contained HTML review report with exact additions/deletions, per-file and per-directory visualization, code/comment/blank classification, production-versus-test accounting including inline Rust tests, and architectural concentration signals. Use when reviewing PR size, explaining where lines came from, separating operational code from tests/docs/generated files, auditing module responsibility, or producing a reusable diff artifact.
---

# Analyze Git Diff

Generate an evidence-based diff composition report. Keep exact Git measurements separate from heuristic classifications and architectural interpretation.

## Workflow

1. Identify the repository, base, and head. Prefer explicit revisions. For a PR, query its current base/head SHAs and record them in the response.
2. Preserve the worktree. Do not checkout, reset, stage, commit, or modify application files.
3. Run the bundled generator:

   ```bash
   python3 .agents/skills/analyze-git-diff/scripts/git_diff_report.py \
     --repo /absolute/repo \
     --base <base-revision> \
     --head <head-revision> \
     --output /absolute/report.html \
     --title "Diff analysis"
   ```

4. Validate its headline totals against:

   ```bash
   git diff --shortstat <base> <head>
   git diff --numstat --find-renames <base> <head>
   ```

5. Manually inspect the largest operational files, largest test files, deleted predecessors, and modules whose placement is in question. Use commit provenance (`git log --numstat <base>..<head> -- <path>`) to distinguish independent fixes from feature work.
6. Add concise architectural findings to a regenerated artifact with repeated `--finding "..."` arguments. Label consolidation estimates as estimates; never subtract them from exact Git totals.
7. Share the HTML artifact and summarize: raw churn, net footprint, operational code, test code, comment-only lines, important moves/consolidations, classification limitations, and actionable responsibility findings.

## Accounting rules

- Treat Git patch additions/deletions as authoritative review churn.
- Report net footprint separately; churn is not the same as newly owned code.
- Classify test paths and Rust `#[cfg(test)]`/`#[test]` regions as tests.
- Keep examples, docs, configuration, tooling, generated files, tests, and operational source mutually exclusive.
- Count comment-only, blank, and code-bearing lines independently from role. A mixed code-plus-inline-comment line is code-bearing.
- Reconstruct old lines from the base blob and new lines from the head blob before classifying them.
- Surface binary files and unusual layouts rather than inventing textual counts.
- Treat comment and role classification as heuristic. The generator handles common C-family, HTML, and hash-comment syntax but can be uncertain around raw strings, macros, regex literals, or unconventional generated files.

## Architectural review

Use the report to locate responsibility, not to judge architecture by line count alone.

- Apply the deletion test: if moving a module merely spreads its behavior across adapters, it may be earning depth; if only one caller uses it and its domain language belongs to that caller, its seam may be misplaced.
- Distinguish foundational correctness from feature policy even when both were found during the same review.
- Check deleted predecessors before calling a new file wholly new code.
- Name the module, interface, implementation, seam, adapter, depth, leverage, and locality consistently when making architectural recommendations.
- Do not turn style preferences or high line counts alone into findings.

## Output contract

The HTML must be standalone UTF-8 with no CDN dependency and include:

- pinned base, head, and merge-base SHAs;
- exact file/add/delete/net totals;
- raw churn and before/after footprint cards;
- operational/test code-bearing counts;
- comment-only and blank counts;
- role, directory, and per-file tables;
- embedded methodology and limitations;
- escaped filenames and user-provided findings;
- machine-readable totals embedded in the artifact.

If there is no diff, still produce a valid empty report and say so. If classification validation fails, stop and report the mismatch instead of publishing partial numbers.

## Maintainer validation

After changing the generator, run its dependency-free regression suite:

```bash
python3 -m unittest discover \
  -s .agents/skills/analyze-git-diff/scripts \
  -p 'test_git_diff_report.py'
```

The fixtures cover renamed paths, binary changes, inline Rust tests, comment
classification, empty reports, and HTML escaping. Add a fixture whenever a new
classification rule or Git edge case is introduced.
