# Who owns a session, and who is out: the specification

This is what every brother computes about the brotherhood: who is in
it, under which key, whose each session is, and who is out, written so
each property can be checked. The code follows it:
`crates/horadric-core/src/said.rs` holds the words and the pure
functions, `lineage.rs` the tree of who let whom in, `mesh.rs` keeps
what a brother holds and signs this machine's own word, `brothers.rs`
acts on it, and two simulations check the invariants below against an
adversary: `brotherhood_sim.rs` over the pure functions, and
`brotherhood_wired.rs` over the real state machine of each brother
(`Brothers`, its files, its links through a relay). Where this and the
code disagree, one of them is a bug.

Five reviews in a row found holes in rules that were patched one finding
at a time. The rules here are built so the invariants follow from their
shape, not from case by case care:

- Whose a session is depends on the words held on it and on nothing
  else: no entry, no clock, no arrival order. Every change of owner, and
  every change of who may take a session over, is a word in that
  session's own chain.
- Who is in the brotherhood, and under which key, is the tree of signed
  admissions, from the brotherhood's first two machines down. A key
  comes into it only by a word its admitter signed, and a brother's key
  changes only on a human's word on each brother.
- Who is out is computed from every cast out word held and that tree at
  once, by one function of the two, never by which word came first.
- Being out, contested or dropped is never stored: each brother works it
  out afresh from the words whenever they change, so what one word did,
  a later word can undo, everywhere alike.
- What one brother can make another hold is cut to a fixed size by rules
  that keep the same words whatever order they came in.

## The parties

- A **brother** is a machine with a relay id and an ECDSA P-256 key pair
  (the ECDH pair it joined with). It holds a key for each brother it
  joined or was introduced to (`brothers.json`), and the key it held
  before for a brother whose key a human replaced (`Store::cast`).
- **Honest** brothers run this code. A **stolen** brother signs whatever
  its thief likes with its own key, and its thief can make **fakes**:
  new keys it lets in itself, as many as it likes. It cannot sign under
  another brother's key. The **relay** passes frames between brothers:
  it can drop, delay, reorder, duplicate and replay them, read the
  routing, and say who is connected, but cannot open what two brothers
  seal for each other, nor sign.
- A **human** sits at a PC. A Linux node has none.

## What a brother holds

`roster.json` holds:

- `said`: the signed words below, each checked once against the signer's
  key when it came.
- For each session it follows (keyed `<origin>/<id>`): its owner, the
  number the owner holds it under (which only a change of owner raises,
  never a change of keepers), the keepers that owner names, and whether
  and how it ended. All of it is `resolve` of the words held on it, cached.
- `outcast` and `contested`: `standing` of the cast out words and the
  tree, cached.
- The **root**: the keys the brotherhood started from, as this machine
  pinned them (below), and whether it is pinned for good yet.
- Notes for itself alone, never served and never consulted by `resolve`
  or `standing`: which brothers showed it they heard each ending
  (`acks`), when it first saw each ending and each orphan (local clock),
  the sessions it let go and when, the copies told to its human, the
  contests put to its human, and the handoffs it signed that have not
  landed yet (`pending`, with when, by this machine's clock).
- Each brother's own signed entry (name, link, keepers, the switch for
  taking over by itself, whom it lets only watch, the root it stands on
  and whether it is pinned, and which brothers it is linked to now). An
  entry decides nothing about owners or about who is in. It says whose
  turn it is among keepers, whether a keeper takes over by itself, what
  root a machine that joins it pins, and whether a brother is reached by
  any other.

`standing.json` holds the human's own switches: whom it lets only watch,
whom it cast out, whom it let in by a code, and a cache of whom the words
bar, rewritten each time they change. Nothing in it decides that this
machine, or any brother, is out: that is worked out from the words each
time.

## The words

Every word is

```
{ n, by, under, at, after, kind, ...fields, sig }
```

`by` is the signer's relay id, `under` the public key it signed under
(b64url), `at` the signer's clock (seconds, shown to humans, never
compared with another clock), `sig` ECDSA P-256 over SHA-256, `r || s`.
`after` is the signature of the word that made `by` the owner this word
builds on (the session's first word, the handoff it acknowledged or the
takeover it made); an acknowledgement names the handoff it
acknowledges; a session's first word, an admission and a cast out name
nothing. So a word signed on a handoff that was later taken back, or on
a step that lost, builds on nothing, whatever number it carries: a
brother that gets the same number again by a new word starts afresh.

The signature covers `"horadric said v5"`, then each part led by its
length as a 4 byte big endian number: the kind, `by`, `under`, `n` and
`at` (8 bytes big endian each), `after`, then for a session `origin` and
`session`, then the kind's own fields in the order the table gives, a
list as a 4 byte count and then each item led by its length, a flag as
one byte.

| kind | fields | says |
|---|---|---|
| `owns` | `origin`, `session`, `keepers`, `v` | `by` runs the session under `n` and names `keepers` (in order) as who may take it over from it at `n`, the `v`th time it names them at `n` |
| `hands` | `origin`, `session`, `to` | `by`, the owner at `n` - 1, hands it to `to` |
| `recalls` | `origin`, `session`, `hand` | `by` takes back its `hands` at `n` whose `sig` is `hand`: it did not land |
| `takes` | `origin`, `session`, `from`, `keepers` | `by`, a keeper of `from`, took it over from `from`'s `n` - 1, and names its own `keepers` |
| `ended` | `origin`, `session` | `by`, the owner at `n`, ended it for good |
| `admits` | `id`, `key`, `name` | `by`'s human let brother `id` in under `key` by a code both humans saw; `n` is 0 |
| `cast_out` | `id`, `key`, `retired` | `by`'s human cast out brother `id`, whose key was `key`; with `retired`, only that key goes, not those let in through it (the machine was set up anew); `n` is 0 |

A word is **taken** by a brother when:

- `under` is the key it holds for `by`, or the key it held for `by` before
  (a key a human replaced). A key named only in some word is never taken
  as anyone's: a thief cannot make its own key pass for another
  brother's by naming it. Admissions and cast outs speak of keys, never
  of ids, so one is taken under any key the tree reaches, held here or
  not yet: a brother just let in learns every other so, and a key no
  admission path reaches signs nothing that counts.
- the signature verifies under `under`;
- it is within bounds: every id and key at most 256 bytes, at most 16
  keepers, an admitted or cast out `key` a P-256 point.

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

## The tree: who is in, and through whom

Input: every `admits` word held, and the **root**, a set of keys. Output:
the **members** (keys), the id each goes by, and for each member key `X`
its **line** `D(X)`: the members every admission path from the root to
which passes through `X` (`X` among them).

1. An admission `A admits (id, K)` is an edge from key `A` to key `K`.
   Of one signer's admissions, only the 32 naming the lowest keys count.
2. The members are the keys reached from the root along edges, the root's
   own keys first, then by how many edges away, then by key: at most 256.
3. `D(X)` is what `X` dominates: the members no longer reached from the
   root once `X` is taken away. The lines nest: two lines either share
   nothing or one holds the other.

A thief's fakes are in its line whatever they sign: they were let in by
the thief or by each other, and no honest brother signed their
admission, so every path to them passes through the thief. An honest
brother is in a thief's line only when it came in through the thief
alone. Admissions a thief signs of honest brothers only add paths to
them, never take one away, so they put nobody in its line.

**The root** is pinned once by each machine in `roster.json` and only
read after:

- Two machines that join by a code when neither has any brother pin the
  two of them: they are the brotherhood's first.
- A machine that joins a brother that has a root pinned (says so in its
  own signed entry) when it had none pins that root. Until then it holds
  the two of them, and says so unpinned in its entry: two first machines
  each see the other say the same two keys, and pin them.
- A machine from before admissions (see "Upgrading") pins its own key and
  every key it holds as it upgrades: every brother of a brotherhood that
  upgrades together pins the same keys.

Every honest brother of one brotherhood pins the same root, so all of
them work out the same tree. A machine that came in through a thief may
pin what the thief says (it is in the thief's line in any case).

Every code join signs an `admits` on both sides, naming the other. A
machine that had no brother names only its new brother; one that had
brothers lets the new one into its tree. A human's yes to taking a key
back (below) signs an `admits` too.

## Keys: which key a brother goes by (I0)

A brother `B` is held under one key. It changes only so:

- **By a code here.** The human here joined `B` by a code, both humans
  seeing the same code: the key it showed is held, and the one held
  before goes into `Store::cast`.
- **By a human's yes here.** A member key `K'` for `B`'s id comes in the
  tree while the key held for `B` is out (cast out or retired). The
  human here is asked, once per key, whether `B` was set up anew; yes
  holds `K'` and signs `admits` of it. A node with no human keeps the key
  it holds; joining it by a code is the way.

So no brother's word, nor any entry, ever replaces a key held, and a
thief that casts a brother's key out and lets in another under its id
gains nothing: every brother asks its human, and says no unless the
human set that machine up anew.

A brother not held at all is **introduced**: its key is taken, with no
code, when it is a member and in (not out, not contested), every member
that admitted it names it by one id (said two ways, or two keys for one
id, it is nobody's to choose), that id is not held under another key,
and this machine's human never cast that id out (then the human is
asked, as above). Introductions come only from the tree: never from
what an entry says it holds.

## `standing`: who is out

Input: every `cast_out` word held, and the tree. Output: the keys
**out** and the keys **contested**.

1. A cast out counts only when its signer and the key it names are both
   members, and the signer does not name itself. Of one signer's words
   naming keys that are not members, only the 32 lowest are kept.
2. **Covers.** `S` casting `X` out attacks every key of `D(X)`: a line is
   cast out whole, and a fake made after the word was signed is in it
   as soon as it is in the line. When `S` is itself in `D(X)` (a
   **rebel**, turning on the line it came through), it attacks `X`
   alone. A **retirement** (`retired`) is no attack: it is a human's word
   that a machine was set up anew. Steps 3 to 8 are worked out without
   retirements; then every key a retirement names is out too, alone, when
   its signer is neither out nor contested by them, though its signer was
   retired in turn since. Only a cast out for real voids a retirement.
3. **Heads.** When `A` and `B` attack each other, the contest is between
   two heads: the topmost key `B` named whose cover holds `A`, and the
   topmost key `A` named whose cover holds `B`. So when a human casts out
   a thief, every fake of the thief that cast that human's brother out
   is answered by the same word, and is the thief's contest, not one of
   its own: fakes count nothing.
4. **Deciding a contest.** Each head's rivals are the heads it is in
   contests with, and the rebels that cast it out without its answer
   (each a rival of its own: a brother turning on its own line is no fake
   of it). A head with more rivals than each of its rivals has is out (a
   thief answered by humans on brothers of two lines loses, however many
   fakes it has), and so is every key that fought as its voice. Repeated
   until no head loses so.
5. **Waiting.** A key in a contest, and every key in the line of a head
   in one, counts only against the lines of that contest's rivals: a
   thief answered by one brother casts out nobody else meanwhile, and
   neither do its fakes. A rebel's word against the head of its own line
   always counts.
6. The grounded labelling of what is left: a key all of whose attackers
   are out is **in** (a key nobody attacks is in), a key attacked by a
   key that is in is **out**, until nothing changes. A key out takes its
   whole line out with it, however it went out (decided by the brothers
   of its own line, its line is covered by nobody's word, and a thief's
   fakes must not stay in by that), and a key contested makes its line
   contested at best.
7. A key out leaves its contests, so its rivals' other attacks count
   again: steps 3 to 6 are worked out again until nothing more goes out.
8. Every key neither in nor out is **contested**.

So:

- A cast out by a brother that is out casts nobody out, whenever it was
  signed.
- Two brothers that cast each other out, with nobody else deciding, are
  both contested, and so are their lines: none acts on any brother until
  a human decides. A human decides by casting one of them out from a
  brother of another line, or from a brother of the thief's own line: if
  that brother is in and the one it casts never cast it out, the one it
  casts is out; if the thief had cast it out too, the thief now has more
  rivals than any of its own and is out.
- A brother out is out whatever it signs before or after, and so is its
  line: every key let in only through it, however it went out.
- A human casting out a brother this machine came in through is told
  that this machine goes with it once that brother is out, and is to be
  joined again by a code from a brother of another line to stay. Its word
  still counts toward deciding: a rebel is a voice of its own.
- Whom a brother lets only watch it decides what that brother may do to
  it, not whether that brother's cast outs count: who is out must be the
  same on every brother (I1), and one brother's switch cannot be.

## What being out does, worked out afresh

Each time the words change, and at start, each brother works out from
the roster held:

- **A brother out** has no control here, its tiles go and its sessions'
  copies are not offered, and it is named as no keeper. Its key stays
  in `brothers.json`, and its link stays up, carrying only rosters and
  signed word both ways: so the moment a later word puts it in again
  (its human answered, a contest went its way), it is a brother again
  everywhere, with nothing to undo. One this machine's human cast out
  links only under the key it had, for rosters and word, so it hears the
  word; asking to join, it is refused unasked.
- **A brother out only because its line was cast out** joins again when
  a human on a brother in asks it, by a code, under the key it has: that
  admission gives it a line of its own. One whose very key a brother not
  out named is asked to set itself up anew.
- **A brother contested** has no control anywhere and is named as no
  keeper; its link stays as it is, so it hears how the contest ends, and
  the takeovers it made stand: which of the two is the thief is what the
  contest has not decided.
- **This machine out**: it serves and hears nothing but rosters and
  signed word on its links, and its human is asked whether to leave the
  brotherhood. Leaving, and only that, lets every brother and its own
  keys go. Restarted, it works the same out again from the words; a
  later word that puts it back in puts it back in.
- A plain `{"t":"cast_out"}` on a link is no word: it only says a roster
  is worth asking for, and changes nothing by itself.

## `resolve`: whose a session is

Input: the session's creator `origin`, every word held on it, and which
keys are out. Output: its owner and number, the owner's keepers and how
it came to own it, its ending if it ended, every brother that owned it
along the chain, the words that can matter (the only ones kept), and the
brothers that ran a copy whose word did not stand.

1. **Genesis.** The chain starts at the creator's own `owns` with the
   lowest `n`, at most 1,000,000, naming nothing `after`. Nobody else
   starts it. The word that made the owner (`M`) is that word.
2. **Keepers at a number.** The owner's keepers at its number `n` are
   those named, with the highest `v`, by its `owns` at `n` after `M` (an
   acknowledgement names the handoff, which is `M`), or by `M` itself (the
   first word, or the `takes`). An owner reached by a handoff it never
   acknowledged names none.
3. **Steps.** From owner `O` at `n`, made so by `M`, the candidates at
   `n` + 1 are the words after `M`:
   - each `hands` by `O` to another brother that no `recalls` by `O`
     names: **acknowledged** if its receiver signed an `owns` at `n` + 1
     after it, rank 0, else rank 3;
   - each `takes` by a keeper `k` of `O` at `n`, from `O`: rank 1,
     ordered by `k`'s place among the keepers.
   The best candidate (lowest rank, then place, then the receiver's id,
   then the word itself) makes the next owner. Ties between two words
   that make the same brother owner are no rivalry.
4. **Ending.** `O`'s `ended` at its own `n` after `M` ends the session there,
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
   owner before it, 32 at most (the lowest); the ending. Nothing else
   is kept nor passed on.
8. **Copies.** A brother made owner by a candidate that lost, at rank 0
   or 1, other than the brother that won, ran a copy that does not stand.

## Holding and dropping (`believe`, `prune`)

- A brother takes in new words, then works out the tree, `standing` and
  every session the new words speak of, and keeps only what `resolve`
  kept.
- A session whose creator's genesis is not held yet is not held: its
  words wait in no queue, they come again with the next roster.
- **Bounds per creator.** Of the sessions one creator started that this
  machine holds (its own excepted), it keeps the 128 live ones with the
  lowest keys, the 1024 ended ones with the lowest keys, and no more
  than 16,384 words for all of them, dropping whole sessions from the
  highest key down. Which sessions are kept depends only on the words
  held, never on their order.
- A creator takes no word on a session it created that it does not hold:
  it knows its own sessions, and one it let go stays gone. Nor does it
  start one it let go afresh: a session let go here is not this
  machine's to bring back under its id.
- **Pruning.** An ended session's owner and words go once every brother
  in good standing showed it heard the ending (its roster ends the
  session, or lets it go), or two years after this machine first saw the
  ending: until then any brother asleep through it, or set up anew, hears
  of the ending from any other. Days count on this machine's clock from
  when it first saw the ending, never from the word's `at`. A session
  whose owner is out, live or ended, goes thirty days after this machine
  first saw it so, unless a keeper took it over meanwhile.
- **Tombstones.** A session let go is refused, whatever anyone says of
  it, for as long as any brother held, out or not, has not shown it heard
  (its roster ends or lets it go), ninety days at least: a brother that
  slept through the ending for a year must not bring it back, nor one out
  whose rosters still come. The brothers' showing is
  kept beside the tombstone. At most 4096 tombstones per creator are
  kept (the oldest go first), and none past two years.
- A session the brother that owns it here lists as let go (`let_go`),
  in a roster over its own link, is let go here too: only an owner that
  ended it lets it go, and so a brother that slept through the ending
  does not keep it as live, and shows it heard.
- Pruning, the bounds and the notes are this machine's own. Two brothers
  that hold the same words agree; pruning only ever drops words, and
  what it drops was over.

## This machine's own word

- **Starting** a session signs its genesis, naming this machine's keepers
  (its book's, less any it lets only watch and any out or contested).
- **Handing** session `s` at `n` to `B` signs `hands` at `n` + 1, with an
  `at` past any handoff of `s` it signed before, keeps it in `pending`
  with this machine's clock, and sends it in the bundle with every word
  held on `s`. Only when `B` says it took it is the same word taken in
  here. A handoff that fails is recalled by a `recalls` naming that very
  word; the next try is a new word and counts. One whose receiver's
  acknowledgement is held landed, whatever the app heard, and is taken
  in rather than recalled. Ending a session takes back any handoff of it
  still in flight first.
- **A handoff left by a process that is gone** (the app restarted, or
  reloaded, while it was in flight) is settled from the words: once the
  receiver's acknowledgement is held, the handoff is taken in, as if the
  process had seen it land; while it is not, the handoff is recalled
  only ten minutes after it was signed, so a bundle still landing is not
  cut from under its receiver.
- **Receiving** checks, before the bundle lands, that the handoff was not
  recalled and that no other brother acknowledged a handoff at that
  number, then signs `owns` at `n` + 1 with its own keepers.
- **Changing keepers** (the book, a view only switch, a cast out) signs a
  new `owns` with the next `v` for every session it owns that names
  others.
- **Taking over** from `O` at `n` signs `takes` at `n` + 1, only while
  `O` still owns the session under the number the snapshot was cut at,
  names this machine among its keepers at that number, never ended it,
  and no brother linked now lists it.
- **Taking over by itself** (the owner's switch) needs more: positive
  word that the owner is gone. The owner is silent on this machine's own
  link past the switch's time, the relay does not list it connected,
  and no brother linked here now says in its own signed entry that it is
  linked to the owner. A relay that drops only the frames between the
  owner and this keeper starts no takeover.
- **Ending** signs `ended` at its number.
- **Joining** by a code signs `admits` of the other, and pins the root
  (above). A key the brother joined under before is **retired**, not
  cast out: the human set the machine up anew, so those let in through it
  stay. The human is told that if the old machine may be in a thief's
  hands, it is to be cast out instead, its line with it.
- **Casting out** signs `cast_out`. A human is shown, before saying yes,
  the brothers that go with it (its line), and told when this machine
  came in through it. A human seeing a contest is asked once whether to
  cast out one of the two: the head of the other line, never one that
  would name this machine's own line's head on a thief's word.

## The invariants

Each is checked by the simulations after every step, and each finding of
the five reviews has a scenario of its own.

- **I0, identity.** The key every honest brother holds for an honest
  brother is that brother's own key, unless that brother's human set it
  up anew and a human here said yes to its new key. No word, no entry,
  no relay, and no number of stolen brothers and fakes change it.
- **I1, agreement.** Two honest brothers that hold the same keys and the
  same words, and pinned the same root, agree on every session's owner,
  number, keepers and ending, on the members and their lines, and on the
  out and contested keys (and so on whom each treats as dropped, and
  whether each is itself out), whatever order the words came in. Holds
  because the tree, `resolve` and `standing` are functions of the word
  set and the root, what is kept is chosen by them alone, and nothing
  that decides is stored apart from the words.
- **I2, no fencing by those without the right.** A live session run by
  an honest owner never moves away from it by anything signed or replayed
  by a brother that is not its keeper at its number: one it never named,
  dropped, switched to view only, or that was contested or out when it
  last named its keepers; nor by a brother out, whenever it signed; nor by
  the relay. Holds because a step needs the owner's own `hands`, or a
  `takes` by a keeper its newest keeper word names under a key that is not
  out, owners name no contested or out key, and words are signed. The one
  way an honest owner's session moves without its word is a takeover by a
  keeper it names, which is what naming a keeper allows; a keeper taking
  over by itself does so only on positive word that the owner is gone.
- **I3, one owner.** Every session held has exactly one owner, and no two
  honest brothers both run it acknowledged at the same time once they
  heard each other's words: the owner is one brother, and a brother whose
  copy lost is told. A session is never left with nobody able to run it
  by a recalled or failed handoff: a recall puts it back on its sender. A
  restart never recalls a handoff that landed.
- **I4, cast out stays out.** Once a human on an honest brother that is
  in casts brother `X` out, `X` and every fake it let in have no control
  on any honest brother once they hold that word, whatever `X` and its
  fakes signed before or after: `X` is out, or contested when it cast
  that brother out too, and then out once a human on a brother of a
  second line casts it out as well.
- **I5, bounded.** No brother can make an honest one hold more than the
  bounds above (sessions per creator, words per creator, words per
  session, admissions and cast outs per signer, members, tombstones), nor
  send a frame past 1 MB: rosters go in parts of at most 256 KB, and no
  word is longer than a part.
- **I6, no honest dead end.** In honest use (many sessions, Warriv, failed
  handoffs retried, brothers asleep for months, restarts and reloads
  mid-handoff, upgrades) nothing gets stuck: a recalled handoff can be
  tried again to the same brother and lands, a session can always be
  ended, every ended session is pruned in the end, and no ended session
  comes back.
- **R, recovery.** Once the humans have answered every stolen brother
  (cast it out from brothers of two lines), every honest brother not in
  a stolen one's line is in, holds every other under its own key, links
  to every other with control as its switches say, and nothing a
  stolen brother or the relay did before is left in force.

## What is not promised

- A thief that holds the key of a keeper may take that keeper's
  sessions over until it is cast out: that is what being a keeper means.
- An owner that changes its keepers while a keeper it drops is taking
  over decides that race for itself: the newest keeper word stands, so
  the takeover does not, and the taker's copy is told it ran twice.
- A thief that casts out brothers before any human casts it out puts
  them out until a human acts: casting the thief out from one machine
  makes the two contested and the rest in again, from brothers of two
  lines the thief is out.
- **Lines are voices.** Brothers that came in through the same brother
  are one voice against a thief that cast that brother out, as a
  thief's fakes are one voice: from the words the two cannot be told
  apart. So while one human has answered a thief, both of their lines
  are contested; a second line (or a brother of the thief's own line)
  decides. One of the brotherhood's first two machines stolen, with
  nobody let in through it but its fakes and every honest brother let in
  through the other, leaves no second line: every brother stays
  contested until the brotherhood is set up anew. Joining some brother
  through each of the first two gives that line.
- **Two machines really stolen.** Two stolen brothers of two lines, each
  let in by a human, are two voices: together they can hold a contest
  against one honest line, or put an honest brother out that only one
  line answers for, until a human on a third line decides.
- A brother that came in only through a brother that is out goes with it,
  honest or not: from the words it cannot be told from a fake. Its human
  joins it again by a code from a brother in, which gives it a line of
  its own.
- A relay that hides an owner from a keeper entirely (drops every frame
  between them, lists the owner as gone, and drops every brother's
  roster that says it is linked to the owner) can make a keeper that
  takes over by itself do so. A keeper the human asks to take over does
  what the human says.
- A machine set up anew is taken under its new key by a brother only
  once a word of that key reaches it from one that took it (and its human
  says yes): a brother that hears of none (two machines set up anew one
  after the other, joined to each other while the rest were away) reaches
  them again only by a code. No brother's word moves a key, which is what
  keeps a thief from moving one.
- A brother whose key was replaced by a human is followed under its old
  key only by brothers that held the old key. A brother that never held
  it cannot check those words and does not follow sessions started under
  it.
- Two brotherhoods joined by a code between one brother of each are
  linked through those two: each side's tree does not hold the other's
  brothers, who join by codes of their own.
- A machine joined by a code to a thief pins what the thief says is the
  root, and is in the thief's line.
- More than 256 members: the members furthest from the root are not
  held. A thief can fill places as far from the root as its fakes are,
  never closer.

## The relay and tokens

- A channel whose handshake has not ended a minute after it was given is
  closed and asked for again, on the next frame the relay sends (its
  answers to pings come every few seconds): a frame the relay dropped
  must not keep two brothers apart for good.

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

The words changed in 0.35 (`"horadric said v5"`, which added admissions
and `retired`). No released Horadric ever held a roster (0.32 is the
newest release; machines that ran 0.33 and 0.34 builds, and 0.35 builds
before admissions, were development machines). A `roster.json` from
before is started anew: its words and owners go, and each machine signs
a genesis for every session it runs. Brothers, keys, entries and the
human's switches stay, and so does every cast out its human made: the
machine signs each again (`standing.json` keeps them). A machine with
brothers and no root pins its own key and every key it holds, and signs
an `admits` of each brother its human let in. A machine that joins one
of them with no root pins what that one's entry says.
