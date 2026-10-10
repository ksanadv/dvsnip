# dvsnip

Press Print Screen, select an area, paste it anywhere. A tiny replacement for the Windows Snipping Tool's Print Screen
capture, written in Rust on plain Win32 and GDI. No tray icon, no window of its own, no runtime, no admin rights: one
0.8 MB exe that waits invisibly at 0% CPU and about 1 MB of memory.

## Install

Download `dvsnip-<version>-windows-x64-setup.exe` from the
[latest release](https://github.com/ksanadv/dvsnip/releases/latest) and run it, or build it yourself:

```powershell
cp .cargo\config.toml.example .cargo\config.toml   # once; set target-dir for your machine
.\build-installer.ps1                              # dist\dvsnip-<version>-windows-x64-setup.exe and -portable.exe
```

The setup (~2 MB) installs for the current user, starts dvsnip with Windows, makes Print Screen open dvsnip instead of
the Windows capture, and adds a Start menu shortcut, an Apps & features entry and `dvsnip-cli` on your Path. Running it
again updates in place; `--quiet` makes it silent. Uninstall from Apps & features, and Print Screen goes back to Windows.
Without the installer, `dvsnip.exe` takes Print Screen until you run `dvsnip.exe --quit`.

## Use

Print Screen freezes and dims the screen. Pick a mode in the bar at the top:

| mode | how |
|---|---|
| Rectangle | drag a rectangle |
| Window | click a window |
| Full screen | click the button |
| Freeform | draw around the area; outside the line is transparent |

On release the snip is on the clipboard as a bitmap and as PNG. Esc or a right click cancels, and the last mode used is
remembered. Options are switches, silent when they work:

```text
dvsnip.exe --autosave on|off   also save each snip to Pictures\Screenshots (default off)
dvsnip.exe --prtsc on|off      whether Print Screen starts a snip; off hands the key back to Windows
dvsnip.exe --quit              stop the background process
```

## Compared with the Windows Snipping Tool

Windows 11 build 26300, Snipping Tool 11.2607.23.0, one 1920x1080 monitor at 125%. The Windows rows marked * were
measured on this machine; the rest is documented behaviour.

| | Windows Snipping Tool | dvsnip |
|---|---|---|
| size on disk * | 586 MB package (`SnippingTool.exe` 2.3 MB) | 0.8 MB exe, ~2 MB setup |
| while idle * | no process running; starts on demand | 0% CPU, ~1 MB (0.4-2.6 MB seen), always resident |
| until the capture overlay is on screen * | ~1.1 s (window 0.65 s, then New, overlay 0.2-0.3 s later) | ~1 ms to the dim layer, ~60-70 ms until ready |
| memory with the overlay open * | 133.5 MB working set, 93.6 MB committed | ~9 MB private |
| capture modes | rectangle, window, full screen, freeform | the same four |
| result | clipboard, plus a notification that opens its editor | clipboard (bitmap + PNG), nothing else shown |
| save to file | yes | optional `--autosave` |
| markup, OCR, video, delay | yes | no |
| command line / scripting | no | `dvsnip-cli`, JSON output, MCP server |
| dependencies | Store app, Windows runtime | none: no image library, no COM |

dvsnip is the faster, lighter way to get pixels onto the clipboard; the Snipping Tool is still the one to use when you
want to annotate, record or extract text.

How the Windows numbers were taken: the Snipping Tool's capture handler (`ms-screenclip:`) was started directly, then
its own *New screenshot* button was pressed through UI Automation (no keys sent), five runs with the process killed
between them; times are to a visible, uncloaked full-screen overlay window and good to about 30 ms. They leave out
the time for the key press to reach Windows, so they favour the Snipping Tool slightly. dvsnip's time to the dim layer
is from five real Print Screen presses (0.8-4.8 ms, median 1.1 ms); its other figures are from development runs and
were not repeated alongside the Windows ones.

## For AI agents and scripts: dvsnip-cli

```text
dvsnip-cli capture [--monitor N | --window "title part" | --hwnd H | --region X,Y,W,H | --foreground]
                   [--max PX] [--out FILE | --out -] [--copy] [--delay S] [--json]
dvsnip-cli snip [--mode rectangle|window|freeform]   the user selects; Esc exits with code 2
dvsnip-cli windows | monitors                         JSON
dvsnip-cli mcp                                        MCP server on stdin/stdout
```

To give Claude Code eyes on the desktop: `claude mcp add dvsnip -- dvsnip-cli mcp`. The server offers `screenshot`,
`list_windows`, `list_monitors` and `user_snip`. Images are shrunk to 1568 px on the long side by default and come with a
JSON line giving the captured rectangle and scale, so an image point maps back as `x + image_x / scale` (physical pixels).

## Numbers

Release build, one 1920x1080 monitor at 125%, median of several runs.

| | dvsnip |
|---|---:|
| idle working set / committed | 0.4-2.6 MB / ~1.3-2.1 MB |
| Print Screen to frozen overlay ready | ~60-70 ms |
| overlay open (1080p) | 9.3 MB private |
| PNG of a 700x450 snip | ~5 ms, 46 KB |
| `dvsnip-cli capture` of the desktop | ~120 ms including process start |

## How it works

- **Fast dim:** a half-transparent black window covers the screen within ~5 ms, excluded from capture, so the one screen
  copy taken behind it is clean. That copy is the only image kept in memory.
- **Own PNG encoder** (`png.rs`) on a worker thread; no image library or Windows codec is loaded. Icons are drawn by code.
- **Print Screen:** Windows 11 keeps the key for its own capture, so the installer turns off *Settings > Accessibility >
  Keyboard > Use the Print screen key to open screen capture* (and restores it on uninstall) and registers a normal
  hotkey. When run without installing, a low-level keyboard hook swallows a plain Print Screen instead.
- **Default-app link:** the installer also offers dvsnip as the handler for `ms-screenclip:`. Measured: with Windows'
  own Print Screen setting switched on and dvsnip chosen for that link, the key's snip ended up in dvsnip, not in the
  Snipping Tool. Pick the Snipping Tool again under *Settings > Apps > Default apps* to undo it.
- Source: `resident.rs`/`hook.rs` (background process), `overlay.rs`/`capture.rs` (selection and capture),
  `png.rs`/`image.rs` (pixels), `cli.rs`/`mcp.rs` (console tool), `install.rs`/`installer/` (setup).

## Limits

- Tested on one monitor; several monitors with different scaling should work but have not been tried.
- No video, OCR, delay timer or markup. No tray icon, so options are command-line switches.
- Needs Windows 10 2004 or later for the instant dim layer. HDR is captured as Windows hands it to GDI.

Settings live in `%APPDATA%\dvsnip\settings.txt`. `cargo test` runs the unit tests.

## Releasing

```powershell
.\release.ps1 -Bump 0.2.0   # sets the version in both Cargo.toml files; commit it on a branch and merge it
.\release.ps1               # on main: builds, tests, tags v0.2.0 and pushes the tag
```

Pushing the tag runs the Release workflow, which publishes the setup and portable exes with a `.sha256` for each.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE).
