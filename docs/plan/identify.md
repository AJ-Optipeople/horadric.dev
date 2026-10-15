# Identify

In Diablo an unidentified item drops grey until you identify it. A turn
that finished while you looked elsewhere is the same: you do not know what
it did until you look. Before this a finished tile stayed green until its
next prompt, so ten green tiles said nothing about which ones you had read.

- **Unread** (`Session::unseen`, `Session::unread`, pure and tested). Set
  when the phase becomes done, cleared by the next phase or when you look.
  Only a change into done sets it, so a second `Stop` on a turn you have
  read changes nothing. Not saved, like the phase it belongs to.
- **Looking** is the session's pane having the keyboard with the stage in
  front (`App::identify`), the same session whose key is latched in. Just
  being in the grid is not enough: the panes without the keyboard step back
  behind a veil, and you may be typing into the one beside it. Checked when
  the keyboard moves (`Input::Spotlight`), after hook events, and every
  second, since the stage coming to the front tells the app nothing.
- **On the tile** an unread turn keeps the green lamp it has now. Once
  identified the lamp goes dark, the check mark dims and the key stands back
  like an idle one (`render.rs`, drawn as idle). So a lit lamp still always
  means something for you, which is the rule of "The look".

Tested on screen with a dev instance on its own port and `APPDATA`, two
`cmd.exe` sessions given a fake prompt and `Stop` each: with the stage
behind other windows both lamps stayed green; with the stage in front the
one with the keyboard went dark within a second and the other stayed lit;
Ctrl+Alt+Left moved the keyboard over and that one went dark too.
