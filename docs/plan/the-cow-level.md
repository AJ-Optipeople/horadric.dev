# The Cow Level

"The Cow Level" in [IDEAS.md](../IDEAS.md). A secret recipe in the cube, as
in Diablo II, where Wirt's Leg and a tome of town portal open the way.

- **The way in.** A session whose name has "wirt" in it, in any case
  ("Wirt's Leg" after a rename), alone in the cube with `main`, the tome.
  `cube::recipe` checks for it before the rest and whatever the session
  is doing, so it wins over the merge its branch would make and runs mid
  turn: it does nothing to the session. Nothing on screen hints at it.
  The leg alone still says "Add main to merge, or another to review",
  and only once `main` is in does the key offer "Open a portal".
- **The portal.** It plays in the cube's own window, never over a tile:
  the leg and `main` swirl in with red light, then a red portal stands
  over the cube with specks turning inward on three rings, and the key
  says "There is no cow level". `motion::portal` (tested) swirls as long
  as a transmute does, opens fast, holds, and fades, 3.5 seconds in all.
- **It starts nothing.** The cube empties and the session stands in its
  cluster as before: no agent, no merge, no journal line.

Tested on screen with a dev instance on its own port, `APPDATA` and
`LOCALAPPDATA`, and `cmd.exe` as the agent: "Wirt's Leg" and "other" in
one cluster, the leg lifted onto the cube (slid under the cursor, the lift
posted), `main` and the key clicked by posted messages, and a `PrintWindow`
of the cube every 120 ms. The portal opened and closed as above, both
tiles stayed, and there were still two hosts and two `cmd.exe`.
