#!/usr/bin/env python3
"""Reachability audit: which Tauri commands can a user actually reach?

`seam_audit.py` answers "do the names line up". This answers the question that
keeps finding real bugs here: **is there a path from the UI to this command at
all?** Every gap it has found so far looked finished from both ends — the Rust
command was written, registered and documented, and a bridge wrapper existed —
while nothing in the app ever called the wrapper. Blocking shipped with no way
to unblock. The "delete my data" flow shipped with no UI. Encrypted-channel
search shipped with nothing to search from.

A command counts as REACHABLE when some file outside `client/src/lib` calls a
bridge wrapper for it, directly or through other bridge wrappers.

Deliberately dormant commands live in `scripts/dormant_allowlist.txt`, one
`name: why` per line. Anything unreachable and unlisted fails the audit — the
point being that "unreachable" should be a decision someone wrote down, not a
thing nobody noticed.
"""
import re
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "client/src"
LIB = SRC / "lib"
ALLOWLIST = ROOT / "scripts/dormant_allowlist.txt"

# Top-level definitions in a lib file, exported or not. The unexported ones
# matter: a chain from a component into a bridge wrapper often passes through a
# module-private helper, and stopping at `export` breaks the chain and reports a
# live command as dormant.
FUNC_SPLIT = re.compile(
    r"^(?:export\s+)?(?:async\s+)?function\s+(\w+)"
    r"|^(?:export\s+)?const\s+(\w+)\s*[:=]",
    re.M,
)
INVOKE = re.compile(r'invoke(?:<[^>]*>)?\(\s*["\']([a-zA-Z0-9_]+)["\']')


def registered_commands() -> set[str]:
    main_rs = (ROOT / "client/src-tauri/src/main.rs").read_text()
    block = re.search(r"generate_handler!\[(.*?)\]", main_rs, re.S).group(1)
    names = set()
    for line in block.splitlines():
        line = line.strip().rstrip(",")
        if not line or line.startswith("//"):
            continue
        names.add(line.split("::")[-1])
    return names


def lib_functions() -> dict[str, tuple[str, set[str]]]:
    """Every exported function in client/src/lib, as name -> (body, commands)."""
    out: dict[str, tuple[str, set[str]]] = {}
    for path in LIB.rglob("*.ts"):
        text = path.read_text()
        marks = [(m.start(), m.group(1) or m.group(2)) for m in FUNC_SPLIT.finditer(text)]
        for i, (start, name) in enumerate(marks):
            end = marks[i + 1][0] if i + 1 < len(marks) else len(text)
            body = text[start:end]
            out[name] = (body, set(INVOKE.findall(body)))
    return out


def app_files() -> list[str]:
    """Everything that is not a bridge wrapper — components, hooks, context."""
    return [
        p.read_text()
        for p in SRC.rglob("*.ts*")
        if LIB not in p.parents and p != LIB
    ]


def load_allowlist() -> dict[str, str]:
    if not ALLOWLIST.exists():
        return {}
    out = {}
    for line in ALLOWLIST.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, _, why = line.partition(":")
        out[name.strip()] = why.strip()
    return out


def main() -> int:
    registered = registered_commands()
    funcs = lib_functions()
    app = app_files()

    # Roots: a bridge wrapper named anywhere outside lib/.
    reachable: set[str] = set()
    for name in funcs:
        pattern = re.compile(rf"\b{re.escape(name)}\s*\(")
        if any(pattern.search(t) for t in app):
            reachable.add(name)

    # ...plus whatever those wrappers call, transitively, inside lib/.
    changed = True
    while changed:
        changed = False
        for name in list(reachable):
            body = funcs[name][0]
            for other in funcs:
                if other in reachable or other == name:
                    continue
                if re.search(rf"\b{re.escape(other)}\s*\(", body):
                    reachable.add(other)
                    changed = True

    live: set[str] = set()
    for name in reachable:
        live |= funcs[name][1]
    # A component may also invoke directly, without a wrapper.
    for text in app:
        live |= set(INVOKE.findall(text))

    allow = load_allowlist()
    unreachable = sorted(registered - live)
    unexplained = [c for c in unreachable if c not in allow]

    print(f"registered={len(registered)} reachable={len(live & registered)} "
          f"dormant={len(unreachable)} (allowlisted {len(unreachable) - len(unexplained)})")

    if unexplained:
        print("\nUNREACHABLE from the UI, and not recorded as deliberate:")
        for name in unexplained:
            print(f"  - {name}")
        print("\nEither wire it up, or add it to scripts/dormant_allowlist.txt with the reason.")
        print("REACHABILITY AUDIT FAILED")
        return 1

    print("REACHABILITY AUDIT PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
