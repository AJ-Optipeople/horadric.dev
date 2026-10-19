# Claude Code 2.1.287 to 2.1.296

What is left from the 2026-10-11 pass over the changelog: only cloud
session tiles. Everything else is built and its notes are in the lore.

## Cloud session tiles

Not built. **Decided 2026-10-13: wait.** Build nothing until Claude Code
lists cloud sessions on this machine. When `claude agents --json`
reports them (a `kind` such as `cloud`), the feeder that finds
background sessions (`crates/horadric-ui/src/agents.rs`,
`crates/horadric-core/src/background.rs`) picks them up, and the tile
work starts from there. Until then: no private APIs, no attach-only
tiles, no hooks in the cloud.

Recheck after each Claude Code upgrade: run `claude agents --json --all`
with a cloud session running and look for a new `kind`. On 2.1.296 it
listed only `interactive` and `background`.

**Open questions for the human**, for when the list appears:

- Is a cloud tile only worth having if it shows sessions started on the
  web or the phone too, or is one for sessions started here enough?
- Does attaching a pane to a cloud session (`claude --cloud <id>`) cost
  anything on the cloud side? It keeps a connection open, but the
  session runs either way.
