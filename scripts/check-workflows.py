#!/usr/bin/env python3
"""Rejects a workflow file GitHub would refuse to read.

An invalid workflow does not fail loudly: the run finishes in about a second with
zero jobs, and the Actions page shows the file's path where its name should be. It
looks like nothing ran, because nothing did.

A plain `yaml.safe_load` is not enough to catch that. PyYAML accepts a mapping with
the same key twice and keeps the last one, so a duplicated `retention-days` parsed
cleanly here and was rejected there. This loader treats a duplicate as the error it
is, and the checks below cover the two other ways this repository has broken a
workflow before.
"""

import sys
import pathlib

import yaml


class StrictLoader(yaml.SafeLoader):
    """A loader that refuses a mapping key it has already seen."""


def _no_duplicates(loader, node, deep=False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            mark = key_node.start_mark
            raise yaml.constructor.ConstructorError(
                None, None,
                f"duplicate key {key!r} at line {mark.line + 1}", key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _no_duplicates
)


def check(path: pathlib.Path) -> list[str]:
    problems = []
    text = path.read_text(encoding="utf-8")

    try:
        document = yaml.load(text, Loader=StrictLoader)
    except yaml.YAMLError as error:
        return [f"{path}: {error}"]

    if not isinstance(document, dict):
        return [f"{path}: not a mapping"]

    if "name" not in document:
        problems.append(f"{path}: no name, so the Actions page shows the file path")

    # `secrets` inside an `if:` makes GitHub reject the whole file. This cost a release
    # workflow that never ran once: every trigger failed in a second with no jobs.
    for line_number, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith(("if:", "- if:")) and "secrets." in stripped:
            problems.append(
                f"{path}:{line_number}: `secrets` in an `if:` — pass it through `env` "
                f"and check it inside the step"
            )

    return problems


def main() -> int:
    directory = pathlib.Path(".github/workflows")
    files = sorted(directory.glob("*.yml")) + sorted(directory.glob("*.yaml"))
    if not files:
        print(f"no workflows found in {directory}")
        return 1

    problems = [problem for path in files for problem in check(path)]
    for problem in problems:
        print(problem)

    if problems:
        return 1

    print(f"{len(files)} workflow files are readable.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
