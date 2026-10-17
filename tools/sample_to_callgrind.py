#!/usr/bin/env python3
"""Turn a macOS `sample` report into a callgrind file KCachegrind can open.

valgrind does not run on Apple Silicon, so callgrind itself is not an option on a
Mac — but KCachegrind (`brew install qcachegrind`) only wants the *file format*,
and `sample(1)`, which ships with macOS and needs no Xcode, already records the
whole call tree with a sample count on every node. This rewrites one as the other:
self cost per function, inclusive cost per call edge, one event (`Samples`, at the
interval `sample` used — 1 ms by default). Each thread becomes a root named after
it, so the render thread, the compute pool and the tile workers stay apart.

    # the client's own perf run, or a windowed one you are turning the camera in
    cargo run -p cartopolis --features perf -- --perf --only earth --perf-frames 600 &
    sample $(pgrep -f 'cartopolis --perf') 5 -f /tmp/cartopolis.sample.txt
    rustfilt -i /tmp/cartopolis.sample.txt -o /tmp/cartopolis.sample.txt   # optional: Rust v0 names
    python3 tools/sample_to_callgrind.py /tmp/cartopolis.sample.txt > /tmp/callgrind.out.cartopolis
    qcachegrind /tmp/callgrind.out.cartopolis

`rustfilt` (`cargo install rustfilt`) demangles the `_RNv…` symbols `sample` leaves
as they are; without it every Rust frame reads as its mangled name. Waits are kept
— a thread parked in `__psynch_cvwait` is a real answer to "where did the frame
go" — so read a thread's inclusive cost against its sample total, not as CPU.
`docs/notes/render-performance-survey.md` has what this found the first time.
"""

import re
import sys
from collections import defaultdict

NODE = re.compile(r"^([\s+!:|]*)(\d+) (.*)$")


def clean(name: str) -> str:
    # `foo  (in libbar.dylib) + 12  [0x1234]` → `foo`; the library goes in `ob=`.
    name = re.sub(r"\s+\[0x[0-9a-f]+(?:,0x[0-9a-f]+)*\]\s*$", "", name)
    name = re.sub(r"\s+\+\s+\d+\s*$", "", name)
    return name.strip()


def split(raw: str):
    m = re.match(r"^(.*?)\s+\(in ([^)]+)\)", raw)
    if m:
        return clean(m.group(1)), m.group(2)
    return clean(raw), "?"


def parse(lines):
    try:
        start = next(i for i, l in enumerate(lines) if l.startswith("Call graph:"))
    except StopIteration:
        sys.exit("no 'Call graph:' section — is this a `sample` report?")
    end = next(
        (i for i, l in enumerate(lines) if l.startswith("Total number in stack")),
        len(lines),
    )
    nodes = []
    for l in lines[start + 1 : end]:
        m = NODE.match(l)
        if m:
            nodes.append((len(m.group(1)), int(m.group(2)), m.group(3)))
    return nodes


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    nodes = parse(open(sys.argv[1], errors="replace").read().split("\n"))

    self_cost = defaultdict(int)  # (obj, fn) -> samples in no deeper frame
    calls = defaultdict(lambda: [0, 0])  # (caller, callee) -> [calls, inclusive]
    stack = []  # [indent, key, count, samples in direct children]

    def close(frame):
        self_cost[frame[1]] += max(0, frame[2] - frame[3])

    for indent, count, raw in nodes:
        while stack and stack[-1][0] >= indent:
            close(stack.pop())
        if raw.startswith("Thread_"):
            name = re.sub(r"^Thread_\d+\s*:?\s*", "", raw).strip() or "thread"
            key = ("[thread]", f"Thread: {name}")
        else:
            fn, obj = split(raw)
            key = (obj, fn)
        if stack:
            stack[-1][3] += count
            edge = calls[(stack[-1][1], key)]
            edge[0] += 1
            edge[1] += count
        stack.append([indent, key, count, 0])
    while stack:
        close(stack.pop())

    out = sys.stdout
    out.write("# callgrind format\nversion: 1\ncreator: tools/sample_to_callgrind.py\n")
    out.write("events: Samples\n\n")
    callees = defaultdict(list)
    for (caller, callee), (n, incl) in calls.items():
        callees[caller].append((callee, n, incl))
    for key in set(self_cost) | set(callees):
        obj, fn = key
        out.write(f"ob={obj}\nfn={fn}\n0 {self_cost.get(key, 0)}\n")
        for (cobj, cfn), n, incl in callees.get(key, ()):
            out.write(f"cob={cobj}\ncfn={cfn}\ncalls={n} 0\n0 {incl}\n")
        out.write("\n")


if __name__ == "__main__":
    main()
