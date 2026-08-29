#!/usr/bin/env python3
"""Build and script the world from a shell, through the simulator's authoring API.

The in-world tools — the create dialog, the gizmo, the script editor — are the
right way to make one object while looking at it, and they are unusable for
anything else: they need a person, a GPU and a signed-in viewer. This talks to
`/api/admin/prims` instead (`crates/simulator/src/authoring.rs`), so a world can
be built by a script, kept in a file, reviewed in a diff, and set up by an agent
with no screen.

Everything here is one admin token and JSON. Nothing in this file is required to
use the API — it is a convenience over `curl`, and the `--print` flag exists so
you can see the request it would have sent.

    export CARTO_SIM=https://sim.cartopolis.org
    export CARTO_ADMIN_USER=dev CARTO_ADMIN_PASSWORD=...

    tools/author.py regions
    tools/author.py prims
    tools/author.py show 42
    tools/author.py create --lat 53.2194 --lng 6.5665 --alt 45 \\
        --shape cylinder --name Firebird --script my-script.lua
    tools/author.py patch 42 --color 1,0.4,0.1
    tools/author.py script 42 my-script.lua
    tools/author.py delete 42

    tools/author.py content         # what the server can install
    tools/author.py install firebird

Two things worth knowing before you use it in anger.

**A script's compile errors do not come back in the HTTP response.** The host
compiles on its own thread and reports through `ScriptOutput` on the *wire*, so
the answer to "did my script work?" is the script console in a connected viewer,
not a 200 here. `store_script` in the simulator says the same thing in its reply.

**Rotations are degrees**, as they are in the edit dialog and in `vfSetRot`, and
are converted at the API boundary. The wire is radians and nothing outside the
server should ever see them.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

DEFAULT_SIM = "http://127.0.0.1:8080"


class ApiError(RuntimeError):
    pass


def request(method: str, url: str, token: str | None, body: object | None) -> object:
    """One JSON call. Errors carry the server's own message — the API answers with
    `{"error": "..."}` for everything it refuses, and that sentence is usually the
    whole diagnosis (a shape that isn't a shape, a region that is full)."""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req) as resp:
            raw = resp.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")
        try:
            detail = json.loads(detail).get("error", detail)
        except json.JSONDecodeError:
            pass
        raise ApiError(f"{method} {url} → {e.code}: {detail}") from None
    except urllib.error.URLError as e:
        raise ApiError(f"cannot reach {url}: {e.reason}") from None


def login(base: str) -> str:
    user = os.environ.get("CARTO_ADMIN_USER")
    password = os.environ.get("CARTO_ADMIN_PASSWORD")
    if not user or not password:
        raise ApiError(
            "set CARTO_ADMIN_USER and CARTO_ADMIN_PASSWORD (the same account you "
            "would sign in to the admin GUI with; `simulator --create-admin <user>` "
            "makes one)"
        )
    answer = request(
        "POST",
        f"{base}/api/admin/login",
        None,
        {"username": user, "password": password},
    )
    return answer["token"]


def parse_triple(text: str, what: str) -> list[float]:
    parts = [p for p in text.replace(" ", "").split(",") if p]
    if len(parts) != 3:
        raise ApiError(f"{what} wants three comma-separated numbers, got {text!r}")
    return [float(p) for p in parts]


def spec_from_args(args: argparse.Namespace) -> dict:
    """The request body, with only the fields the caller actually named.

    Absent is not the same as null here, and that is the API's contract rather
    than this script's convenience: a field left out is left alone, while an
    explicit `null` takes a texture or an emitter *off*. Sending everything with
    a default would make `patch --color` a reset of the whole prim."""
    spec: dict = {}
    if args.region is not None:
        spec["region_id"] = args.region
    if args.name is not None:
        spec["name"] = args.name
    if args.shape is not None:
        spec["shape"] = args.shape
    if args.lat is not None:
        spec["latitude"] = args.lat
    if args.lng is not None:
        spec["longitude"] = args.lng
    if args.alt is not None:
        spec["altitude"] = args.alt
    if args.rotation is not None:
        spec["rotation"] = parse_triple(args.rotation, "--rotation")
    if args.scale is not None:
        spec["scale"] = parse_triple(args.scale, "--scale")
    if args.color is not None:
        spec["color"] = parse_triple(args.color, "--color")
    if args.particles is not None:
        # `--particles none` is the explicit null: it takes the emitter off,
        # which is a different request from not mentioning it.
        spec["particles"] = (
            None
            if args.particles.strip().lower() == "none"
            else json.loads(read_maybe_file(args.particles))
        )
    if args.script is not None:
        spec["script"] = read_maybe_file(args.script)
    return spec


def read_maybe_file(value: str) -> str:
    """A path if it is one, otherwise the literal text. Lets `--script` take
    either a path to a `.lua` file or a one-liner in quotes, which is the
    difference between trying something out and installing it."""
    if os.path.exists(value):
        with open(value, encoding="utf-8") as f:
            return f.read()
    return value


def emit(value: object) -> None:
    print(json.dumps(value, indent=2))


# ---------------------------------------------------------------------------


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument(
        "--sim",
        default=os.environ.get("CARTO_SIM", DEFAULT_SIM),
        help=f"base URL of the simulator's HTTP listener (default {DEFAULT_SIM})",
    )
    p.add_argument(
        "--print",
        dest="dry_run",
        action="store_true",
        help="show the request instead of sending it",
    )
    sub = p.add_subparsers(dest="command", required=True)

    sub.add_parser("regions", help="list regions and their prim/script budgets")

    lp = sub.add_parser("prims", help="list prims")
    lp.add_argument("--region", type=int)

    sp = sub.add_parser("show", help="one prim, with its script source")
    sp.add_argument("id", type=int)

    def add_spec_flags(parser: argparse.ArgumentParser) -> None:
        parser.add_argument("--region", type=int)
        parser.add_argument("--name")
        parser.add_argument(
            "--shape", choices=["box", "sphere", "cylinder", "cone", "torus"]
        )
        parser.add_argument("--lat", type=float)
        parser.add_argument("--lng", type=float)
        parser.add_argument("--alt", type=float, help="metres above the ground")
        parser.add_argument("--rotation", help="degrees, 'x,y,z'")
        parser.add_argument("--scale", help="metres, 'x,y,z'")
        parser.add_argument("--color", help="linear 0-1, 'r,g,b'")
        parser.add_argument(
            "--particles",
            help="a ParticleEffect as JSON, a path to one, or 'none' to remove it",
        )
        parser.add_argument("--script", help="Luau source, or a path to it")

    cp = sub.add_parser("create", help="make a prim")
    add_spec_flags(cp)

    pp = sub.add_parser("patch", help="change a prim; unnamed fields are left alone")
    pp.add_argument("id", type=int)
    add_spec_flags(pp)

    scp = sub.add_parser("script", help="store or (with an empty file) remove a script")
    scp.add_argument("id", type=int)
    scp.add_argument("source", help="Luau source, or a path to it")

    dp = sub.add_parser("delete", help="remove a prim, its script and its emitter")
    dp.add_argument("id", type=int)

    sub.add_parser("content", help="list the content the server can install")

    ip = sub.add_parser(
        "install",
        help="install a named piece of world the server already carries",
    )
    ip.add_argument("name", help="e.g. firebird")

    args = p.parse_args()
    base = args.sim.rstrip("/")

    try:
        # `--print` never signs in: showing the request is meant to work with no
        # credentials and no server, which is most of what makes it useful for
        # writing one of these by hand.
        token = None if args.dry_run else login(base)

        if args.command == "regions":
            emit(request("GET", f"{base}/api/admin/regions", token, None))
        elif args.command == "prims":
            query = f"?region_id={args.region}" if args.region else ""
            emit(request("GET", f"{base}/api/admin/prims{query}", token, None))
        elif args.command == "show":
            emit(request("GET", f"{base}/api/admin/prims/{args.id}", token, None))
        elif args.command == "create":
            spec = spec_from_args(args)
            if args.dry_run:
                emit({"POST": "/api/admin/prims", "body": spec})
            else:
                emit(request("POST", f"{base}/api/admin/prims", token, spec))
        elif args.command == "patch":
            spec = spec_from_args(args)
            if args.dry_run:
                emit({"PATCH": f"/api/admin/prims/{args.id}", "body": spec})
            else:
                emit(request("PATCH", f"{base}/api/admin/prims/{args.id}", token, spec))
        elif args.command == "script":
            body = {"source": read_maybe_file(args.source)}
            if args.dry_run:
                emit({"PUT": f"/api/admin/prims/{args.id}/script", "body": body})
            else:
                emit(
                    request(
                        "PUT", f"{base}/api/admin/prims/{args.id}/script", token, body
                    )
                )
        elif args.command == "delete":
            if args.dry_run:
                emit({"DELETE": f"/api/admin/prims/{args.id}"})
            else:
                request("DELETE", f"{base}/api/admin/prims/{args.id}", token, None)
                print(f"prim {args.id} deleted")
        elif args.command == "content":
            emit(request("GET", f"{base}/api/admin/content", token, None))
        elif args.command == "install":
            # Nothing about *what* gets installed is decided here. The prims,
            # their placement and their scripts live in the simulator
            # (`crates/simulator/src/content.rs`, scripts embedded at compile
            # time), because a deployed server has no repository beside it — so
            # content kept in the tree is content a running server cannot install.
            url = f"{base}/api/admin/content/{args.name}"
            if args.dry_run:
                emit({"POST": f"/api/admin/content/{args.name}"})
            else:
                emit(request("POST", url, token, None))
    except ApiError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
