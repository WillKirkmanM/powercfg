use clap::{Parser, Subcommand};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Summarise and compare saved English-language powercfg /requests reports"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Show category totals and requester details")]
    Inspect {
        #[arg(help = "Saved English-language report (UTF-8 or UTF-16 with a byte order mark)")]
        input: PathBuf,
    },
    #[command(about = "Show requests added, cleared, or given changed reasons")]
    Compare {
        #[arg(help = "Earlier saved report")]
        before: PathBuf,
        #[arg(help = "Later saved report")]
        after: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Category {
    Display,
    System,
    Awaymode,
    Execution,
    Perfboost,
    Activelockscreen,
}

impl Category {
    const ALL: [Self; 6] = [
        Self::Display,
        Self::System,
        Self::Awaymode,
        Self::Execution,
        Self::Perfboost,
        Self::Activelockscreen,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Display => "DISPLAY",
            Self::System => "SYSTEM",
            Self::Awaymode => "AWAYMODE",
            Self::Execution => "EXECUTION",
            Self::Perfboost => "PERFBOOST",
            Self::Activelockscreen => "ACTIVELOCKSCREEN",
        }
    }

    fn heading(line: &str) -> Option<Self> {
        let name = line.strip_suffix(':')?;
        Self::ALL
            .into_iter()
            .find(|category| category.name() == name)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Requester {
    Process,
    Service,
    Driver,
}

impl fmt::Display for Requester {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Process => "PROCESS",
            Self::Service => "SERVICE",
            Self::Driver => "DRIVER",
        })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Key {
    category: Category,
    requester: Requester,
    name: String,
}

#[derive(Debug)]
struct Pending {
    key: Key,
    line: usize,
    reasons: Vec<String>,
}

type Report = BTreeMap<Key, Vec<String>>;

fn finish(pending: &mut Option<Pending>, report: &mut Report) -> Result<(), String> {
    if let Some(entry) = pending.take() {
        let reason = entry.reasons.join("\n");
        let reason = reason.trim_matches(['\r', '\n']);
        if reason.trim().is_empty() {
            return Err(format!("line {}: request has no reason", entry.line));
        }
        report.entry(entry.key).or_default().push(reason.to_owned());
    }
    Ok(())
}

fn parse(input: &str) -> Result<Report, String> {
    let mut report = Report::new();
    let mut seen = BTreeSet::new();
    let mut category = None;
    let mut empty = false;
    let mut entries = 0;
    let mut pending = None::<Pending>;

    for (index, raw) in input.trim_start_matches('\u{feff}').lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim();
        if let Some(next) = Category::heading(line) {
            finish(&mut pending, &mut report)?;
            if category.is_some() && !empty && entries == 0 {
                return Err(format!(
                    "line {line_number}: previous category has no entries or None."
                ));
            }
            if !seen.insert(next) {
                return Err(format!(
                    "line {line_number}: repeated {} category",
                    next.name()
                ));
            }
            category = Some(next);
            empty = false;
            entries = 0;
        } else if line.is_empty() {
            if let Some(entry) = pending.as_mut() {
                entry.reasons.push(String::new());
            }
        } else if line == "None." {
            if category.is_none() {
                return Err(format!(
                    "line {line_number}: None. outside a recognised category"
                ));
            }
            if empty || entries > 0 {
                return Err(format!(
                    "line {line_number}: None. must be the only entry in a category"
                ));
            }
            empty = true;
        } else if line.starts_with('[') {
            finish(&mut pending, &mut report)?;
            let category = category.ok_or_else(|| {
                format!("line {line_number}: request outside a recognised category")
            })?;
            if empty {
                return Err(format!("line {line_number}: request follows None."));
            }
            let (kind, name) = line.split_once(']').ok_or_else(|| {
                format!("line {line_number}: request is missing its closing bracket")
            })?;
            let requester = match kind {
                "[PROCESS" => Requester::Process,
                "[SERVICE" => Requester::Service,
                "[DRIVER" => Requester::Driver,
                _ => {
                    return Err(format!(
                        "line {line_number}: unrecognised requester type {kind}]"
                    ));
                }
            };
            if !name.starts_with(char::is_whitespace) || name.trim().is_empty() {
                return Err(format!(
                    "line {line_number}: expected a space and requester name after ]"
                ));
            }
            pending = Some(Pending {
                key: Key {
                    category,
                    requester,
                    name: name.trim().to_owned(),
                },
                line: line_number,
                reasons: Vec::new(),
            });
            entries += 1;
        } else if line.strip_suffix(':').is_some_and(|name| {
            !name.is_empty() && name.chars().all(|character| character.is_ascii_uppercase())
        }) {
            return Err(format!("line {line_number}: unrecognised category {line}"));
        } else if let Some(entry) = pending.as_mut() {
            entry.reasons.push(raw.to_owned());
        } else {
            return Err(format!(
                "line {line_number}: unexpected text outside a request reason"
            ));
        }
    }
    finish(&mut pending, &mut report)?;
    if seen.is_empty() {
        return Err("unrecognised report: no recognised category headings".to_owned());
    }
    if !empty && entries == 0 {
        return Err("final category has no entries or None.".to_owned());
    }
    for reasons in report.values_mut() {
        reasons.sort();
    }
    Ok(report)
}

fn decode(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little_endian = bytes[0] == 0xff;
        let mut chunks = bytes[2..].chunks_exact(2);
        let words: Vec<_> = chunks
            .by_ref()
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        if !chunks.remainder().is_empty() {
            return Err("invalid UTF-16: incomplete code unit".to_owned());
        }
        String::from_utf16(&words).map_err(|error| format!("invalid UTF-16: {error}"))
    } else {
        String::from_utf8(bytes.to_vec()).map_err(|error| format!("invalid UTF-8: {error}"))
    }
}

fn read_report(path: &Path) -> Result<Report, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
    decode(&bytes)
        .and_then(|input| parse(&input))
        .map_err(|error| format!("'{}': {error}", path.display()))
}

fn show_reasons(output: &mut String, label: &str, reasons: &[String]) {
    for reason in reasons {
        let _ = writeln!(output, "    {label}");
        for line in reason.lines() {
            let _ = writeln!(output, "      {line}");
        }
    }
}

fn inspect(report: &Report) -> String {
    let mut output = String::new();
    for category in Category::ALL {
        let total: usize = report
            .iter()
            .filter(|(key, _)| key.category == category)
            .map(|(_, reasons)| reasons.len())
            .sum();
        let _ = writeln!(output, "{}: {total}", category.name());
        for (key, reasons) in report.iter().filter(|(key, _)| key.category == category) {
            let _ = writeln!(output, "  [{}] {}", key.requester, key.name);
            show_reasons(&mut output, "Reason:", reasons);
        }
    }
    output
}

fn compare(before: &Report, after: &Report) -> String {
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut details = String::new();
    let (mut added, mut cleared, mut changed) = (0, 0, 0);
    for key in keys {
        let (label, old, new) = match (before.get(key), after.get(key)) {
            (None, Some(new)) => {
                added += 1;
                ("Added", None, Some(new))
            }
            (Some(old), None) => {
                cleared += 1;
                ("Cleared", Some(old), None)
            }
            (Some(old), Some(new)) if old != new => {
                changed += 1;
                ("Changed", Some(old), Some(new))
            }
            _ => continue,
        };
        let _ = writeln!(
            details,
            "{label} {} [{}] {}",
            key.category.name(),
            key.requester,
            key.name
        );
        if let Some(reasons) = old {
            show_reasons(&mut details, "Before:", reasons);
        }
        if let Some(reasons) = new {
            show_reasons(&mut details, "After:", reasons);
        }
    }
    format!("Added: {added}; Cleared: {cleared}; Changed: {changed}\n{details}")
}

fn run(cli: Cli) -> Result<(), String> {
    let output = match cli.command {
        Command::Inspect { input } => inspect(&read_report(&input)?),
        Command::Compare { before, after } => {
            compare(&read_report(&before)?, &read_report(&after)?)
        }
    };
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(|error| format!("cannot write output: {error}"))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_names_multiline_reasons_and_empty_categories() {
        let report = parse("DISPLAY:\nNone.\n\nSYSTEM:\n[PROCESS] C:\\Tools\\backup.exe\nBackup in progress\n  Waiting for disk\n\nAWAYMODE:\nNone.\n").unwrap();
        let (key, reasons) = report.first_key_value().unwrap();
        assert_eq!(key.name, "C:\\Tools\\backup.exe");
        assert_eq!(reasons, &["Backup in progress\n  Waiting for disk"]);
        assert!(inspect(&report).starts_with("DISPLAY: 0\nSYSTEM: 1\n"));
    }

    #[test]
    fn rejects_structurally_invalid_reports() {
        for input in [
            "[PROCESS] backup.exe\nBackup in progress\n",
            "unrecognised report\n",
            "SYSTEM:\n[PROCESS] backup.exe\n",
            "SYSTEM:\nNone.\n[SERVICE] Backup\nRunning\n",
            "SYSTEM:\n[UNKNOWN] Backup\nRunning\n",
            "SYSTEM:\n[DRIVER]\nRunning\n",
            "SYSTEM:\nNone.\nSYSTEM:\nNone.\n",
            "SYSTEM:\nDISPLAY:\nNone.\n",
            "SYSTEM:\n",
        ] {
            assert!(parse(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn compares_category_type_name_and_reasons() {
        let before = parse("SYSTEM:\n[PROCESS] Backup\nOld reason\n[SERVICE] Backup\nRunning\nDISPLAY:\n[DRIVER] Screen\nActive\n").unwrap();
        let after = parse(
            "SYSTEM:\n[PROCESS] Backup\nNew reason\n[DRIVER] Backup\nRunning\nDISPLAY:\nNone.\n",
        )
        .unwrap();
        let output = compare(&before, &after);
        assert!(output.starts_with("Added: 1; Cleared: 2; Changed: 1\n"));
        assert!(output.contains("Changed SYSTEM [PROCESS] Backup\n    Before:\n      Old reason\n    After:\n      New reason\n"));
        assert!(output.contains("Cleared DISPLAY [DRIVER] Screen"));
        assert!(output.contains("Added SYSTEM [DRIVER] Backup"));
    }

    #[test]
    fn repeated_requesters_are_compared_independently_of_entry_order() {
        let first = parse("EXECUTION:\n[PROCESS] Task\nOne\n[PROCESS] Task\nTwo\n").unwrap();
        let second = parse("EXECUTION:\n[PROCESS] Task\nTwo\n[PROCESS] Task\nOne\n").unwrap();
        assert_eq!(
            compare(&first, &second),
            "Added: 0; Cleared: 0; Changed: 0\n"
        );
        assert!(inspect(&first).contains("EXECUTION: 2\n"));
    }

    #[test]
    fn decodes_windows_utf16_reports() {
        let input = "SYSTEM:\r\n[SERVICE] Backup\r\nBackup in progress\r\n";
        for little_endian in [true, false] {
            let mut bytes = if little_endian {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for word in input.encode_utf16() {
                bytes.extend(if little_endian {
                    word.to_le_bytes()
                } else {
                    word.to_be_bytes()
                });
            }
            assert_eq!(decode(&bytes).unwrap(), input);
            assert!(parse(&decode(&bytes).unwrap()).is_ok());
        }
        assert!(decode(&[0xff, 0xfe, 0x00]).is_err());
        assert!(decode(&[0xff]).is_err());
    }
}
