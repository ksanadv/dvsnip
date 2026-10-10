// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// `dvsnip-cli mcp`: a Model Context Protocol server on stdin/stdout (newline-delimited JSON-RPC 2.0), so an AI
// client can look at the desktop. stdout carries protocol messages only; diagnostics go to stderr.
use crate::cli::{self, Target, DEFAULT_MAX};
use crate::image;
use crate::json::{self, obj, Value};
use crate::png;
use std::io::{BufRead, Write};

const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

fn tools() -> Value {
    let int = |d: &str| obj(&[("type", "integer".into()), ("description", d.into())]);
    let string = |d: &str| obj(&[("type", "string".into()), ("description", d.into())]);
    let max = int("Shrink so the longer side is at most this many pixels (default 1568; 0 keeps full size).");
    let tool = |name: &str, desc: &str, props: Vec<(&str, Value)>| {
        obj(&[
            ("name", name.into()),
            ("description", desc.into()),
            ("inputSchema", obj(&[("type", "object".into()), ("properties", obj(&props))])),
        ])
    };
    Value::Arr(vec![
        tool(
            "screenshot",
            "Capture the user's Windows desktop: the whole desktop (default), one monitor, one window (even if \
             covered by others) or a region. Returns the image and a JSON line with the captured rectangle in \
             physical desktop pixels and the scale, so a point in the image maps back as \
             desktop_x = x + image_x / scale.",
            vec![
                ("monitor", int("Monitor index from list_monitors (0 is the primary monitor).")),
                ("window", string("Part of the window title, case-insensitive. The frontmost match is used.")),
                ("hwnd", int("Exact window handle from list_windows.")),
                (
                    "foreground",
                    obj(&[("type", "boolean".into()), ("description", "Capture the active window.".into())]),
                ),
                (
                    "region",
                    obj(&[
                        ("type", "object".into()),
                        ("description", "Desktop rectangle in physical pixels.".into()),
                        (
                            "properties",
                            obj(&[
                                ("x", int("left")),
                                ("y", int("top")),
                                ("width", int("width")),
                                ("height", int("height")),
                            ]),
                        ),
                        ("required", Value::Arr(vec!["x".into(), "y".into(), "width".into(), "height".into()])),
                    ]),
                ),
                ("max_size", max.clone()),
                ("save_path", string("Also write the full-size capture to this PNG file.")),
            ],
        ),
        tool(
            "list_windows",
            "Visible top-level windows, front to back: title, process, hwnd and frame rectangle.",
            vec![],
        ),
        tool("list_monitors", "Monitors with their desktop rectangles and display scale.", vec![]),
        tool(
            "user_snip",
            "Ask the user to select part of the screen with the dvsnip overlay and return it. Waits until the user \
             finishes; pressing Esc cancels and returns an error. Use when the user should point at something.",
            vec![
                (
                    "mode",
                    obj(&[
                        ("type", "string".into()),
                        ("enum", Value::Arr(vec!["rectangle".into(), "window".into(), "freeform".into()])),
                    ]),
                ),
                ("max_size", max),
            ],
        ),
    ])
}

fn text(s: String) -> Value {
    obj(&[("type", "text".into()), ("text", s.into())])
}

fn tool_error(msg: String) -> Value {
    obj(&[("content", Value::Arr(vec![text(msg)])), ("isError", true.into())])
}

fn image_result(c: cli::Capture, args: &Value) -> Value {
    let max = args.get("max_size").and_then(Value::int).map_or(DEFAULT_MAX, |n| n.clamp(0, 16384) as u32);
    let mut saved = None;
    if let Some(p) = args.get("save_path").and_then(Value::str) {
        match png::save(&c.img, std::path::Path::new(p)) {
            Ok(()) => saved = Some(p.to_owned()),
            Err(e) => return tool_error(format!("cannot write {p}: {e}")),
        }
    }
    let sent = image::fit_within(&c.img, max);
    let bytes = png::encode(&sent);
    let info = cli::describe(&c, &sent, saved.as_deref());
    obj(&[(
        "content",
        Value::Arr(vec![
            obj(&[("type", "image".into()), ("data", json::base64(&bytes).into()), ("mimeType", "image/png".into())]),
            text(info.to_json()),
        ]),
    )])
}

fn call(params: &Value) -> Value {
    let empty = Value::Obj(Vec::new());
    let args = params.get("arguments").unwrap_or(&empty);
    match params.get("name").and_then(Value::str).unwrap_or("") {
        "screenshot" => {
            let target = if let Some(r) = args.get("region") {
                let n = |k: &str| r.get(k).and_then(Value::int).unwrap_or(0) as i32;
                let (x, y, w, h) = (n("x"), n("y"), n("width"), n("height"));
                if w <= 0 || h <= 0 {
                    return tool_error("region needs a positive width and height".into());
                }
                Target::Region(crate::image::Rect::new(x, y, x + w, y + h))
            } else if let Some(h) = args.get("hwnd").and_then(Value::int) {
                Target::Hwnd(h as isize)
            } else if let Some(t) = args.get("window").and_then(Value::str) {
                Target::Window(t.to_owned())
            } else if args.get("foreground").and_then(Value::bool) == Some(true) {
                Target::Foreground
            } else if let Some(m) = args.get("monitor").and_then(Value::int) {
                Target::Monitor(m.max(0) as usize)
            } else {
                Target::Screen
            };
            match cli::take(&target) {
                Ok(c) => image_result(c, args),
                Err(e) => tool_error(e),
            }
        }
        "list_windows" => obj(&[("content", Value::Arr(vec![text(cli::windows_json().to_json())]))]),
        "list_monitors" => obj(&[("content", Value::Arr(vec![text(cli::monitors_json().to_json())]))]),
        "user_snip" => {
            let mode = cli::parse_mode(args.get("mode").and_then(Value::str).unwrap_or("rectangle"));
            match cli::user_snip(mode) {
                Ok(c) => image_result(c, args),
                Err(e) => tool_error(e),
            }
        }
        other => tool_error(format!("unknown tool {other}")),
    }
}

/// The reply to one request, or None for a notification.
pub fn handle(req: &Value) -> Option<Value> {
    let id = req.get("id")?.clone();
    let method = req.get("method").and_then(Value::str).unwrap_or("");
    let empty = Value::Obj(Vec::new());
    let params = req.get("params").unwrap_or(&empty);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::str).unwrap_or("");
            let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
            obj(&[
                ("protocolVersion", version.into()),
                ("capabilities", obj(&[("tools", obj(&[]))])),
                ("serverInfo", obj(&[("name", "dvsnip".into()), ("version", env!("CARGO_PKG_VERSION").into())])),
                (
                    "instructions",
                    "Screenshots of the user's Windows desktop. Coordinates are physical pixels of the virtual \
                     desktop; list_windows and list_monitors describe what is on screen."
                        .into(),
                ),
            ])
        }
        "ping" => obj(&[]),
        "tools/list" => obj(&[("tools", tools())]),
        "tools/call" => call(params),
        _ => {
            return Some(obj(&[
                ("jsonrpc", "2.0".into()),
                ("id", id),
                ("error", obj(&[("code", (-32601).into()), ("message", format!("method not found: {method}").into())])),
            ]))
        }
    };
    Some(obj(&[("jsonrpc", "2.0".into()), ("id", id), ("result", result)]))
}

pub fn handle_line(line: &str) -> Option<String> {
    // some writers (PowerShell among them) start the stream with a byte order mark
    match json::parse(line.trim_start_matches('\u{feff}')) {
        Ok(req) => handle(&req).map(|v| v.to_json()),
        Err(e) => Some(
            obj(&[
                ("jsonrpc", "2.0".into()),
                ("id", Value::Null),
                ("error", obj(&[("code", (-32700).into()), ("message", format!("parse error: {e}").into())])),
            ])
            .to_json(),
        ),
    }
}

pub fn serve() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(&line) {
            if writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err() {
                break;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(line: &str) -> Value {
        json::parse(&handle_line(line).unwrap()).unwrap()
    }

    #[test]
    fn initialize_lists_tools_and_ignores_notifications() {
        let r = reply(
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        );
        assert_eq!(r.get("id"), Some(&Value::Num(0.0)));
        let res = r.get("result").unwrap();
        assert_eq!(res.get("protocolVersion").and_then(Value::str), Some("2025-06-18"));
        assert!(res.get("capabilities").and_then(|c| c.get("tools")).is_some());
        assert!(handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let t = reply(r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#);
        assert_eq!(t.get("id").and_then(Value::str), Some("a"));
        let Some(Value::Arr(tools)) = t.get("result").and_then(|r| r.get("tools")) else { panic!() };
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get("name").and_then(Value::str)).collect();
        assert_eq!(names, ["screenshot", "list_windows", "list_monitors", "user_snip"]);
    }

    #[test]
    fn unknown_versions_methods_and_bad_json() {
        let r = reply(r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#);
        assert_eq!(r.get("result").and_then(|r| r.get("protocolVersion")).and_then(Value::str), Some(VERSIONS[0]));
        let e = reply(r#"{"jsonrpc":"2.0","id":8,"method":"resources/list"}"#);
        assert_eq!(e.get("error").and_then(|e| e.get("code")).and_then(Value::int), Some(-32601));
        let bom = reply("\u{feff}{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}");
        assert!(bom.get("result").is_some());
        let p = reply("{nope");
        assert_eq!(p.get("error").and_then(|e| e.get("code")).and_then(Value::int), Some(-32700));
        let bad = reply(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"screenshot","arguments":{"region":{"x":0,"y":0,"width":0,"height":5}}}}"#,
        );
        assert_eq!(bad.get("result").and_then(|r| r.get("isError")), Some(&Value::Bool(true)));
    }
}
