#!/usr/bin/env python3
"""Seam audit for the Farder Tauri client.

Cross-checks three surfaces that have no compile-time link:
  1. every invoke("X") in tauri-bridge.ts  <->  a registered #[tauri::command] in main.rs
  2. every listen("server:...") in useServerEvents.ts  <->  an emit("server:...") in bridge.rs
  3. every emit("server:...") in bridge.rs  <->  a listen in useServerEvents.ts (no orphan emit)
Exits non-zero on any hard mismatch.
"""
import re, sys, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
bridge_ts = (ROOT / "client/src/lib/tauri-bridge.ts").read_text()
main_rs = (ROOT / "client/src-tauri/src/main.rs").read_text()
use_events = (ROOT / "client/src/hooks/useServerEvents.ts").read_text()
bridge_rs = (ROOT / "client/src-tauri/src/bridge.rs").read_text()

# 1. invoke("...") names
invokes = set(re.findall(r'invoke(?:<[^>]*>)?\(\s*["\']([a-zA-Z0-9_]+)["\']', bridge_ts))

# registered commands: every `module::fn` and bare `fn` inside generate_handler![...]
block = re.search(r'generate_handler!\[(.*?)\]', main_rs, re.S).group(1)
registered = set()
for entry in re.findall(r'([a-zA-Z_][a-zA-Z0-9_]*)\s*::\s*([a-zA-Z_][a-zA-Z0-9_]*)', block):
    registered.add(entry[1])
for entry in re.findall(r'(?:^|,)\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*(?:,|$)', block):
    if '::' not in entry and entry.strip():
        registered.add(entry.strip())

# 2/3. server:* events
listens = set(re.findall(r'listen\(\s*["\'](server:[a-zA-Z0-9_]+)["\']', use_events))
emits = set(re.findall(r'emit\(\s*["\'](server:[a-zA-Z0-9_]+)["\']', bridge_rs))

fail = False

missing_reg = invokes - registered
if missing_reg:
    print("INVOKE with no registered command:")
    for x in sorted(missing_reg): print(f"  - {x}")
    fail = True

unused_cmd = registered - invokes
if unused_cmd:
    print("NOTE: registered commands with no invoke in tauri-bridge.ts (may be Rust-only):")
    for x in sorted(unused_cmd): print(f"  - {x}")

missing_emit = listens - emits
if missing_emit:
    print("LISTEN with no matching bridge emit:")
    for x in sorted(missing_emit): print(f"  - {x}")
    fail = True

orphan_emit = emits - listens
if orphan_emit:
    print("EMIT with no matching listen (orphan event):")
    for x in sorted(orphan_emit): print(f"  - {x}")
    fail = True

print(f"\ninvokes={len(invokes)} registered={len(registered)} listens={len(listens)} emits={len(emits)}")
if fail:
    print("SEAM AUDIT FAILED")
    sys.exit(1)
print("SEAM AUDIT PASSED")
