"""Present LLVM totals by scope/platform, never average incompatible denominators."""

import json
import os
from pathlib import Path
import sys


def expected_ids():
    return [f"workspace-{platform}" for platform in (
        "linux-arm64", "macos-arm64"
    )] + [
        f"{suite}-linux-{arch}-{baseline}"
        for suite in ("devcontainer-v1", "openssh")
        # x86_64 is intentionally disabled while cdenv targets ARM hosts only.
        for arch in ("arm64",)
        for baseline in ("minimum", "pinned")
    ]


def index(source, destination):
    destination.mkdir(parents=True, exist_ok=True)
    reports = {}
    errors = []
    lines = ["# Informational coverage", "",
             "Percentages are not pass/fail thresholds. Missing/failed reports are errors.", "",
             "| Scope | Functions covered/total | Branches covered/total |",
             "| --- | ---: | ---: |"]
    for identifier in expected_ids():
        directory = source / f"coverage-{identifier}"
        try:
            if (directory / "test-exit-code.txt").read_text().strip() != "0":
                raise ValueError("test/gate failed; any report is partial")
            if (directory / "report-exit-code.txt").read_text().strip() != "0":
                raise ValueError("report generation failed")
            for filename in ("coverage.lcov", "html/index.html", "versions.txt", "tests.log"):
                if (directory / filename).stat().st_size == 0:
                    raise ValueError(f"empty {filename}")
            data = json.loads((directory / "coverage.json").read_text())
            if data["type"] != "llvm.coverage.json.export":
                raise ValueError("not an LLVM coverage export")
            # cargo-llvm-cov exports one merged data record per invocation.
            if len(data["data"]) != 1:
                raise ValueError("expected one merged LLVM data record")
            totals = data["data"][0]["totals"]
            if totals["functions"]["count"] == 0 or totals["branches"]["count"] == 0:
                raise ValueError("missing function or branch instrumentation")
            reports[identifier] = totals
            functions = totals["functions"]
            branches = totals["branches"]
            lines.append(f"| {identifier} | {functions['covered']}/{functions['count']} | "
                         f"{branches['covered']}/{branches['count']} |")
        except (OSError, ValueError, KeyError, TypeError) as error:
            errors.append(f"{identifier}: {error}")
            lines.append(f"| {identifier} | **MISSING/FAILED** | **MISSING/FAILED** |")
    document = {"reports": reports, "errors": errors}
    (destination / "index.json").write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
    lines += ["", *errors]
    markdown = "\n".join(lines) + "\n"
    (destination / "index.md").write_text(markdown)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as output:
            output.write(markdown)
    return bool(errors)


if __name__ == "__main__":
    sys.exit(index(Path(sys.argv[1]), Path(sys.argv[2])))
