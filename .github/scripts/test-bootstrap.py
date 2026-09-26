#!/usr/bin/env python3
"""Regression checks for required and optional mise bootstrap boundaries."""

import json
import pathlib
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[2]
MISE = tomllib.loads((ROOT / "mise.toml").read_text(encoding="utf-8"))
TASKS = MISE["tasks"]


def dependency_closure(task: str) -> set[str]:
    """Return task and all transitive mise dependencies, rejecting cycles."""
    closure: set[str] = set()
    visiting: set[str] = set()

    def visit(name: str) -> None:
        assert name in TASKS, f"unknown mise dependency {name!r}"
        assert name not in visiting, f"mise dependency cycle at {name!r}"
        if name in closure:
            return
        visiting.add(name)
        for dependency in TASKS[name].get("depends", []):
            visit(dependency)
        visiting.remove(name)
        closure.add(name)

    visit(task)
    return closure


def main() -> None:
    required = dependency_closure("bootstrap-ci")
    assert "cargo-deny-install" in required
    assert "web-agent-install" not in required
    assert "web-agent" not in required

    local = dependency_closure("bootstrap")
    assert {"bootstrap-ci", "cargo-deny-install", "web-agent-install"} <= local

    cargo_deny = TASKS["cargo-deny-install"]["run"]
    assert cargo_deny == "cargo install cargo-deny --version 0.20.2 --locked"
    assert "||" not in cargo_deny

    devcontainer = json.loads(
        (ROOT / ".devcontainer/devcontainer.json").read_text(encoding="utf-8")
    )
    assert devcontainer["postCreateCommand"] == "mise install && mise bootstrap-ci"
    assert "postStartCommand" not in devcontainer


if __name__ == "__main__":
    main()
