#!/usr/bin/env python3
"""Every registered Tauri command must appear in `docs/modules/tauri-commands.md`.

CLAUDE.md treats docs like tests: "drift is not done". This is the mechanical
half of that rule for the command surface, which is the one place where an
undocumented entry is invisible — the command works, so nothing complains, and
the next person to touch it reads the implementation instead of the doc.

Matching is deliberately loose (a command counts as documented if its name
appears in a heading, a code span or an invoke-name line), because the goal is
"somebody wrote about this", not a format.
"""
import re
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
MAIN = ROOT / "client/src-tauri/src/main.rs"
DOC = ROOT / "docs/modules/tauri-commands.md"


def registered() -> list[str]:
    block = re.search(r"generate_handler!\[(.*?)\]", MAIN.read_text(), re.S).group(1)
    out = []
    for line in block.splitlines():
        line = line.strip().rstrip(",")
        if line and not line.startswith("//"):
            out.append(line.split("::")[-1])
    return out


def main() -> int:
    doc = DOC.read_text()
    cmds = registered()
    missing = [c for c in cmds if f"`{c}(" not in doc and f"`{c}`" not in doc and f'"{c}"' not in doc]
    print(f"registered={len(cmds)} documented={len(cmds) - len(missing)}")
    if missing:
        print("\nNOT IN docs/modules/tauri-commands.md:")
        for c in missing:
            print(f"  - {c}")
        print("\nDOC AUDIT FAILED")
        return 1
    print("DOC AUDIT PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
