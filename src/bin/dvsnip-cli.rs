// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// A console program, so terminals wait for it and its stdout is a plain pipe (which MCP clients need).

fn main() {
    std::process::exit(dvsnip::cli::run(std::env::args().skip(1).collect()));
}
