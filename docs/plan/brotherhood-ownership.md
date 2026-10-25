# Who owns a session, and who is out: the specification

This is what every brother computes about the brotherhood's sessions and
its cast outs, written so each property can be checked. The code follows
it: `crates/horadric-core/src/said.rs` holds the pure functions,
`mesh.rs` keeps what a brother holds and signs this machine's own word,
and `crates/horadric-core/src/brotherhood_sim.rs` checks the
invariants below against an adversary. Where this and the code disagree,
one of them is a bug.

Four reviews in a row found holes in rules that were patched one finding
at a time, and each round of patches opened new ones. The rules here are
built so the invariants follow from their shape, not from case by case
care:

- Whose a session is depends on the words held on it and on nothing
  else: no entry, no clock, no arrival order. Every change of owner, and
  every change of who may take a session over, is a word in that
  session's own chain.
- Who is out is computed from every cast out word held at once, by one
  function of that set, never by which word came first.
- What one brother can make another hold is cut to a fixed size by rules
  that keep the same words whatever order they came in.

## The parties

- A **brother** is a machine with a relay id and an ECDSA P-256 key pair
  (the ECDH pair it joined with). A brother holds a key for each brother
  it joined or was introduced to (`brothers.json`), and remembers the
  key it held before for a brother whose key was replaced or cast out
  (`Store::cast`).
- **Honest** brothers run this code. A **stolen** brother signs whatever
  its thief likes with its own key; it cannot sign as another. The
  **relay** passes frames between brothers: it can drop, delay, reorder,
  duplicate and replay them, and read the routing, but cannot open what
  two brothers seal for each other, nor sign.
- A **human** sits at a PC. A Linux node has none.

## What a brother holds

`roster.json` holds:

- `said`: the signed words below, each checked once against the signer's
  key when it came.
- For each session it follows (keyed `<origin>/<id>`): its owner, the
  number the owner holds it under (which only a change of owner raises,
  never a change of keepers), the keepers that owner names, and whether
  and how it ended. All of it is `resolve` of the words held on it, cached.
- `outcast` and `contested`: `standing` of the cast out words held,
  cached.
- Notes for itself alone, never served and never consulted by `resolve`:
  which brothers showed it they heard each ending (`acks`), when it first
  saw each ending and each orphan (local clock), the sessions it let go
  and when, the copies told to its human, the contests put to its human,
  and the handoffs it signed that have not landed yet (`pending`).
- Each brother's own signed entry (name, link, keepers, the switch for
  taking over by itself, whom it lets only watch, whom it let in). An
  entry decides nothing about owners. It says whose turn it is among
  keepers, whether a keeper takes over by itself, and whom an entry
  introduces.

## The words

Every word is

```
{ n, by, under, at, kind, ...fields, sig }
```

`by` is the signer's relay id, `under` the public key it signed under
(b64url), `at` the signer's clock (seconds, shown to humans, never
compared with another clock), `sig` ECDSA P-256 over SHA-256, `r || s`.

The signature covers `"horadric said v4"`, then each part led by its
length as a 4 byte big endian number: the kind, `by`, `under`, `n` and `at`
(8 bytes big endian each), then for a session `origin` and `session`,
then the kind's own fields in the order the table gives, a list as a 4
byte count and then each item led by its length.

| kind | fields | says |
|---|---|---|
| `owns` | `origin`, `session`, `keepers`, `v` | `by` runs the session under `n` and names `keepers` (in order) as who may take it over from it at `n`, the `v`th time it names them at `n` |
| `hands` | `origin`, `session`, `to` | `by`, the owner at `n` - 1, hands it to `to` |
| `recalls` | `origin`, `session`, `hand` | `by` takes back its `hands` at `n` whose `sig` is `hand`: it did not land |
| `takes` | `origin`, `session`, `from`, `keepers` | `by`, a keeper of `from`, took it over from `from`'s `n` - 1, and names its own `keepers` |
| `ended` | `origin`, `session` | `by`, the owner at `n`, ended it for good |
| `cast_out` | `id`, `key` | `by`'s human cast out brother `id`, whose key was `key`; `n` is 0 |

A word is **taken** by a brother when:

- `under` is the key it holds for `by`, or the key it held for `by` before
  (a key cast out or replaced). A key named only in some word is never
  taken as anyone's: a thief cannot make its own key pass for another
  brother's by casting it out.
- the signature verifies under `under`;
- it is within bounds: every id and key at most 256 bytes, at most 16
  keepers, `cast_out`'s `key` a P-256 point.

A word under a key that is out is taken like any other. What it counts
for is `resolve`'s and `standing`'s to say, so two brothers that heard a
cast out at different moments still hold the same words.

`owns` with `v` 0 is the start of a session by its creator (its genesis),
the receiver's acknowledgement of a handoff, or, with a taker, nothing
more than its first `takes`. A higher `v` at the same `n` by the same
owner changes its keepers: the owner signs one whenever its keepers, or
whom it lets only watch, change, for every session it owns, so a keeper
it dropped or switched to view only is no keeper of those sessions from
then on.

## `standing`: who is out

Input: every `cast_out` word held. Output: the keys **out** and the keys
**contested**.

1. Each word is an attack from its `under` key (the signer's) on the key
   it names. A word naming its own signer's key is ignored. Of one
   signer's words, only the 32 naming the lowest keys count.
2. Two keys that attack each other are a **contest**. A key in contests
   with more keys than each of its rivals has is out (a thief that cast
   every brother out, answered by two of them, loses). Repeated until no
   key loses so.
3. A key still in a contest counts only against its rivals: a thief
   answered by one brother casts out nobody else meanwhile.
4. The grounded labelling of what is left: a key all of whose attackers
   are out is **in** (a key nobody attacks is in), a key attacked by a
   key that is in is **out**, until nothing changes.
5. A key out leaves its contests, so its rivals' other attacks count
   again: steps 2 to 4 are worked out again until nothing more goes out.
6. Every key neither in nor out is **contested**.

So:

- A cast out by a brother that is out casts nobody out, whenever it was
  signed.
- Two brothers that cast each other out, with nobody else deciding, are
  both contested: neither acts on any brother until a human decides. A
  human decides by casting one of them out from another brother: if that
  brother is in and was never cast out by the one it casts out, the one
  it casts is out; if the thief had cast it out too, the thief now has
  more contests than any of its rivals and is out.
- A brother cast out by a key that is in is out whatever it signs before
  or after.
- Whom a brother lets only watch it decides what that brother may do to
  it, not whether that brother's cast outs count: who is out must be the
  same on every brother (I1), and one brother's switch cannot be.

Out and contested keys both lose control everywhere: no brother lets a
link under one act on it (`Trust::control`, `Standing::contested`), and
no owner names one as a keeper in its next keeper word. An out key's
`takes` do not count (below), and it is dropped: its links close and its
tiles go. A contested key keeps its links, so it hears how the contest
ends, and the takeovers it made stand: which of the two is the thief is
what the contest has not decided.

## `resolve`: whose a session is

Input: the session's creator `origin`, every word held on it, and which
keys are out. Output: its owner and number, the owner's keepers and
how it came to own it, its ending if it ended, every
brother that owned it along the chain, the words that can matter (the
only ones kept), and the brothers that ran a copy whose word did not
stand.

1. **Genesis.** The chain starts at the creator's own `owns` with the
   lowest `n`, at most 1,000,000. Nobody else starts it.
2. **Keepers at a number.** The owner's keepers at its number `n` are
   those named by its words at `n` with the highest `v`: an `owns` by
   the owner at `n`, or the `takes` that made it the owner (`v` 0). An
   owner reached by a handoff it never acknowledged names none.
3. **Steps.** From owner `O` at `n`, the candidates at `n` + 1 are:
   - each `hands` by `O` to another brother that no `recalls` by `O` at
     `n` + 1 names: **acknowledged** if its receiver signed an `owns` at
     `n` + 1, rank 0, else rank 3;
   - each `takes` by a keeper `k` of `O` at `n`, from `O`, under a key
     that is in: rank 1, ordered by `k`'s place among the keepers.
   The best candidate (lowest rank, then place, then the receiver's id,
   then the word itself) makes the next owner. Ties between two words
   that make the same brother owner are no rivalry.
4. **Ending.** `O`'s `ended` at its own `n` ends the session there,
   unless an acknowledged handoff left `n`: then the receiver runs it on.
   This holds at the last number reachable too: a session that moved as
   often as allowed can still end.
5. **Length.** At most 256 steps are followed.
6. **Takeovers by a key that is out.** A `takes` under a key that is out
   counts only when the chain moves on from its taker, by a handoff
   acknowledged under a key that is not out. Otherwise it is set
   aside and the chain is worked out again without it. So a brother cast
   out cannot take a session from a live owner, whenever it signed, and
   a takeover it made before it was cast out and then handed on to a
   brother that ran it stands.
7. **What is kept.** The genesis; at each number the owner's keeper word
   with the highest `v`; at each step the best two candidates of each
   rank (with the `owns` acknowledging each) and every recall by the
   owner before it, eight at most (the lowest); the ending. Nothing else
   is kept nor passed on.
8. **Copies.** A brother made owner by a candidate that lost, at rank 0
   or 1, other than the brother that won, ran a copy that does not stand.

## Holding and dropping (`believe`, `prune`)

- A brother takes in new words, then works out `standing` and every
  session the new words speak of, and keeps only what `resolve` kept.
- A session whose creator's genesis is not held yet is not held: its
  words wait in no queue, they come again with the next roster.
- **Bounds per creator.** Of the sessions one creator started that this
  machine holds (its own excepted), it keeps the 128 live ones with the
  lowest keys, the 1024 ended ones with the lowest keys, and no more
  than 16,384 words for all of them, dropping whole sessions from the
  highest key down. Which sessions are kept depends only on the words
  held, never on their order.
- A creator takes no word on a session it created that it does not hold:
  it knows its own sessions, and one it let go stays gone.
- **Pruning.** An ended session's owner and words go once every brother
  that may hold a copy of it (each brother that owned it along the chain,
  and each keeper any of them named) showed it heard the ending, or
  ninety days after this machine first saw the ending; and once every
  brother in good standing showed it heard, or thirty days after this
  machine first saw it. Days count on this machine's clock from when it
  first saw the ending, never from the word's `at`. A session whose owner
  is out, live or ended, goes thirty days after this machine first saw it
  so, unless a keeper took it over meanwhile. A session let go is
  refused, whatever anyone says of it, for ninety days, as long as any
  brother may still hold it to pass on.
- A session the brother that owns it here lists as let go (`let_go`),
  in a roster over its own link, is let go here too: only an owner that
  ended it lets it go, and so a brother that slept through the ending
  does not keep it as live.
- Pruning, the bounds and the notes are this machine's own. Two brothers
  that hold the same words agree; pruning only ever drops words, and
  what it drops was over.

## This machine's own word

- **Starting** a session signs its genesis, naming this machine's keepers
  (its book's, less any it lets only watch and any out or contested).
- **Handing** session `s` at `n` to `B` signs `hands` at `n` + 1, with an
  `at` past any handoff of `s` it signed before, keeps it in `pending`,
  and sends it in the bundle with every word held on `s`. Only when `B`
  says it took it is the same word taken in here. A handoff that fails
  is recalled by a `recalls` naming that very word; the next try is a new
  word and counts. A `pending` handoff left from before this process
  started (the process that drove it is gone) is recalled.
- **Receiving** checks, before the bundle lands, that the handoff was not
  recalled and that no other brother acknowledged a handoff at that
  number, then signs `owns` at `n` + 1 with its own keepers.
- **Changing keepers** (the book, a view only switch, a cast out) signs a
  new `owns` with the next `v` for every session it owns that names
  others.
- **Taking over** from `O` at `n` signs `takes` at `n` + 1, only while
  `O` still owns the session under the number the snapshot was cut at,
  names this machine among its keepers at that number, never ended it, and no brother linked now lists it.
- **Ending** signs `ended` at its number.
- **Casting out** signs `cast_out`, and a human seeing a contest is asked
  once whether to cast out one of the two.

## The invariants

Each is checked by the simulator after every step, and each finding of
the four reviews has a scenario of its own.

- **I1, agreement.** Two honest brothers that hold the same keys and the
  same words agree on every session's owner, number, keepers and
  ending, and on the out and contested keys, whatever order the words
  came in. Holds because `resolve` and `standing` are functions of the
  word set, and what is kept is chosen by them alone.
- **I2, no fencing by those without the right.** A live session run by
  an honest owner never moves away from it by anything signed or replayed
  by a brother that is not its keeper at its number: one it never named,
  dropped, switched to view only, or that was contested or out when it
  last named its keepers; nor by a brother out, whenever it signed; nor by
  the relay. Holds because a step needs the owner's own `hands`, or a
  `takes` by a keeper its newest keeper word names under a key that is not
  out, owners name no contested or out key, and words are signed. The one
  way an honest owner's session moves without its word is a takeover by a
  keeper it names, which is what naming a keeper allows.
- **I3, one owner.** Every session held has exactly one owner, and no two
  honest brothers both run it acknowledged at the same time once they
  heard each other's words: the owner is one brother, and a brother whose
  copy lost is told. A session is never left with nobody able to run it
  by a recalled or failed handoff: a recall puts it back on its sender.
- **I4, cast out stays out.** Once a human on any honest brother that is
  in casts brother `X` out, `X` has no control on any brother once they
  hold that word, whatever `X` signed before or after: `X` is out, or
  contested when it cast that brother out too.
- **I5, bounded.** No brother can make an honest one hold more than the
  bounds above (sessions per creator, words per creator, words per
  session, cast outs per signer), nor send a frame past 1 MB: rosters go
  in parts of at most 256 KB, and no word is longer than a part.
- **I6, no honest dead end.** In honest use (many sessions, Warriv, failed
  handoffs retried, brothers asleep, upgrades) nothing gets stuck: a
  recalled handoff can be tried again to the same brother and lands, a
  session can always be ended, and every ended session is pruned in the
  end.

## What is not promised

- A thief that holds the key of a keeper may take that keeper's
  sessions over until it is cast out: that is what being a keeper means.
- An owner that changes its keepers while a keeper it drops is taking
  over decides that race for itself: the newest keeper word stands, so
  the takeover does not, and the taker's copy is told it ran twice.
- A brother that rejoined with a new key is followed under the old key
  only by brothers that held the old key. A brother that never held it
  cannot check those words and does not follow sessions started under it.
- A thief that casts out every brother before anyone casts it out puts
  every brother out, and each drops the others, until a human acts:
  casting the thief out from one machine makes the two contested and the
  rest in again, from two the thief is out. Brothers already dropped come
  back by joining again.
- A thief with an accomplice it let in can keep a brother's word against
  it from counting: the accomplice casts that brother out, and while the
  two are contested that brother's other cast outs count no more. A
  second human casting either of them out decides it.
- A brother dropped here because its key was out comes back when its
  human asks to join again.

## The relay and tokens

- A signed in machine says `X-Horadric-Refresh: 1` and sends a fresh
  access token on the socket before the old one expires. It keeps the
  old expiry until the relay answers `{"token":"ok","exp"}`; on
  `"later"` it asks again shortly; on `"refused"` it gets a new token
  and tries again.
- The relay closes a signed in socket two minutes past its token's
  expiry. Only machines whose id it first saw before 2026-10-26, when
  in band refresh shipped, may skip the header, and then only until
  2027-01-01: a token stolen now holds a socket minutes, not months.

## Upgrading

The words changed in 0.35 (`"horadric said v4"`). No released Horadric
ever held a roster (0.32 is the newest release; machines that ran 0.33
and 0.34 builds were development machines, and none of them holds a
`roster.json` today), so a `roster.json` from before is started anew:
its words and owners go, and each machine signs a genesis for every
session it runs. Brothers, keys, cast out keys and entries stay.
