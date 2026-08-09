#!/usr/bin/env python3
"""Check site/wallpapers.json before it reaches the published page.

A contributed entry with a typo'd status renders as an unstyled card, and a
duplicate id silently shadows an existing result -- both are invisible in a diff
and obvious here. Run it locally the same way CI does: `python3 site/validate.py`.
"""

import json
import pathlib
import sys

SITE = pathlib.Path(__file__).parent
STATUSES = {"works", "partial", "broken", "asset"}
TYPES = {"scene", "video", "web", "image", "asset", "application", "unknown"}
FIELDS = {"id", "title", "type", "status", "notes"}

errors = []


def check(entry, index):
    where = f"entry {index}"
    if not isinstance(entry, dict):
        errors.append(f"{where}: not an object")
        return
    where = f"entry {index} ({entry.get('id', 'no id')})"

    missing = FIELDS - entry.keys()
    if missing:
        errors.append(f"{where}: missing {', '.join(sorted(missing))}")
    for extra in entry.keys() - FIELDS:
        errors.append(f"{where}: unknown field {extra!r}")

    item_id = entry.get("id")
    if not isinstance(item_id, str) or not item_id.isdigit():
        errors.append(f"{where}: id must be the numeric Workshop id, as a string")
    if not entry.get("title", "").strip():
        errors.append(f"{where}: title is empty")
    if entry.get("status") not in STATUSES:
        errors.append(
            f"{where}: status {entry.get('status')!r} is not one of "
            f"{', '.join(sorted(STATUSES))}"
        )
    if entry.get("type") not in TYPES:
        errors.append(
            f"{where}: type {entry.get('type')!r} is not one of "
            f"{', '.join(sorted(TYPES))}"
        )
    # A bare "broken" tells a reader nothing they can act on.
    if entry.get("status") in {"partial", "broken"} and not entry.get("notes", "").strip():
        errors.append(f"{where}: status {entry['status']} needs notes saying what is wrong")


def main():
    path = SITE / "wallpapers.json"
    try:
        data = json.loads(path.read_text())
    except json.JSONDecodeError as err:
        print(f"wallpapers.json is not valid JSON: {err}", file=sys.stderr)
        return 1

    entries = data.get("wallpapers")
    if not isinstance(entries, list):
        print("wallpapers.json has no `wallpapers` array", file=sys.stderr)
        return 1

    seen = {}
    for index, entry in enumerate(entries):
        check(entry, index)
        if isinstance(entry, dict) and (item_id := entry.get("id")):
            if item_id in seen:
                errors.append(f"entry {index}: id {item_id} already listed at entry {seen[item_id]}")
            seen[item_id] = index

    # Screenshots are optional (web items cannot be captured without a CEF
    # build), but an orphan file is a rename that lost its entry.
    listed = set(seen)
    for shot in sorted((SITE / "shots").glob("*.jpg")):
        if shot.stem not in listed:
            errors.append(f"shots/{shot.name}: no entry with that id in wallpapers.json")

    if errors:
        print(f"{len(errors)} problem(s) in the compatibility list:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1

    print(f"wallpapers.json OK — {len(entries)} entries, {len(list((SITE / 'shots').glob('*.jpg')))} screenshots")
    return 0


if __name__ == "__main__":
    sys.exit(main())
