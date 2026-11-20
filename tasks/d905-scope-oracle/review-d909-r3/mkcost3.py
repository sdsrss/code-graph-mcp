#!/usr/bin/env python3
"""mkcost3.py <out-dir>: the inputs 0.204.0's notes time a bare call's two
scope picks on (pre-tag review of 0.204.0, round 3, MEDIUM-1). Every call sees
every same-file `h`, so both picks are O(calls x namesakes).

  a1_nested_calls_16k     16,000 functions that each define and call their own h (901 KB)
  a5_min_nested_calls_40k 40,000 of them, minimal text (1,000,000 bytes)
  a7_js_nested_calls_16k  the a1 shape in JavaScript (869 KB)

Time with ../timeab.sh (fresh copy per run, arms alternating)."""
import os, sys

out = sys.argv[1]
def write(case, name, text):
    d = os.path.join(out, case)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name), "w", newline="\n") as f:
        f.write(text)

write("a1_nested_calls_16k", "m.py", "".join(
    f"def f{i}():\n    def h():\n        pass\n    return h()\n\n\n" for i in range(16000)))
write("a1_nested_calls_16k", "other.py", "def z():\n    pass\n")
write("a5_min_nested_calls_40k", "m.py", "def a():\n def h():0\n h()\n" * 40000)
write("a7_js_nested_calls_16k", "m.js", "".join(
    f"function f{i}() {{\n  function h() {{}}\n  return h();\n}}\n\n" for i in range(16000)))
