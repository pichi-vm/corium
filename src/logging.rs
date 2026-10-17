// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{self, Write as _};
use std::io::Write as _;

use log::kv::{Key, Value, VisitSource};
use log::{LevelFilter, Log, Metadata, Record};

static CONSOLE: Console = Console;

struct Console;

/// Install the synchronous console logger with the bootstrap error filter.
/// Call once at process entry, before boot can fail.
///
/// # Panics
/// Panics if a global logger has already been installed.
pub fn init_logging() {
    log::set_logger(&CONSOLE).expect("Corium installs its logger once");
    log::set_max_level(LevelFilter::Error);
}

impl Log for Console {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata())
            && let Ok(line) = format_record(record)
        {
            // No persistent descriptor: stderr is the CLOEXEC console.
            // Logging failures must not panic PID 1 or recursively log.
            let _ = std::io::stderr().lock().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().lock().flush();
    }
}

fn format_record(record: &Record<'_>) -> Result<String, fmt::Error> {
    let mut line = format!("{} {}: message=", record.level(), record.target());
    quoted(&mut line, &record.args().to_string())?;
    record
        .key_values()
        .visit(&mut Fields(&mut line))
        .map_err(|_| fmt::Error)?;
    line.push('\n');
    Ok(line)
}

struct Fields<'a>(&'a mut String);

impl<'kvs> VisitSource<'kvs> for Fields<'_> {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), log::kv::Error> {
        write!(self.0, " {key}=")?;
        if let Some(number) = value.to_i128() {
            write!(self.0, "{number}")?;
        } else if let Some(number) = value.to_u128() {
            write!(self.0, "{number}")?;
        } else if let Some(number) = value.to_f64().filter(|n| n.is_finite()) {
            write!(self.0, "{number}")?;
        } else if let Some(boolean) = value.to_bool() {
            write!(self.0, "{boolean}")?;
        } else {
            quoted(self.0, &value.to_string())?;
        }
        Ok(())
    }
}

/// JSON-style string escaping, without adding a JSON serialization dependency.
fn quoted(out: &mut String, value: &str) -> fmt::Result {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if ch.is_control() => write!(out, "\\u{:04x}", u32::from(ch))?,
            ch => out.push(ch),
        }
    }
    out.push('"');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;

    #[test]
    fn formats_all_supported_field_types() {
        let fields = [
            ("boolean", Value::from(true)),
            ("float", Value::from(1.5_f64)),
            ("large", Value::from(u128::MAX)),
            ("nan", Value::from(f64::NAN)),
            ("infinity", Value::from(f64::INFINITY)),
        ];
        let line = format_record(
            &Record::builder()
                .level(Level::Info)
                .target("corium")
                .args(format_args!("types"))
                .key_values(&fields)
                .build(),
        )
        .unwrap();
        assert_eq!(
            line,
            concat!(
                "INFO corium: message=\"types\" boolean=true float=1.5 ",
                "large=340282366920938463463374607431768211455 ",
                "nan=\"NaN\" infinity=\"inf\"\n"
            )
        );
    }

    #[test]
    fn escapes_remaining_controls() {
        let mut output = String::new();
        quoted(&mut output, "\u{08}\u{0c}\u{7f}").unwrap();
        assert_eq!(output, "\"\\b\\f\\u007f\"");
    }

    #[test]
    fn stable_format_preserves_numeric_and_string_fields() {
        let fields = [
            ("phase", Value::from("assembled")),
            ("elapsed_us", Value::from(28000_u128)),
            ("duration_us", Value::from(8000_u128)),
        ];
        let line = format_record(
            &Record::builder()
                .level(Level::Info)
                .target("corium")
                .args(format_args!("phase completed"))
                .key_values(&fields)
                .build(),
        )
        .unwrap();
        assert_eq!(
            line,
            "INFO corium: message=\"phase completed\" phase=\"assembled\" elapsed_us=28000 duration_us=8000\n"
        );
    }

    #[test]
    fn messages_and_fields_cannot_inject_lines_or_terminal_controls() {
        let fields = [("path", Value::from("a\"\\\n\r\t\u{001b}β"))];
        let line = format_record(
            &Record::builder()
                .level(Level::Error)
                .target("corium")
                .args(format_args!("bad\n\"message\""))
                .key_values(&fields)
                .build(),
        )
        .unwrap();
        assert_eq!(
            line,
            "ERROR corium: message=\"bad\\n\\\"message\\\"\" path=\"a\\\"\\\\\\n\\r\\t\\u001bβ\"\n"
        );
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn numeric_looking_strings_remain_quoted() {
        let fields = [("path", Value::from("123"))];
        let line = format_record(
            &Record::builder()
                .level(Level::Debug)
                .target("corium::modules")
                .args(format_args!("module loaded"))
                .key_values(&fields)
                .build(),
        )
        .unwrap();
        assert!(line.ends_with(" path=\"123\"\n"));
    }
}
