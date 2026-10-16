"""Build corpus.jsonl for the recall yardstick.

Run from anywhere with `python -I build.py`. The output is deterministic for
the same inputs: files are read in sorted order and sessions are sampled by
a fixed rule, never at random.

Sources:
  doc       every tracked markdown file in the repository
  note      the lore notes of this project, from %APPDATA%/Horadric/lore
  dialogue  Claude Code transcripts of this project, human prompts and
            agent text only

The set is only ever added to. Rebuilding after the sources have grown can
add chunks; it must not change or drop the ids that questions.jsonl names.
To extend, run with `--append`, which keeps every existing line and adds
only chunks whose ids are new.
"""

import glob
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
OUT = os.path.join(HERE, "corpus.jsonl")

LORE = os.path.join(os.environ.get("APPDATA", ""), "Horadric", "lore", "horadric.dev-990a2c84.json")
NOTE_PROJECT = "horadric.dev"
TRANSCRIPTS = os.path.join(
    os.path.expanduser("~"), ".claude", "projects", "C--Users-morte-Documents-Github-horadric-dev*", "*.jsonl"
)

MIN, MAX = 400, 1000

# Warriv's wake rounds are many and much alike. Keep one in this many so
# they do not crowd out the sessions where a human asked for something.
WOKE_EVERY = 4
# Of the other sessions, drop one in this many, in time order, so dialogue
# stays near half the corpus while still spanning every week.
SESSION_SKIP = 3

SECRET = re.compile(
    r"(?<![A-Za-z])sk-[A-Za-z0-9_-]{8,}|ghp_|gho_|github_pat_|xox[abp]-|AKIA[0-9A-Z]{12}|"
    r"[Bb]earer\s+[A-Za-z0-9._-]{12,}|[Aa]uthorization:\s*\S{12,}|"
    r"(api[_-]?key|token|secret|password|passwd)\s*[:=]\s*['\"]?[A-Za-z0-9._/+-]{8,}|"
    r"\b[0-9a-fA-F]{32,}\b|-----BEGIN|"
    # base64: long, and mixing digits with both cases, which a path or a word never does
    r"(?=[A-Za-z0-9+/]*\d)(?=[A-Za-z0-9+/]*[a-z])(?=[A-Za-z0-9+/]*[A-Z])[A-Za-z0-9+/]{40,}|"
    # an IP address other than this machine's loopback
    r"\b(?!127\.0\.0\.1\b)(?!0\.0\.0\.0\b)\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b"
)
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
PHONE = re.compile(r"\+\d{1,3}[ -]?\d{2}[ -]?\d{2}[ -]?\d{2}[ -]?\d{2}|\b\d{2} \d{2} \d{2} \d{2}\b")
# Matters that are not about building Horadric: the human's day job, family,
# health, money. A chunk that touches one is left out whole.
PERSONAL = re.compile(
    r"\b(optipeople|opticore|opticloud|wife|husband|girlfriend|boyfriend|my (son|daughter|kids?|family|mom|dad|mother|father)|"
    r"doctor|hospital|sick leave|salary|bank account|credit card|cpr|ssn|social security|"
    r"vacation|holiday|birthday|morten|prads\w*|mp-opti|danish|denmark)\b|"
    # a social profile or post names a person
    r"\b(x|twitter|linkedin|facebook|instagram)\.com/\w+",
    re.I,
)


def private(text):
    return bool(SECRET.search(text) or EMAIL.search(text) or PHONE.search(text) or PERSONAL.search(text))


def split_long(text, limit=MAX):
    """Split one paragraph that is over the limit, on lines, then sentences, then spaces."""
    if len(text) <= limit:
        return [text]
    for sep in ("\n", ". ", " "):
        parts = text.split(sep)
        if len(parts) > 1:
            out, cur = [], ""
            for i, p in enumerate(parts):
                piece = p + (sep if i < len(parts) - 1 else "")
                if cur and len(cur) + len(piece) > limit:
                    out.append(cur.rstrip())
                    cur = ""
                cur += piece
            if cur.strip():
                out.append(cur.rstrip())
            result = []
            for o in out:
                result.extend(split_long(o, limit) if len(o) > limit and sep != " " else [o])
            return result
    return [text]


def pack(units, heading_break=False):
    """Pack paragraphs or messages into chunks of MIN to MAX characters."""
    chunks, cur = [], ""
    for u in units:
        for piece in split_long(u):
            starts_section = heading_break and piece.startswith("#")
            if cur and (len(cur) + 2 + len(piece) > MAX or (starts_section and len(cur) >= MIN)):
                chunks.append(cur)
                cur = ""
            cur = cur + "\n\n" + piece if cur else piece
    if cur:
        if chunks and len(cur) < MIN // 2 and len(chunks[-1]) + 2 + len(cur) <= MAX + MIN // 2:
            chunks[-1] += "\n\n" + cur
        else:
            chunks.append(cur)
    return [c for c in chunks if len(c) >= 80]


def docs():
    files = subprocess.run(
        ["git", "-C", REPO, "ls-files", "*.md"], capture_output=True, text=True, check=True
    ).stdout.split()
    for rel in sorted(files):
        if rel.startswith("crates/horadric-memory/yardstick/"):
            continue
        with open(os.path.join(REPO, rel), encoding="utf-8") as f:
            text = f.read().replace("\r\n", "\n")
        paras = [p.strip() for p in re.split(r"\n\s*\n", text) if p.strip()]
        for i, c in enumerate(pack(paras, heading_break=True), 1):
            yield {"id": f"doc:{rel}#{i}", "kind": "doc", "text": c}


def notes():
    if not os.path.exists(LORE):
        return
    with open(LORE, encoding="utf-8") as f:
        data = json.load(f)
    for n in sorted(data["notes"], key=lambda n: n["id"]):
        text = n["text"].strip()
        paths = [x["path"] if isinstance(x, dict) else x for x in n.get("files", [])]
        if paths:
            text += "\nFiles: " + ", ".join(paths)
        yield {"id": f"note:{NOTE_PROJECT}#{n['id']}", "kind": "note", "text": text}


NOISE_PREFIX = (
    "<", "Horadric woke", "Caveat:", "[Request interrupted", "Base directory for this skill",
    "Another Claude", "Continue:", "This session is being continued",
)


def human_text(entry):
    if entry.get("isMeta"):
        return None
    m = entry.get("message", {}).get("content")
    if isinstance(m, str):
        texts = [m]
    elif isinstance(m, list):
        if any(isinstance(b, dict) and b.get("type") == "tool_result" for b in m):
            return None
        texts = [b.get("text", "") for b in m if isinstance(b, dict) and b.get("type") == "text"]
    else:
        return None
    text = "\n\n".join(t for t in texts if t.strip())
    text = re.sub(r"\[Image[^\]]*\]", "", text)
    text = re.sub(r"<system-reminder>.*?</system-reminder>", "", text, flags=re.S).strip()
    if not text or text.startswith(NOISE_PREFIX) or text.lower().startswith("reply with"):
        return None
    return text


def agent_text(entry):
    m = entry.get("message", {}).get("content")
    if not isinstance(m, list):
        return None
    texts = [b.get("text", "").strip() for b in m if isinstance(b, dict) and b.get("type") == "text"]
    text = "\n\n".join(t for t in texts if t)
    if not text or text in ("No response requested.",) or text.startswith("API Error"):
        return None
    return text


def session(path):
    """The dialogue of one transcript, its start time, and whether Warriv was woken in it."""
    msgs, start, woke = [], None, False
    with open(path, encoding="utf-8") as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if e.get("isSidechain") or e.get("type") not in ("user", "assistant"):
                continue
            start = start or e.get("timestamp")
            if e["type"] == "user":
                m = e.get("message", {}).get("content")
                raw = m if isinstance(m, str) else " ".join(
                    b.get("text", "") for b in (m or []) if isinstance(b, dict)
                )
                if raw.startswith("Horadric woke"):
                    woke = True
                t = human_text(e)
                if t:
                    msgs.append("Human: " + t)
            else:
                t = agent_text(e)
                if t:
                    msgs.append("Agent: " + t)
    return start or "", woke, msgs


def dialogue():
    sessions = []
    for p in sorted(glob.glob(TRANSCRIPTS)):
        start, woke, msgs = session(p)
        if msgs and any(m.startswith("Human: ") for m in msgs) or (woke and msgs):
            sessions.append((start, os.path.basename(p)[:8], woke, msgs))
    sessions.sort()
    woke_seen = other_seen = 0
    for start, short, woke, msgs in sessions:
        if woke:
            woke_seen += 1
            if woke_seen % WOKE_EVERY:
                continue
        else:
            other_seen += 1
            if other_seen % SESSION_SKIP == 0:
                continue
        # A message over the limit is split on paragraphs, each piece keeping
        # its speaker so a chunk never starts with words nobody owns.
        units = []
        for m in msgs:
            who, body = m.split(": ", 1)
            paras = [p.strip() for p in re.split(r"\n\s*\n", body) if p.strip()]
            pieces = pack(paras) if len(m) > MAX else [body]
            units.extend(f"{who}: {p}" for p in pieces)
        for i, c in enumerate(pack(units), 1):
            yield {"id": f"dialogue:{short}#{i}", "kind": "dialogue", "text": c}


def main():
    append = "--append" in sys.argv
    kept, seen = [], set()
    if append and os.path.exists(OUT):
        with open(OUT, encoding="utf-8") as f:
            for line in f:
                row = json.loads(line)
                kept.append(row)
                seen.add(row["id"])
    dropped = 0
    for gen in (docs, notes, dialogue):
        for row in gen():
            if row["id"] in seen:
                continue
            if private(row["text"]):
                dropped += 1
                continue
            seen.add(row["id"])
            kept.append(row)
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        for row in kept:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
    counts = {}
    for row in kept:
        counts[row["kind"]] = counts.get(row["kind"], 0) + 1
    print(counts, "dropped as private:", dropped)


if __name__ == "__main__":
    main()
