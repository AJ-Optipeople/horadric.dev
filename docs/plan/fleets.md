# Fleets

Some projects manage hundreds of devices over SSH, a fleet of IoT boxes
rather than a few servers. SSH hosts was built for a handful and breaks
there in three places: the project menu lists every host (and stopped at
80), each connection is a tile and a pane, and every agent's prompt names
every host, hundreds of names on every session for the few it touches.
Asked for because that is how some of the human's projects work.

- **An inventory, not a list.** `"inventory": "devices.ini"` in
  `.horadric/config.json`, a path from the project's folder or absolute.
  The fleet already lives in a file like it, so Horadric reads that
  rather than a copy in `hosts`. The format is Ansible's INI, which a
  plain list of names also is: `[group]` headings, one device a line, its
  name first, and `ansible_host`, `ansible_user` and `ansible_port` when
  the name alone does not reach it. `:vars` and `:children` sections are
  skipped. A port becomes an `ssh://user@host:port` destination, the one
  form `ssh` takes a port in. YAML inventories are out: a parser is a
  dependency, and INI covers the common case.
- **Found by name, not listed.** With an inventory, or more than eight
  hosts, the project menu has one "SSH to..." in place of a line a host.
  It opens the question dialog with the hosts and the devices as
  suggestions, each with its group, found as it is typed by every word in
  the name, the group or the address, names that start with it first. The
  best match is lit, so Enter takes it, and anything else `ssh` takes
  connects too when nothing matches. "Add host" does the same with the
  `~/.ssh/config` names when there are more than a submenu reads well.
- **A device names its terminal.** "SSH 7" says nothing among dozens, so
  a terminal opened on a device from the inventory is named after it. The
  destination is its second line, and a restart brings it back by both.
  Terminals are opened one at a time, on demand, so a fleet of 240 is
  still a few tiles.
- **The agent is told where, not which.** Its prompt gives the
  inventory's path, how many devices and the groups (twelve named, the
  rest counted), how to read a line, to look a device up before reaching
  it, and to loop with `-o ConnectTimeout=10` so a device that is offline
  does not hold up the rest. Past twenty, `hosts` are counted the same
  way.
- **Pure and tested:** reading the inventory and its path, destinations
  with users and ports, finding, the groups, the prompt, counting many
  hosts, and what a typed host is refused for.

Done, all of it. Seen on screen with a dev instance and a project of 240
devices in three groups plus one host: the menu had "SSH to..." and no
host lines, the picker said 241 to pick from with groups beside the
names, "cam 07" lit camera-007 first, Enter opened a tile named
camera-007 running `ssh -- pi@camera-007.invalid`, and its refusal kept
the pane with the tile paused. The picker first drew the folder dialog's
folder icons and Tab added a `\`, so a pick now says its icon and whether
its suggestions are paths. Checked with `horadric run` on haiku: told
nothing else, the agent knew the count, groups and path, and asked for
the command for camera-012 it read the inventory and gave
`ssh.exe -o BatchMode=yes pi@camera-012.invalid 'uptime'`. The first
wording let it guess the bare name, hence the line about looking a device
up. Not tested: a real device, for want of one, and running a command
across many.
