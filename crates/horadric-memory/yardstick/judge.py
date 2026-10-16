"""Scores Horadrim against verbatim on the yardstick, with Claude.

The yardstick example writes, for every question, the hits recall
answered with, word for word and inscribed (`target/yardstick/
horadrim.jsonl`). For each question and each form this has a model answer
the question from the hits alone, then has another judge that answer
against the passages that answer it. It also counts the hits' tokens with
Claude's own tokenizer: the input tokens of the answering call less those
of the same call with no hits.

Run it with `python -I judge.py [rows]`. It calls `claude -p`, so it uses
the Claude Code login of whoever runs it: about 260 short calls, cents.
Results go to the console and beside the rows as `judged.jsonl`.
"""

import json
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ANSWERER = "claude-haiku-4-5-20251001"
JUDGE = "sonnet"
FORMS = ["verbatim", "inscribed", "session"]

ANSWER_SYSTEM = (
    "You answer questions about Horadric, a software project, from the "
    "recalled hits below and nothing else. Answer in one or two sentences. "
    "If the hits do not say, answer exactly: not in the hits.\n\n<hits>\n{hits}\n</hits>"
)
JUDGE_SYSTEM = (
    "You grade an answer to a question against reference passages that "
    "answer it. Reply YES if the answer gives the fact the references give "
    "in answer to the question (other wording is fine, extra detail is "
    "fine), NO if it is wrong, missing or says it is not in the hits. "
    "Reply with YES or NO only."
)


def claude(model, system, prompt, tries=5):
    """The reply and the input tokens of one call."""
    cmd = [
        "claude", "-p", "--output-format", "json", "--model", model,
        "--setting-sources", "", "--system-prompt", system, "--tools", "",
        "--no-session-persistence",
    ]
    for _ in range(tries):
        p = subprocess.run(cmd, input=prompt, capture_output=True, text=True, encoding="utf8")
        try:
            out = json.loads(p.stdout)
            u = out["usage"]
            n = u["input_tokens"] + u["cache_creation_input_tokens"] + u["cache_read_input_tokens"]
            if n > 0 and not out.get("is_error"):
                return out["result"].strip(), n
        except (ValueError, KeyError):
            pass
        time.sleep(3)
    raise RuntimeError(f"claude failed: {p.stdout[:300]} {p.stderr[:300]}")


def answer(row, form):
    text = row[form]
    reply, n = claude(ANSWERER, ANSWER_SYSTEM.format(hits=text), row["q"])
    refs = "\n\n".join(f"<reference>\n{r}\n</reference>" for r in row["reference"])
    verdict, _ = claude(
        JUDGE, JUDGE_SYSTEM, f"Question: {row['q']}\n\n{refs}\n\nAnswer: {reply}"
    )
    return {"answer": reply, "right": verdict.upper().startswith("YES"), "tokens": n}


def judge(row):
    _, empty = claude(ANSWERER, ANSWER_SYSTEM.format(hits=""), row["q"])
    out = {"q": row["q"], "from": row["from"], "found": row["found"], "hits": row["hits"]}
    for form in FORMS:
        a = answer(row, form)
        a["tokens"] -= empty
        out[form] = a
    return out


def main():
    here = Path(__file__).resolve().parent
    rows_path = Path(sys.argv[1]) if len(sys.argv) > 1 else here / "../../../target/yardstick/horadrim.jsonl"
    rows = [json.loads(l) for l in rows_path.read_text(encoding="utf8").splitlines() if l.strip()]
    with ThreadPoolExecutor(4) as ex:
        judged = list(ex.map(judge, rows))
    (rows_path.parent / "judged.jsonl").write_text(
        "".join(json.dumps(j) + "\n" for j in judged), encoding="utf8"
    )
    hits = sum(j["hits"] for j in judged)
    found = [j for j in judged if j["found"]]
    print(f"{len(judged)} questions, recall found an answer for {len(found)}")
    for form in FORMS:
        right = sum(j[form]["right"] for j in judged)
        right_found = sum(j[form]["right"] for j in found)
        tokens = sum(j[form]["tokens"] for j in judged)
        print(
            f"{form:<10} answered right {right}/{len(judged)} "
            f"({right_found}/{len(found)} where found), "
            f"{tokens / hits:.0f} tokens a hit, {tokens} in all"
        )
    for form in FORMS[1:]:
        lost = [j["q"] for j in judged if j["verbatim"]["right"] and not j[form]["right"]]
        won = [j["q"] for j in judged if j[form]["right"] and not j["verbatim"]["right"]]
        print(f"\n{form}: right verbatim only {len(lost)}, right {form} only {len(won)}")
        for q in lost:
            print(f"  lost: {q}")
        for q in won:
            print(f"  won:  {q}")


if __name__ == "__main__":
    main()
