#!/usr/bin/env python3
"""Regression checks for the shared mise quality-gate bootstrap."""

import json
import pathlib
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[2]
MISE = tomllib.loads((ROOT / "mise.toml").read_text(encoding="utf-8"))
TASKS = MISE["tasks"]


def main() -> None:
    assert TASKS["bootstrap"]["depends"] == ["cargo-deny-install"]
    assert "run" not in TASKS["bootstrap"]
    assert TASKS["cargo-deny-install"]["run"] == (
        "cargo install cargo-deny --version 0.20.2 --locked"
    )

    devcontainer = json.loads(
        (ROOT / ".devcontainer/devcontainer.json").read_text(encoding="utf-8")
    )
    assert devcontainer["postCreateCommand"] == "mise install && mise bootstrap"
    assert "postStartCommand" not in devcontainer


if __name__ == "__main__":
    main()
