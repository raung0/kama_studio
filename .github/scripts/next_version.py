#!/usr/bin/env python3
import re
import subprocess
import sys
from datetime import datetime, timezone

RELEASE_LABELS = {"alpha", "beta", "rc", "stable"}
LABELS = {"dev", *RELEASE_LABELS}


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], text=True).strip()


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in LABELS:
        labels = ", ".join(sorted(LABELS))
        print(f"usage: {sys.argv[0]} <{labels}>", file=sys.stderr)
        return 2

    label = sys.argv[1]
    month = datetime.now(timezone.utc).strftime("%Y.%m")

    if label == "dev":
        print(f"{month}-dev")
        return 0

    pattern = f"{month}-{label}.*"
    regex = re.compile(rf"^{re.escape(month)}-{re.escape(label)}\.(\d+)$")
    tags = git("tag", "--list", pattern).splitlines()
    numbered: list[tuple[int, str]] = []
    for tag in tags:
        match = regex.fullmatch(tag)
        if match:
            numbered.append((int(match.group(1)), tag))

    head_tags = set(git("tag", "--points-at", "HEAD", "--list", pattern).splitlines())
    existing = [(number, tag) for number, tag in numbered if tag in head_tags]
    if existing:
        print(max(existing)[1])
        return 0

    next_number = max((number for number, _ in numbered), default=0) + 1
    print(f"{month}-{label}.{next_number}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
