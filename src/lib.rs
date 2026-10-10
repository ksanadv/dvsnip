// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// Shared by the two programs: dvsnip.exe (background process and overlay) and dvsnip-cli.exe (console and MCP server).

pub mod capture;
pub mod cli;
pub mod glyphs;
pub mod hook;
#[cfg(test)]
#[allow(dead_code)]
mod icon_art;
pub mod image;
pub mod install;
pub mod json;
pub mod mcp;
pub mod overlay;
pub mod png;
pub mod resident;
pub mod settings;
pub mod sys;
