#!/usr/bin/env python3
"""Regression tests for the standalone Git diff report generator."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

import git_diff_report as report


class GitDiffReportTests(unittest.TestCase):
    def test_classifies_nested_skill_scripts_as_tooling(self) -> None:
        self.assertEqual(
            report.classify_file(".agents/skills/example/scripts/report.py"),
            "tooling",
        )

    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp_dir.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Skill Test")
        self.git("config", "user.email", "skill-test@example.invalid")

        (self.repo / "src").mkdir()
        (self.repo / "docs").mkdir()
        (self.repo / "src/lib.rs").write_text(
            "pub fn answer() -> u8 { 42 }\n\n"
            "#[cfg(test)]\nmod tests {\n"
            "    #[test]\n    fn answers() {\n"
            "        assert_eq!(super::answer(), 42);\n    }\n}\n",
            encoding="utf-8",
        )
        (self.repo / "docs/notes.md").write_text(
            "---\n# Notes\n---\n",
            encoding="utf-8",
        )
        (self.repo / "asset.bin").write_bytes(b"\x00\x01\x02")
        self.git("add", ".")
        self.git("commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").strip()

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def git(self, *args: str) -> str:
        result = subprocess.run(
            ["git", *args],
            cwd=self.repo,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return result.stdout

    def commit(self, message: str) -> str:
        self.git("add", "-A")
        self.git("commit", "-qm", message)
        return self.git("rev-parse", "HEAD").strip()

    def analyze(self, head: str) -> dict[str, report.FileDelta]:
        files = report.parse_patch(self.repo, self.base, head)
        report.annotate(self.repo, self.base, head, files)
        report.validate(files)
        return files

    def test_classifies_operational_and_inline_rust_test_changes(self) -> None:
        source = self.repo / "src/lib.rs"
        source.write_text(
            source.read_text(encoding="utf-8").replace(
                "pub fn answer() -> u8 { 42 }",
                "// Public answer\npub fn answer() -> u8 { 42 }",
            ).replace(
                "assert_eq!(super::answer(), 42);",
                "assert_eq!(super::answer(), 42);\n"
                "        assert_ne!(super::answer(), 0);",
            ),
            encoding="utf-8",
        )
        head = self.commit("change rust")
        files = self.analyze(head)
        delta = files["src/lib.rs"]

        self.assertEqual(delta.additions, 2)
        self.assertEqual(delta.add_roles["operational"], 1)
        self.assertEqual(delta.add_roles["test"], 1)
        self.assertEqual(delta.add_content["comment"], 1)
        self.assertEqual(delta.add_role_content[("test", "code")], 1)

    def test_preserves_a_rename_with_spaces_as_one_zero_churn_file(self) -> None:
        self.git("mv", "docs/notes.md", "docs/renamed notes.md")
        head = self.commit("rename notes")
        files = self.analyze(head)

        self.assertEqual(list(files), ["docs/renamed notes.md"])
        delta = files["docs/renamed notes.md"]
        self.assertEqual(delta.old_path, "docs/notes.md")
        self.assertEqual(delta.new_path, "docs/renamed notes.md")
        self.assertEqual((delta.additions, delta.deletions), (0, 0))

    def test_surfaces_binary_changes_without_inventing_line_counts(self) -> None:
        (self.repo / "asset.bin").write_bytes(b"\x00\x03\x04\x05")
        head = self.commit("change binary")
        files = self.analyze(head)

        delta = files["asset.bin"]
        self.assertTrue(delta.binary)
        self.assertEqual((delta.additions, delta.deletions), (0, 0))

    def test_matches_git_numstat_for_added_and_deleted_text_files(self) -> None:
        (self.repo / "docs/notes.md").unlink()
        (self.repo / "tests").mkdir()
        (self.repo / "tests/new.test.ts").write_text(
            "// test helper\nDeno.test('works', () => {});\n",
            encoding="utf-8",
        )
        head = self.commit("replace text")
        files = self.analyze(head)
        expected_additions = 0
        expected_deletions = 0
        for line in self.git("diff", "--numstat", self.base, head).splitlines():
            added, deleted, _path = line.split("\t", 2)
            expected_additions += int(added)
            expected_deletions += int(deleted)

        self.assertEqual(sum(item.additions for item in files.values()), expected_additions)
        self.assertEqual(sum(item.deletions for item in files.values()), expected_deletions)
        self.assertEqual(files["tests/new.test.ts"].add_roles["test"], 2)
        self.assertEqual(files["docs/notes.md"].deletions, 3)

    def test_generates_an_empty_report_and_escapes_findings(self) -> None:
        files = self.analyze(self.base)
        rendered = report.generate_html(
            self.repo,
            self.base,
            self.base,
            files,
            "Empty <report>",
            ["Inspect <script>alert(1)</script>"],
        )

        self.assertIn("Empty &lt;report&gt;", rendered)
        self.assertIn("Inspect &lt;script&gt;alert(1)&lt;/script&gt;", rendered)
        self.assertIn('&quot;additions&quot;: 0', rendered)
        self.assertNotIn("<script>alert(1)</script>", rendered)


if __name__ == "__main__":
    unittest.main()
