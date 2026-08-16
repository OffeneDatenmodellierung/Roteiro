"""FIXTURE — deliberately unsafe. Never imported, never run. See ../README.md."""

import subprocess


def archive(name):
    """Runs through a shell, so `name` becomes shell syntax."""
    return subprocess.run(f"tar czf {name}.tgz {name}", shell=True, check=False)


def apply_rule(expression, context):
    """Executes whatever the caller supplied."""
    return eval(expression, {}, context)
