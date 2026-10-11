# The bundled pseudo console

`conpty.dll` and `OpenConsole.exe` for x64, unchanged, from the
`Microsoft.Windows.Console.ConPTY` NuGet package, version
1.25.260930003 (built from github.com/microsoft/terminal, MIT, see
`LICENSE`). `horadric_pty::openconsole` embeds them, and its `VERSION`
must name the package they came from.

Why: Windows' own pseudo console answers `CSI c` itself, so Claude Code,
which fences its OSC 7501 query with one, hears no. This one passes it on
to Horadric, which answers the query and then the fence, in order.

To update: download
`https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/<v>/microsoft.windows.console.conpty.<v>.nupkg`,
unzip it, copy `runtimes/win-x64/native/conpty.dll` and
`build/native/runtimes/x64/OpenConsole.exe` here, and set `VERSION`.

| File | SHA-256 |
| --- | --- |
| conpty.dll | feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a |
| OpenConsole.exe | 3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586 |
