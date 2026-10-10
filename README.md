# dvsnip

Press Print Screen, select an area, paste it anywhere. A replacement for the Windows Snipping Tool's Print Screen
capture, written in Rust on plain Win32 and GDI. It has no tray icon and no window of its own: it waits invisibly at 0%
CPU and about 0.1 MB of resident memory, and is only seen while you are taking a snip. One 0.8 MB exe, no runtime, no
admin rights, no image library or Windows codec.

## Install

```powershell
.\build-installer.ps1
.\dist\dvsnip-Setup.exe
```

`dvsnip-Setup.exe` (~2 MB) installs for the current user into `%LOCALAPPDATA%\Programs\dvsnip`: it starts dvsnip with
Windows, makes Print Screen open dvsnip instead of the Windows screen capture, adds a Start menu shortcut (which just
starts a snip) and an Apps & features entry, and puts `dvsnip-cli` on your Path. Running it again updates in place.
Silent: `dvsnip-Setup.exe --quiet`. Uninstall from Apps & features or `dvsnip.exe --uninstall`; Print Screen goes back to
Windows.

Without the installer, run `dvsnip.exe` from the build folder: it takes Print Screen until you run `dvsnip.exe --quit`.

## Use

Print Screen freezes the screen and dims it. Pick a mode in the bar at the top:

| mode | how |
|---|---|
| Rectangle | drag a rectangle |
| Window | click a window (it is highlighted under the cursor) |
| Full screen | click the button: the whole desktop is taken at once |
| Freeform | draw around the area; outside the line is transparent |

When you let go the snip is on the clipboard, as a bitmap and as PNG, so it pastes into chats, documents, image editors
and browsers. Esc or a right click cancels. The mode you used last is the one the next snip opens in. Ctrl, Shift, Alt
and Win + Print Screen keep doing what Windows does with them.

There is nothing to click when idle, so options are command-line switches (silent when they work):

```text
dvsnip.exe                       take a snip now (what the Start menu shortcut does)
dvsnip.exe --autosave on|off     also save every snip to Pictures\Screenshots as "Screenshot 2026-10-10 093614.png"
                                 (default off, like the clipboard-only behaviour you asked for)
dvsnip.exe --prtsc on|off        whether Print Screen starts a snip; off hands the key back to Windows
dvsnip.exe --quit                stop the background process
```

## How Print Screen reaches dvsnip

Windows 11 keeps Print Screen for its own screen capture unless *Settings > Accessibility > Keyboard > Use the Print
screen key to open screen capture* is off (`HKCU\Control Panel\Keyboard\PrintScreenKeyForSnippingEnabled`). While it is
on, a registered hotkey "succeeds" but is never delivered.

- Installed: dvsnip turns that setting off, keeping the old value to put back on uninstall, and registers Print Screen
  as an ordinary hotkey. No code runs until the key is pressed.
- While Windows still has the setting on (for example when run without installing), dvsnip uses a low-level keyboard
  hook on its own thread instead, which sees each key press and swallows only a plain Print Screen.
- Explorer applies the setting change a moment after it is written and drops a hotkey registered in between, so dvsnip
  registers once more 1.5 seconds later (and again whenever Explorer restarts).

dvsnip also registers itself for the `ms-screenclip:` and `ms-screensketch:` links, so it can be picked under
*Settings > Apps > Default apps > Choose defaults by link type*. Whether Windows routes Win+Shift+S through that choice
has not been verified.

## For AI agents and scripts: dvsnip-cli

`dvsnip-cli.exe` is a separate console program: a capture is one screen copy (or one `PrintWindow`, which also gets
windows that are covered), a PNG encode, and a write.

```text
dvsnip-cli capture [--monitor N | --window "title part" | --hwnd H | --region X,Y,W,H | --foreground]
                   [--max PX] [--out FILE | --out -] [--copy] [--delay S] [--json]
dvsnip-cli snip [--mode rectangle|window|freeform] ...   the user selects; Esc exits with code 2
dvsnip-cli windows | monitors                             JSON
dvsnip-cli mcp                                            MCP server on stdin/stdout
```

To give Claude Code eyes on the desktop:

```powershell
claude mcp add dvsnip -- dvsnip-cli mcp
```

The server offers `screenshot` (desktop, monitor, window by title or handle, or region; shrunk to 1568 px on the long side
by default), `list_windows`, `list_monitors` and `user_snip` (the user picks the area with the dvsnip overlay). Each
image comes with a JSON line giving the captured rectangle and the scale, so a point in the image maps back to the
desktop as `x + image_x / scale`. Coordinates are physical pixels.

## Numbers

Measured on this machine (Windows 11, one 1920x1080 monitor at 125%), release build, median of several runs:

| | dvsnip |
|---|---:|
| program size | 0.8 MB (dvsnip.exe), 0.7 MB (dvsnip-cli.exe) |
| idle: working set / committed | 0.1 MB / 1.3 MB |
| idle CPU over 10 s | 0 ms |
| Print Screen until the screen is dimmed | ~20-40 ms (the dim layer itself is up in ~5 ms) |
| Print Screen until the frozen overlay is ready | ~60-70 ms (the screen copy alone is ~35-40 ms) |
| while the overlay is open (1080p) | 9.3 MB private (one copy of the screen) |
| after a snip | 0.4 MB working set; ~4.7 MB committed while the snip is on the clipboard |
| PNG of a 700x450 snip | ~5 ms, 46 KB |
| `dvsnip-cli capture` of the desktop | ~120 ms including process start; 2 MB committed |

The "until" times include starting the key-sending test tool, so they are slightly high. The Store Snipping Tool was not
benchmarked side by side. Floor: the screen copy (a GDI `BitBlt` of the desktop) costs ~35-40 ms and 8 MB here and is
done by Windows' compositor, not by dvsnip; a faster route (DXGI desktop duplication) needs Direct3D loaded, which costs
more memory than it saves.

## Performance design

- A plain half-transparent black window covers the screen within ~5 ms of the key press. It is excluded from screen
  capture (`WDA_EXCLUDEFROMCAPTURE`), so the single screen copy taken right behind it is clean, and it looks exactly like
  the dimmed picture that replaces it.
- The screen is copied once, on its own thread, while the main thread builds the overlay window (which is shown cloaked
  during the copy, so only the first paint happens afterwards). Only that one copy stays in memory: each paint dims just
  the changed rows through a 64-row strip buffer instead of keeping a second, dimmed copy of the whole screen.
- The bitmap goes on the clipboard at once. The PNG is encoded by dvsnip's own encoder (`png.rs`: LZ77 with a hash probe,
  the fixed Huffman code, per-row filter choice, written from scratch) on a worker thread and then added to the
  clipboard if nothing else was copied meanwhile. No image library, and not Windows' imaging component, is loaded.
- When the snip is done the screen copy is freed and the working set is handed back to Windows.
- Nothing is drawn with Direct2D, DirectWrite or COM; `shell32` is delay-loaded. The toolbar icons and the app icon are
  drawn by code (`glyphs.rs`, `icon_art.rs`): a few lines rasterized with antialiasing, no icon font or image files.
- No timers run while idle (one 1.5 s timer fires after the hotkey is registered). Build: static CRT, link-time
  optimization, one codegen unit, `opt-level = 3`.

What cannot be written in plain Rust: capturing the screen, seeing a key press and showing a window are things only
Windows can do, so they are Win32 calls (`BitBlt`, `RegisterHotKey`, `SetWindowsHookEx`, `CreateWindowEx`). Rust's
standard library has no access to any of them, and there is no stable way to call the kernel without going through
those DLLs. Everything that Windows merely *offered* (image codecs, notification area, dialogs, COM) has been replaced
or removed.

## Source layout

| file | role |
|---|---|
| `resident.rs`, `hook.rs` | the invisible background process: Print Screen hotkey or keyboard hook, clipboard, single instance |
| `overlay.rs`, `capture.rs`, `glyphs.rs` | the selection overlay, screen and window capture, toolbar icons |
| `image.rs`, `png.rs` | pixel buffers (crop, freeform mask, downscaling, clipboard DIB), the PNG encoder |
| `cli.rs`, `mcp.rs`, `json.rs`, `bin/dvsnip-cli.rs` | the console tool and its MCP server |
| `settings.rs`, `install.rs`, `installer/` | settings file, per-user install, the setup program |

`cargo test` runs the unit tests (pixel operations, PNG round trips, JSON, the MCP protocol, the icons).
`DVSNIP_SHEET=out.png cargo test contact_sheet -- --ignored` draws every icon to one image;
`DVSNIP_BENCH=1 cargo test --release bench_ -- --ignored --nocapture` times the PNG encoder on your screen. Set
`DVSNIP_LOG` to a file path to log timings.

## Limits

- Tested on one monitor. Several monitors with different scaling should work (everything is in physical pixels), but
  has not been tried here.
- No video, OCR, delay timer or markup; the Store Snipping Tool keeps those.
- No tray icon, so changing an option means a command-line switch (see above), and the process is stopped with `--quit`.
- Requires Windows 10 2004 or later for the instant dim layer; older versions copy the screen before showing anything.
- HDR displays are captured as Windows hands them to GDI.

## Notes

- Settings live in `%APPDATA%\dvsnip\settings.txt`.
- `.cargo/config.toml` points the build output outside the project folder; change `target-dir` to suit your machine.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE).
