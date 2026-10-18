"""Select CI from a git diff and local Cargo dependencies, without network access."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def workspace_graph(root):
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    manifests = {}
    for member in workspace["workspace"]["members"]:
        for directory in root.glob(member):
            data = tomllib.loads((directory / "Cargo.toml").read_text(encoding="utf-8"))
            manifests[directory.resolve()] = (data["package"]["name"], data)
    if not manifests:
        raise ValueError("empty workspace")
    reverse = {name: set() for name, _ in manifests.values()}
    shared = workspace["workspace"].get("dependencies", {})
    for directory, (name, data) in manifests.items():
        for table in [data, *data.get("target", {}).values()]:
            for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
                for alias, dependency in table.get(kind, {}).items():
                    if not isinstance(dependency, dict):
                        continue
                    dependency_root = directory
                    if dependency.get("workspace"):
                        dependency = shared[alias]
                        dependency_root = root
                    if isinstance(dependency, dict) and "path" in dependency:
                        path = (dependency_root / dependency["path"]).resolve()
                        if path in manifests:
                            reverse[manifests[path][0]].add(name)
    return {p.relative_to(root.resolve()).as_posix(): n for p, (n, _) in manifests.items()}, reverse


def full_plan(reason):
    return dict(rust=True, full=True, ui=True, recipes=True, llama=True,
                daemon_e2e=True, cli_e2e=True, packages=[], reason=reason)


def select_checks(paths, root=ROOT, force_full=False):
    if force_full:
        return full_plan("full run requested")
    try:
        directories, reverse = workspace_graph(root)
    except (OSError, ValueError, KeyError) as error:
        return full_plan(f"workspace graph unavailable: {error}")
    selected = set()
    ui = False
    for raw_path in paths:
        path = raw_path.replace("\\", "/").removeprefix("./")
        if (Path(path).name in {"Cargo.toml", "Cargo.lock", "build.rs"}
                or ".cargo" in Path(path).parts
                or Path(path).name.startswith("rust-toolchain")
                or path.startswith((".github/", ".cargo/", "scripts/", "vendor/", "rust-toolchain"))):
            return full_plan(f"build/CI configuration: {path}")
        if path.startswith("apps/akasha-ui/"):
            ui = True
            continue
        if path.startswith("docs/user/") or path == "docs/user_guide_final.md":
            return full_plan(f"runtime documentation: {path}")
        if (path in {"README.md", "AGENTS.md", "LICENSE", "LICENSE.md", ".gitignore"}
                or (path.endswith(".md") and path.startswith(("docs/", "spec/")))):
            continue
        package = next((name for directory, name in directories.items()
                        if path.startswith(directory + "/")), None)
        if package:
            selected.add(package)
        else:
            return full_plan(f"unclassified/runtime input: {path}")
    affected = set(selected)
    pending = list(selected)
    while pending:
        for dependent in reverse[pending.pop()]:
            if dependent not in affected:
                affected.add(dependent)
                pending.append(dependent)
    if affected & {"akasha-core", "akasha-daemon"}:
        return full_plan("core or daemon affected (including reverse dependencies)")
    return dict(rust=bool(affected), full=False, ui=ui, recipes=False,
                llama=bool(affected & {"akasha-embedded-llm", "akasha-llm"}),
                daemon_e2e="akasha-daemon" in affected,
                cli_e2e="akasha-cli" in affected, packages=sorted(affected),
                reason="affected packages/UI" if affected or ui else "documentation only")


def changed_paths(base, head, root=ROOT):
    # Both sides of moves must be classified, especially code -> documentation.
    output = subprocess.check_output(
        ["git", "diff", "--name-only", "--no-renames", "-z", base, head, "--"], cwd=root)
    return [p.decode("utf-8") for p in output.split(b"\0") if p]


def write_outputs(path, plan):
    with open(path, "a", encoding="utf-8") as stream:
        for key, value in plan.items():
            # JSON also escapes newlines in untrusted Git filenames/reasons.
            stream.write(f"{key}={json.dumps(value, ensure_ascii=True)}\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--full", action="store_true")
    args = parser.parse_args()
    try:
        if args.full or not args.base or set(args.base) == {"0"}:
            plan = full_plan("manual run or missing baseline")
        else:
            plan = select_checks(changed_paths(args.base, args.head))
    except (subprocess.CalledProcessError, UnicodeError):
        plan = full_plan("git diff unavailable")
    print(json.dumps(plan, ensure_ascii=False, indent=2))
    if output := os.environ.get("GITHUB_OUTPUT"):
        write_outputs(output, plan)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write("### CI selection\n\n    " + json.dumps(plan) + "\n")


if __name__ == "__main__":
    main()
