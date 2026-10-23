#!/usr/bin/env python3
"""A viewer of a Linux node, as `ssh <host> horadric node --stdio` gives one.

`first`: hello, the list (the fake session posted to the port and the
shell started with `horadric new`), then the shell attached, typed into
and read back. `again`, after the node was restarted: the same shell,
attached again from its host, replays what it printed, and ends.
"""
import json
import os
import select
import struct
import subprocess
import sys
import time

PREAMBLE = b"\x00horadric-node 1\n"


class Link:
    def __init__(self, binary):
        self.p = subprocess.Popen(
            [binary, "node", "--stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
        )
        self.buf = b""
        self.started = False

    def send(self, msg):
        data = json.dumps(msg).encode()
        self.p.stdin.write(struct.pack(">I", len(data)) + data)
        self.p.stdin.flush()

    def _frame(self):
        if not self.started:
            at = self.buf.find(PREAMBLE)
            if at < 0:
                return None
            self.buf = self.buf[at + len(PREAMBLE):]
            self.started = True
        if len(self.buf) < 4:
            return None
        n = struct.unpack(">I", self.buf[:4])[0]
        if len(self.buf) < 4 + n:
            return None
        frame, self.buf = self.buf[4:4 + n], self.buf[4 + n:]
        return json.loads(frame)

    def until(self, what, pred, timeout=15):
        deadline = time.time() + timeout
        while True:
            msg = self._frame()
            if msg is not None:
                print("<-", json.dumps(msg)[:200])
                if pred(msg):
                    return msg
                continue
            left = deadline - time.time()
            if left <= 0:
                sys.exit(f"FAIL: no {what} within {timeout}s")
            ready, _, _ = select.select([self.p.stdout], [], [], left)
            if ready:
                chunk = os.read(self.p.stdout.fileno(), 65536)
                if not chunk:
                    sys.exit(f"FAIL: --stdio ended before {what}")
                self.buf += chunk

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=5)


def b64(s):
    import base64

    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def main():
    binary, mode = sys.argv[1], sys.argv[2]
    link = Link(binary)
    link.send({"t": "hello", "v": 3, "kind": "brother"})
    hello = link.until("hello", lambda m: m["t"] == "hello")
    if hello["v"] != 3 or not hello.get("brother") or hello.get("control") is not True:
        sys.exit(f"FAIL: hello {hello}")

    # A host attached again after a restart is listed under its id,
    # which `horadric new` made from the name.
    def is_shell(r):
        return r.get("hosted") and (r["name"] == "shell" or r["id"].startswith("shell-"))

    def listed(m):
        if m["t"] != "sessions":
            return False
        return any(r["name"] == "fake" for r in m["list"]) and any(map(is_shell, m["list"]))

    rows = link.until("a list with both sessions", listed)["list"]
    shell = next(r for r in rows if is_shell(r))
    fake = next(r for r in rows if r["name"] == "fake")
    if mode == "first" and fake["phase"] != "working":
        sys.exit(f"FAIL: the fake session is {fake['phase']}, not working")
    sid = shell["id"]

    link.send({"t": "attach", "id": sid, "cols": 100, "rows": 30})
    link.until("a size", lambda m: m["t"] == "size" and m["id"] == sid)
    seen = bytearray()

    def took(m):
        if m["t"] == "out" and m["id"] == sid:
            seen.extend(b64(m["b64"]))
            return True
        return False

    link.until("a replay", lambda m: took(m) and m.get("reset"))

    def shows(text):
        return lambda m: (took(m) or True) and text in seen

    if mode == "first":
        # Typed as one line; the shell prints the sum, never the words.
        link.send({"t": "input", "id": sid, "data": "echo SMOKE-$((6*7))\r"})
        link.until("the shell's answer", shows(b"SMOKE-42"))
    else:
        link.until("the replay of the first run", shows(b"SMOKE-42"))
        link.send({"t": "input", "id": sid, "data": "exit\r"})
        link.until(
            "the end of the shell",
            lambda m: m["t"] == "error" and m.get("id") == sid and m["text"] == "ended",
        )
    link.close()
    print(f"OK: {mode}")


main()
