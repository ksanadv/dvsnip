// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
#![windows_subsystem = "windows"]

use dvsnip::settings::Settings;
use dvsnip::{install, resident, sys};

const HELP: &str = "dvsnip: Print Screen, select an area, paste it.

  dvsnip.exe                     take a snip now (starts the background process if it is not running)
  dvsnip.exe --background        start the background process without snipping
  dvsnip.exe --autosave on|off   also save every snip to Pictures\\Screenshots (default: off)
  dvsnip.exe --prtsc on|off      whether Print Screen starts a snip (default: on)
  dvsnip.exe --quit              stop the background process
  dvsnip.exe --install           install for this user (starts with Windows, takes Print Screen)
  dvsnip.exe --uninstall         remove it and give Print Screen back to Windows";

fn cli(result: Result<String, String>) -> ! {
    match result {
        Ok(text) => {
            sys::message(&text, false);
            std::process::exit(0)
        }
        Err(e) => {
            sys::message(&e, true);
            std::process::exit(1)
        }
    }
}

fn on_off(v: Option<&String>) -> Result<bool, String> {
    match v.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("on" | "1" | "true") => Ok(true),
        Some("off" | "0" | "false") => Ok(false),
        _ => Err(format!("This switch takes on or off.\n\n{HELP}")),
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        sys::show_fatal(&format!("dvsnip hit an internal error and must close.\n\n{info}"));
    }));
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--quiet").collect();
    match args.first().map(String::as_str) {
        Some("--install") => cli(install::install().map(|exe| {
            install::quit_resident();
            let _ = std::process::Command::new(&exe).arg("--background").spawn();
            format!(
                "dvsnip is installed ({}).\n\nPrint Screen now takes a snip and copies it to the clipboard.",
                exe.display()
            )
        })),
        Some("--uninstall") => cli(install::uninstall().map(|()| "dvsnip has been uninstalled.".to_owned())),
        Some("--help" | "-h" | "/?") => cli(Ok(HELP.to_owned())),
        Some(switch @ ("--autosave" | "--prtsc")) => {
            // a switch is meant for scripts: silent when it works, a message only when it does not
            let result = on_off(args.get(1)).map(|on| {
                let mut s = Settings::load();
                if switch == "--autosave" {
                    s.auto_save = on;
                } else {
                    s.take_prtsc = on;
                    // the Windows setting follows, so Print Screen goes back to Windows when dvsnip lets go of it
                    if on && install::is_installed() {
                        install::take_print_screen();
                    } else if !on {
                        install::restore_print_screen();
                    }
                }
                s.save();
                install::send_to_resident("--reload");
            });
            match result {
                Ok(()) => std::process::exit(0),
                Err(e) => cli(Err(e)),
            }
        }
        _ => {}
    }
    resident::run(args.first().map(String::as_str));
}
