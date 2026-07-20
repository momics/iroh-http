#!/usr/bin/env python3
"""Generate a self-contained HTML line-accounting report for a Git diff.

The report keeps exact Git additions/deletions separate from heuristic
classification. Changed lines are classified by lexical content (code,
comment-only, blank) and by role (operational, test, docs, example, generated,
configuration, tooling, other). Rust #[cfg(test)] blocks are classified as test
even when they live inside a production source file.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import html
import json
import os
import re
import shlex
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable


ROLE_ORDER = [
    "operational",
    "test",
    "example",
    "docs",
    "configuration",
    "tooling",
    "generated",
    "other",
]
ROLE_LABEL = {
    "operational": "Operational source",
    "test": "Tests / harness",
    "example": "Examples / test UI",
    "docs": "Documentation",
    "configuration": "Configuration / metadata",
    "tooling": "Tooling / scripts",
    "generated": "Generated / lockfiles",
    "other": "Other",
}
ROLE_COLOR = {
    "operational": "#315efb",
    "test": "#14a673",
    "example": "#9b51e0",
    "docs": "#e08a1e",
    "configuration": "#768194",
    "tooling": "#09a6c7",
    "generated": "#c65d7b",
    "other": "#a0a7b4",
}

CODE_EXTENSIONS = {
    ".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".kt", ".kts",
    ".swift", ".py", ".sh", ".bash", ".zsh", ".java", ".c", ".h",
    ".cpp", ".hpp", ".css", ".scss", ".html", ".sql",
}


@dataclass
class ChangeLine:
    side: str
    number: int
    text: str


@dataclass
class FileDelta:
    path: str
    old_path: str | None = None
    new_path: str | None = None
    binary: bool = False
    additions: int = 0
    deletions: int = 0
    added: list[ChangeLine] = field(default_factory=list)
    removed: list[ChangeLine] = field(default_factory=list)
    role: str = "other"
    directory: str = "other"
    add_content: collections.Counter = field(default_factory=collections.Counter)
    del_content: collections.Counter = field(default_factory=collections.Counter)
    add_roles: collections.Counter = field(default_factory=collections.Counter)
    del_roles: collections.Counter = field(default_factory=collections.Counter)
    old_content: collections.Counter = field(default_factory=collections.Counter)
    new_content: collections.Counter = field(default_factory=collections.Counter)
    old_roles: collections.Counter = field(default_factory=collections.Counter)
    new_roles: collections.Counter = field(default_factory=collections.Counter)
    add_role_content: collections.Counter = field(default_factory=collections.Counter)
    del_role_content: collections.Counter = field(default_factory=collections.Counter)


def run(repo: Path, *args: str, check: bool = True) -> str:
    result = subprocess.run(
        ["git", *args], cwd=repo, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if check and result.returncode:
        raise SystemExit(result.stderr.strip() or f"git {' '.join(args)} failed")
    return result.stdout


def git_blob(repo: Path, revision: str, path: str | None) -> list[str]:
    if not path or path == "/dev/null":
        return []
    result = subprocess.run(
        ["git", "show", f"{revision}:{path}"], cwd=repo,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    if result.returncode or b"\x00" in result.stdout[:8192]:
        return []
    return result.stdout.decode("utf-8", errors="replace").splitlines()


def parse_patch(repo: Path, base: str, head: str) -> dict[str, FileDelta]:
    patch = run(
        repo, "-c", "core.quotePath=false", "diff", "--no-ext-diff",
        "--no-color", "--find-renames", "--unified=0", base, head, "--",
    )
    files: dict[str, FileDelta] = {}
    current: FileDelta | None = None
    old_line = new_line = 0
    in_hunk = False

    for raw in patch.splitlines():
        if raw.startswith("diff --git "):
            if raw.startswith("diff --git a/") and " b/" in raw:
                old_field, new_field = raw[len("diff --git "):].split(" b/", 1)
                old_path = old_field.removeprefix("a/")
                new_path = new_field
            else:
                try:
                    fields = shlex.split(raw)
                except ValueError:
                    current = None
                    continue
                if len(fields) != 4 or fields[:2] != ["diff", "--git"]:
                    current = None
                    continue
                old_path = fields[2].removeprefix("a/")
                new_path = fields[3].removeprefix("b/")
            path = new_path
            current = FileDelta(path=path, old_path=old_path, new_path=new_path)
            files[path] = current
            in_hunk = False
        elif current is None:
            continue
        elif raw.startswith("rename from "):
            current.old_path = raw.removeprefix("rename from ")
        elif raw.startswith("rename to "):
            current.new_path = raw.removeprefix("rename to ")
            current.path = current.new_path
        elif raw.startswith("new file mode "):
            current.old_path = None
        elif raw.startswith("deleted file mode "):
            current.new_path = None
            current.path = current.old_path or current.path
        elif raw.startswith("Binary files ") or raw.startswith("GIT binary patch"):
            current.binary = True
            in_hunk = False
        elif raw.startswith("@@ "):
            match = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", raw)
            if not match:
                in_hunk = False
                continue
            old_line = int(match.group(1))
            new_line = int(match.group(3))
            in_hunk = True
        elif in_hunk and raw.startswith("+"):
            current.added.append(ChangeLine("added", new_line, raw[1:]))
            current.additions += 1
            new_line += 1
        elif in_hunk and raw.startswith("-"):
            current.removed.append(ChangeLine("removed", old_line, raw[1:]))
            current.deletions += 1
            old_line += 1
        elif in_hunk and raw.startswith(" "):
            old_line += 1
            new_line += 1
        elif in_hunk and raw.startswith("\\ No newline"):
            pass

    return files


def is_test_path(path: str) -> bool:
    lower = path.lower()
    name = Path(lower).name
    parts = set(Path(lower).parts)
    return (
        any(
            part in {"test", "tests", "testing", "fixtures", "benches", "__tests__"}
            or part.endswith("tests")
            for part in parts
        )
        or "android-contract-tests" in lower
        or ".test." in name
        or ".spec." in name
        or name.startswith("test_")
        or name == "tests.rs"
        or name.endswith("_test.rs")
        or name.endswith("_tests.rs")
    )


def classify_file(path: str) -> str:
    p = path.lower()
    name = Path(p).name
    suffix = Path(p).suffix
    if is_test_path(p):
        return "test"
    if (
        name in {"cargo.lock", "package-lock.json", "deno.lock", "yarn.lock", "pnpm-lock.yaml"}
        or "generated" in Path(p).parts
        or p == "packages/iroh-http-node/index.js"
        or p == "packages/iroh-http-node/index.d.ts"
    ):
        return "generated"
    if p.startswith("examples/"):
        return "example"
    if p.startswith("docs/") or suffix in {".md", ".mdx", ".rst"}:
        return "docs"
    if p.startswith("scripts/") or "/scripts/" in p or p.startswith("xtask/"):
        return "tooling"
    if (
        name in {"cargo.toml", "package.json", "tsconfig.json", "vite.config.ts", "build.rs"}
        or suffix in {".toml", ".yaml", ".yml"}
        or "/permissions/" in p
        or "/capabilities/" in p
        or p.startswith(".github/")
    ):
        return "configuration"
    if suffix in CODE_EXTENSIONS and (
        p.startswith("src/") or "/src/" in p
        or p.startswith("crates/") or p.startswith("packages/")
    ):
        return "operational"
    return "other"


def directory_group(path: str) -> str:
    parts = Path(path).parts
    if not parts:
        return "other"
    if parts[0] in {"crates", "packages", "examples"} and len(parts) > 1:
        return "/".join(parts[:2])
    if parts[0] == "docs" and len(parts) > 1:
        return "/".join(parts[:2])
    if parts[0] == "tests" and len(parts) > 1:
        return "/".join(parts[:2])
    return parts[0]


def rust_test_lines(lines: list[str]) -> set[int]:
    """Return 1-based lines inside cfg(test) or directly test-attributed items."""
    marked: set[int] = set()
    attrs = re.compile(r"^\s*#\[(?:cfg\s*\(\s*test\s*\)|(?:tokio::)?test(?:\s*\([^]]*\))?)\]")
    i = 0
    while i < len(lines):
        if not attrs.search(lines[i]):
            i += 1
            continue
        start = i
        j = i
        depth = 0
        opened = False
        while j < len(lines):
            text = strip_strings_and_line_comments(lines[j])
            depth += text.count("{") - text.count("}")
            opened = opened or "{" in text
            if opened and depth <= 0:
                break
            if not opened and ";" in text and j > start:
                break
            j += 1
        marked.update(range(start + 1, min(j + 1, len(lines)) + 1))
        i = max(i + 1, j + 1)
    return marked


def strip_strings_and_line_comments(line: str) -> str:
    out: list[str] = []
    quote: str | None = None
    escaped = False
    i = 0
    while i < len(line):
        char = line[i]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            out.append(" ")
            i += 1
            continue
        if char in {'"', '`'}:
            quote = char
            out.append(" ")
            i += 1
            continue
        if line.startswith("//", i):
            break
        out.append(char)
        i += 1
    return "".join(out)


def c_family_content(lines: list[str]) -> list[str]:
    result: list[str] = []
    block = False
    html_block = False
    for line in lines:
        if not line.strip():
            result.append("blank")
            continue
        i = 0
        has_code = False
        has_comment = False
        quote: str | None = None
        escaped = False
        while i < len(line):
            if html_block:
                has_comment = True
                end = line.find("-->", i)
                if end < 0:
                    i = len(line)
                else:
                    html_block = False
                    i = end + 3
                continue
            if block:
                has_comment = True
                end = line.find("*/", i)
                if end < 0:
                    i = len(line)
                else:
                    block = False
                    i = end + 2
                continue
            char = line[i]
            if quote:
                has_code = has_code or not char.isspace()
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == quote:
                    quote = None
                i += 1
                continue
            if line.startswith("<!--", i):
                has_comment = True
                html_block = True
                i += 4
            elif line.startswith("/*", i):
                has_comment = True
                block = True
                i += 2
            elif line.startswith("//", i):
                has_comment = True
                break
            elif char in {'"', '`'}:
                quote = char
                has_code = True
                i += 1
            else:
                if not char.isspace():
                    has_code = True
                i += 1
        result.append("code" if has_code else "comment" if has_comment else "blank")
    return result


def hash_comment_content(lines: list[str]) -> list[str]:
    result = []
    for line in lines:
        stripped = line.strip()
        if not stripped:
            result.append("blank")
        elif stripped.startswith("#") and not stripped.startswith("#!["):
            result.append("comment")
        else:
            result.append("code")
    return result


def content_map(path: str, lines: list[str]) -> list[str]:
    suffix = Path(path).suffix.lower()
    name = Path(path).name.lower()
    if suffix in {".py", ".sh", ".bash", ".zsh", ".toml", ".yaml", ".yml"}:
        return hash_comment_content(lines)
    if suffix in CODE_EXTENSIONS or name == "build.rs":
        return c_family_content(lines)
    return ["blank" if not line.strip() else "code" for line in lines]


def annotate(repo: Path, base: str, head: str, files: dict[str, FileDelta]) -> None:
    for delta in files.values():
        delta.role = classify_file(delta.path)
        delta.directory = directory_group(delta.path)
        old_lines = git_blob(repo, base, delta.old_path)
        new_lines = git_blob(repo, head, delta.new_path)
        old_content = content_map(delta.old_path or delta.path, old_lines)
        new_content = content_map(delta.new_path or delta.path, new_lines)
        old_test = rust_test_lines(old_lines) if delta.path.endswith(".rs") else set()
        new_test = rust_test_lines(new_lines) if delta.path.endswith(".rs") else set()

        delta.old_content.update(old_content)
        delta.new_content.update(new_content)
        for number in range(1, len(old_lines) + 1):
            delta.old_roles["test" if number in old_test else delta.role] += 1
        for number in range(1, len(new_lines) + 1):
            delta.new_roles["test" if number in new_test else delta.role] += 1

        for changed in delta.added:
            kind = new_content[changed.number - 1] if changed.number <= len(new_content) else "code"
            role = "test" if changed.number in new_test else delta.role
            delta.add_content[kind] += 1
            delta.add_roles[role] += 1
            delta.add_role_content[(role, kind)] += 1
        for changed in delta.removed:
            kind = old_content[changed.number - 1] if changed.number <= len(old_content) else "code"
            role = "test" if changed.number in old_test else delta.role
            delta.del_content[kind] += 1
            delta.del_roles[role] += 1
            delta.del_role_content[(role, kind)] += 1


def aggregate(files: Iterable[FileDelta], key) -> dict[str, dict]:
    groups: dict[str, dict] = {}
    for delta in files:
        name = key(delta)
        group = groups.setdefault(name, {
            "files": 0, "add": 0, "del": 0,
            "add_content": collections.Counter(), "del_content": collections.Counter(),
            "add_roles": collections.Counter(), "del_roles": collections.Counter(),
            "add_role_content": collections.Counter(),
            "del_role_content": collections.Counter(),
        })
        group["files"] += 1
        group["add"] += delta.additions
        group["del"] += delta.deletions
        group["add_content"].update(delta.add_content)
        group["del_content"].update(delta.del_content)
        group["add_roles"].update(delta.add_roles)
        group["del_roles"].update(delta.del_roles)
        group["add_role_content"].update(delta.add_role_content)
        group["del_role_content"].update(delta.del_role_content)
    return groups


def fmt(value: int) -> str:
    return f"{value:,}"


def pct(part: int, whole: int) -> str:
    return "0.0%" if not whole else f"{100 * part / whole:.1f}%"


def role_bar(counter: collections.Counter, total: int) -> str:
    if not total:
        return '<div class="bar empty"></div>'
    pieces = []
    for role in ROLE_ORDER:
        value = counter[role]
        if value:
            pieces.append(
                f'<span title="{html.escape(ROLE_LABEL[role])}: {fmt(value)}" '
                f'style="width:{100 * value / total:.4f}%;background:{ROLE_COLOR[role]}"></span>'
            )
    return '<div class="bar">' + "".join(pieces) + "</div>"


def metric_card(label: str, value: int | str, note: str = "") -> str:
    return (
        '<div class="metric"><div class="metric-label">' + html.escape(label) +
        '</div><div class="metric-value">' + html.escape(str(value)) +
        '</div><div class="metric-note">' + html.escape(note) + '</div></div>'
    )


def generate_html(
    repo: Path, base: str, head: str, files: dict[str, FileDelta], title: str,
    findings: list[str],
) -> str:
    ordered = sorted(files.values(), key=lambda d: (-(d.additions + d.deletions), d.path))
    dirs = aggregate(ordered, lambda d: d.directory)
    totals = aggregate(ordered, lambda _d: "total").get("total", {
        "files": 0, "add": 0, "del": 0,
        "add_content": collections.Counter(), "del_content": collections.Counter(),
        "add_roles": collections.Counter(), "del_roles": collections.Counter(),
        "add_role_content": collections.Counter(),
        "del_role_content": collections.Counter(),
    })
    add = totals["add"]
    delete = totals["del"]
    add_roles = totals["add_roles"]
    del_roles = totals["del_roles"]
    add_content = totals["add_content"]
    del_content = totals["del_content"]
    operational_changed = add_roles["operational"] + del_roles["operational"]
    test_changed = add_roles["test"] + del_roles["test"]
    operational_code_changed = (
        totals["add_role_content"][("operational", "code")]
        + totals["del_role_content"][("operational", "code")]
    )
    test_code_changed = (
        totals["add_role_content"][("test", "code")]
        + totals["del_role_content"][("test", "code")]
    )
    comment_changed = add_content["comment"] + del_content["comment"]
    code_changed = add_content["code"] + del_content["code"]
    base_oid = run(repo, "rev-parse", base).strip()
    head_oid = run(repo, "rev-parse", head).strip()
    generated_at = dt.datetime.now(dt.timezone.utc).astimezone().isoformat(timespec="seconds")
    merge_base = run(repo, "merge-base", base, head).strip()
    old_roles = sum((d.old_roles for d in ordered), collections.Counter())
    new_roles = sum((d.new_roles for d in ordered), collections.Counter())
    old_content = sum((d.old_content for d in ordered), collections.Counter())
    new_content = sum((d.new_content for d in ordered), collections.Counter())
    footprint = sum(new_content.values()) - sum(old_content.values())
    operational_footprint = new_roles["operational"] - old_roles["operational"]
    test_footprint = new_roles["test"] - old_roles["test"]
    comment_footprint = new_content["comment"] - old_content["comment"]

    cards = "".join([
        metric_card("Files touched", fmt(len(ordered)), "Git paths; renames detected"),
        metric_card("Lines added", f"+{fmt(add)}", f"net {add - delete:+,}"),
        metric_card("Lines removed", f"−{fmt(delete)}", f"{fmt(add + delete)} changed lines total"),
        metric_card("Net footprint", f"{footprint:+,}", "Physical lines across touched files"),
        metric_card("Operational code-bearing", fmt(operational_code_changed), f"within {fmt(operational_changed)} operational lines"),
        metric_card("Test code-bearing", fmt(test_code_changed), f"within {fmt(test_changed)} test/harness lines"),
        metric_card("Operational footprint", f"{operational_footprint:+,}", "Runtime lines after minus before"),
        metric_card("Test footprint", f"{test_footprint:+,}", "Test/harness lines after minus before"),
        metric_card("Comment-only", fmt(comment_changed), f"{pct(comment_changed, add + delete)} of changed lines"),
        metric_card("Comment footprint", f"{comment_footprint:+,}", "Comment-only lines after minus before"),
        metric_card("Code-bearing", fmt(code_changed), f"{pct(code_changed, add + delete)} of changed lines"),
        metric_card("Blank", fmt(add_content["blank"] + del_content["blank"]), "Formatting / separation"),
    ])

    legend = "".join(
        f'<span><i style="background:{ROLE_COLOR[r]}"></i>{html.escape(ROLE_LABEL[r])}</span>'
        for r in ROLE_ORDER
    )
    findings_html = ""
    if findings:
        findings_html = (
            '<h2>Architectural reading notes</h2><div class="panel"><ul>'
            + "".join(f"<li>{html.escape(item)}</li>" for item in findings)
            + "</ul></div>"
        )

    directory_rows = []
    for name, group in sorted(dirs.items(), key=lambda item: (-(item[1]["add"] + item[1]["del"]), item[0])):
        changed = group["add"] + group["del"]
        directory_rows.append(
            "<tr>"
            f"<td><code>{html.escape(name)}</code>{role_bar(group['add_roles'] + group['del_roles'], changed)}</td>"
            f"<td>{fmt(group['files'])}</td><td class='plus'>+{fmt(group['add'])}</td>"
            f"<td class='minus'>−{fmt(group['del'])}</td><td>{group['add'] - group['del']:+,}</td>"
            f"<td>{fmt(group['add_role_content'][('operational', 'code')] + group['del_role_content'][('operational', 'code')])}</td>"
            f"<td>{fmt(group['add_role_content'][('test', 'code')] + group['del_role_content'][('test', 'code')])}</td>"
            f"<td>{fmt(group['add_content']['comment'] + group['del_content']['comment'])}</td>"
            f"<td>{fmt(group['add_content']['blank'] + group['del_content']['blank'])}</td>"
            "</tr>"
        )

    file_rows = []
    for delta in ordered:
        changed = delta.additions + delta.deletions
        primary_role = max(
            ROLE_ORDER, key=lambda role: delta.add_roles[role] + delta.del_roles[role]
        )
        file_rows.append(
            "<tr>"
            f"<td><code>{html.escape(delta.path)}</code>{role_bar(delta.add_roles + delta.del_roles, changed)}</td>"
            f"<td><span class='pill' style='--pill:{ROLE_COLOR[primary_role]}'>{html.escape(ROLE_LABEL[primary_role])}</span></td>"
            f"<td class='plus'>+{fmt(delta.additions)}</td><td class='minus'>−{fmt(delta.deletions)}</td>"
            f"<td>{delta.additions - delta.deletions:+,}</td>"
            f"<td>{fmt(delta.add_role_content[('operational', 'code')] + delta.del_role_content[('operational', 'code')])}</td>"
            f"<td>{fmt(delta.add_role_content[('test', 'code')] + delta.del_role_content[('test', 'code')])}</td>"
            f"<td>{fmt(delta.add_content['comment'] + delta.del_content['comment'])}</td>"
            f"<td>{fmt(delta.add_content['blank'] + delta.del_content['blank'])}</td>"
            "</tr>"
        )

    role_rows = []
    for role in ROLE_ORDER:
        role_add = add_roles[role]
        role_del = del_roles[role]
        if role_add + role_del == 0:
            continue
        role_rows.append(
            "<tr>"
            f"<td><span class='role-dot' style='background:{ROLE_COLOR[role]}'></span>{html.escape(ROLE_LABEL[role])}</td>"
            f"<td class='plus'>+{fmt(role_add)}</td><td class='minus'>−{fmt(role_del)}</td>"
            f"<td>{role_add - role_del:+,}</td>"
            f"<td>{fmt(totals['add_role_content'][(role, 'code')] + totals['del_role_content'][(role, 'code')])}</td>"
            f"<td>{fmt(totals['add_role_content'][(role, 'comment')] + totals['del_role_content'][(role, 'comment')])}</td>"
            f"<td>{fmt(totals['add_role_content'][(role, 'blank')] + totals['del_role_content'][(role, 'blank')])}</td>"
            f"<td>{pct(role_add + role_del, add + delete)}</td>"
            "</tr>"
        )

    data = {
        "base": base_oid, "head": head_oid, "merge_base": merge_base, "files": len(ordered),
        "additions": add, "deletions": delete,
        "footprint_net": footprint,
        "operational_footprint_net": operational_footprint,
        "test_footprint_net": test_footprint,
        "comment_footprint_net": comment_footprint,
        "operational_code_changed": operational_code_changed,
        "test_code_changed": test_code_changed,
        "operational_code_added": totals["add_role_content"][("operational", "code")],
        "operational_code_deleted": totals["del_role_content"][("operational", "code")],
        "test_code_added": totals["add_role_content"][("test", "code")],
        "test_code_deleted": totals["del_role_content"][("test", "code")],
        "roles_added": dict(add_roles), "roles_deleted": dict(del_roles),
        "content_added": dict(add_content), "content_deleted": dict(del_content),
    }

    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{html.escape(title)}</title>
<style>
:root{{--bg:#f4f6fa;--paper:#fff;--ink:#172033;--muted:#687386;--line:#dfe4ec;--plus:#087f5b;--minus:#c13d4a}}
*{{box-sizing:border-box}} body{{margin:0;background:var(--bg);color:var(--ink);font:14px/1.5 Inter,ui-sans-serif,system-ui,-apple-system,sans-serif}}
main{{max-width:1500px;margin:auto;padding:40px 28px 80px}} h1{{font-size:32px;line-height:1.15;margin:0 0 8px}} h2{{margin:36px 0 12px;font-size:21px}}
.subtitle,.note{{color:var(--muted)}} .subtitle code{{color:var(--ink)}} .metrics{{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin:24px 0}}
.metric,.panel{{background:var(--paper);border:1px solid var(--line);border-radius:12px;box-shadow:0 2px 8px #1f2a4410}}
.metric{{padding:16px}} .metric-label{{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.06em}} .metric-value{{font-size:28px;font-weight:750;margin:3px 0}} .metric-note{{color:var(--muted);font-size:12px}}
.panel{{padding:18px;overflow:auto}} table{{width:100%;border-collapse:collapse}} th{{text-align:right;color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.05em;white-space:nowrap}} th:first-child,td:first-child{{text-align:left}} td{{border-top:1px solid var(--line);padding:10px 8px;text-align:right;vertical-align:top}} code{{font:12px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace}}
.plus{{color:var(--plus);font-variant-numeric:tabular-nums}} .minus{{color:var(--minus);font-variant-numeric:tabular-nums}} .bar{{display:flex;height:6px;margin-top:7px;overflow:hidden;border-radius:4px;background:#edf0f5;min-width:180px}} .bar span{{height:100%}} .bar.empty{{background:#edf0f5}}
.legend{{display:flex;flex-wrap:wrap;gap:14px;margin:10px 0 16px;color:var(--muted)}} .legend span{{display:flex;align-items:center;gap:5px}} .legend i,.role-dot{{display:inline-block;width:9px;height:9px;border-radius:50%}}
.pill{{display:inline-flex;align-items:center;padding:2px 8px;border-radius:999px;background:color-mix(in srgb,var(--pill) 13%,white);color:color-mix(in srgb,var(--pill) 80%,black);font-size:11px;white-space:nowrap}}
.callout{{border-left:4px solid #315efb;background:#eaf0ff;padding:13px 16px;border-radius:8px;margin:18px 0}} .method{{display:grid;grid-template-columns:1fr 1fr;gap:16px}} ul{{margin:6px 0 0;padding-left:20px}}
details{{margin-top:14px}} summary{{cursor:pointer;font-weight:650}} .raw{{white-space:pre-wrap;font:12px ui-monospace,SFMono-Regular,Menlo,monospace;background:#111827;color:#dbe5f5;padding:14px;border-radius:8px}}
@media(max-width:900px){{.metrics{{grid-template-columns:repeat(2,1fr)}}.method{{grid-template-columns:1fr}}main{{padding:24px 14px}}}}
</style></head><body><main>
<h1>{html.escape(title)}</h1>
<div class="subtitle">Exact Git diff from <code>{html.escape(base)} ({base_oid[:12]})</code> to <code>{html.escape(head)} ({head_oid[:12]})</code><br>Merge base <code>{merge_base[:12]}</code> · Generated {html.escape(generated_at)}</div>
<div class="metrics">{cards}</div>
<div class="callout"><strong>How to read this:</strong> “Operational” means runtime source, not “all non-test files.” Tests embedded in Rust production files under <code>#[cfg(test)]</code> are counted as tests. Comment-only and blank lines are measured independently from role. Churn measures review work; footprint measures the before/after size of touched files.</div>
{findings_html}
<h2>Change role</h2><div class="panel"><table><thead><tr><th>Role</th><th>Added</th><th>Removed</th><th>Net</th><th>Code-bearing</th><th>Comments</th><th>Blank</th><th>Share of churn</th></tr></thead><tbody>{''.join(role_rows)}</tbody></table></div>
<h2>Per directory</h2><div class="legend">{legend}</div><div class="panel"><table><thead><tr><th>Directory</th><th>Files</th><th>Added</th><th>Removed</th><th>Net</th><th>Operational code</th><th>Test code</th><th>Comments</th><th>Blank</th></tr></thead><tbody>{''.join(directory_rows)}</tbody></table></div>
<h2>Per file</h2><div class="panel"><table><thead><tr><th>File</th><th>Primary role</th><th>Added</th><th>Removed</th><th>Net</th><th>Operational code</th><th>Test code</th><th>Comments</th><th>Blank</th></tr></thead><tbody>{''.join(file_rows)}</tbody></table></div>
<h2>Method and limitations</h2><div class="method">
<div class="panel"><strong>Exact measurements</strong><ul><li>File list and additions/removals come from Git’s zero-context patch.</li><li>Deleted lines are classified against the base blob; additions against the head blob.</li><li>Footprint compares complete before/after versions of every touched text file.</li><li>Directory totals sum exactly to the headline Git totals.</li><li>Binary files are listed but have no textual line count.</li></ul></div>
<div class="panel"><strong>Heuristic classifications</strong><ul><li>Role is path-based, with Rust <code>#[cfg(test)]</code>/<code>#[test]</code> regions detected line-by-line.</li><li>Comment-only detection understands C-family block/line comments, HTML comments, and hash comments. A line containing both code and an inline comment is “code-bearing.”</li><li>Markdown prose is documentation content, not a source-code comment.</li><li>Raw strings, macros, unusual generated layouts, and renamed files can require manual review.</li></ul></div>
</div>
<details><summary>Machine-readable totals</summary><div class="raw">{html.escape(json.dumps(data, indent=2, sort_keys=True))}</div></details>
</main></body></html>"""


def validate(files: dict[str, FileDelta]) -> None:
    for delta in files.values():
        if sum(delta.add_content.values()) != delta.additions:
            raise SystemExit(f"addition content mismatch: {delta.path}")
        if sum(delta.del_content.values()) != delta.deletions:
            raise SystemExit(f"deletion content mismatch: {delta.path}")
        if sum(delta.add_roles.values()) != delta.additions:
            raise SystemExit(f"addition role mismatch: {delta.path}")
        if sum(delta.del_roles.values()) != delta.deletions:
            raise SystemExit(f"deletion role mismatch: {delta.path}")
        if sum(delta.add_role_content.values()) != delta.additions:
            raise SystemExit(f"addition role/content mismatch: {delta.path}")
        if sum(delta.del_role_content.values()) != delta.deletions:
            raise SystemExit(f"deletion role/content mismatch: {delta.path}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--title", default="Git change line analysis")
    parser.add_argument("--finding", action="append", default=[])
    args = parser.parse_args()
    repo = args.repo.resolve()
    files = parse_patch(repo, args.base, args.head)
    annotate(repo, args.base, args.head, files)
    validate(files)
    report = generate_html(repo, args.base, args.head, files, args.title, args.finding)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(report, encoding="utf-8")
    print(args.output.resolve())


if __name__ == "__main__":
    main()
