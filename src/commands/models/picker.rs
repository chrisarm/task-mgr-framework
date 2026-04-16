//! Numbered interactive model picker.
//!
//! Pure in the sense that it takes `BufRead` + `Write`, not stdin/stdout
//! directly — tests feed canned input without touching the process TTY.

use std::io::{self, BufRead, Write};

/// One choice shown in the picker. Kept separate from `api::RemoteModel` so
/// callers can build a list from either the live API or the hardcoded
/// constants without double-representing.
#[derive(Debug, Clone)]
pub struct ModelChoice {
    /// Stable model id (what gets persisted).
    pub id: String,
    /// Tier label (`"Opus"`, `"Sonnet"`, `"Haiku"`, or `""`).
    pub tier: String,
    /// Optional creation date / display hint (shown in parens after id).
    pub note: Option<String>,
}

/// Maximum number of invalid inputs before the picker gives up and returns
/// `None`. Keeps the prompt from blocking forever on bad pipes.
const MAX_RETRIES: u32 = 3;

/// Render the list and read a choice. Returns:
/// - `Ok(Some(id))` when the user picks a valid number.
/// - `Ok(None)` when the user submits a blank line, hits EOF, or exhausts retries.
pub fn select_model_interactive<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    choices: &[ModelChoice],
) -> io::Result<Option<String>> {
    if choices.is_empty() {
        writeln!(
            writer,
            "(no models available; set ANTHROPIC_API_KEY + TASK_MGR_USE_API=1 for live list)"
        )?;
        return Ok(None);
    }

    writeln!(writer, "Available Claude models:")?;
    writeln!(writer)?;
    for (i, choice) in choices.iter().enumerate() {
        let tier = if choice.tier.is_empty() {
            String::new()
        } else {
            format!("  [{}]", choice.tier)
        };
        let note = choice
            .note
            .as_ref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        writeln!(writer, "  {:>2}. {}{}{}", i + 1, choice.id, note, tier)?;
    }
    writeln!(writer)?;

    for _ in 0..MAX_RETRIES {
        write!(
            writer,
            "Pick a number 1-{} (or press Enter to skip): ",
            choices.len()
        )?;
        writer.flush()?;

        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            // EOF
            writeln!(writer, "(skipped)")?;
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            writeln!(writer, "(skipped)")?;
            return Ok(None);
        }
        match trimmed.parse::<usize>() {
            Ok(n) if (1..=choices.len()).contains(&n) => {
                return Ok(Some(choices[n - 1].id.clone()));
            }
            _ => {
                writeln!(
                    writer,
                    "(not a valid choice; enter a number 1-{} or Enter to skip)",
                    choices.len()
                )?;
            }
        }
    }
    writeln!(writer, "(too many invalid inputs; skipping)")?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
    use std::io::Cursor;

    fn choices() -> Vec<ModelChoice> {
        vec![
            ModelChoice {
                id: OPUS_MODEL.to_string(),
                tier: "Opus".to_string(),
                note: None,
            },
            ModelChoice {
                id: SONNET_MODEL.to_string(),
                tier: "Sonnet".to_string(),
                note: None,
            },
            ModelChoice {
                id: HAIKU_MODEL.to_string(),
                tier: "Haiku".to_string(),
                note: Some("2025-10-01".to_string()),
            },
        ]
    }

    fn run(input: &str) -> (Option<String>, String) {
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        let picked = select_model_interactive(reader, &mut output, &choices()).unwrap();
        (picked, String::from_utf8(output).unwrap())
    }

    #[test]
    fn valid_number_picks_model() {
        let (picked, _) = run("2\n");
        assert_eq!(picked, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn first_and_last_work() {
        assert_eq!(run("1\n").0, Some(OPUS_MODEL.to_string()));
        assert_eq!(run("3\n").0, Some(HAIKU_MODEL.to_string()));
    }

    #[test]
    fn blank_line_skips() {
        let (picked, _) = run("\n");
        assert_eq!(picked, None);
    }

    #[test]
    fn eof_skips() {
        let (picked, _) = run("");
        assert_eq!(picked, None);
    }

    #[test]
    fn invalid_then_valid_succeeds() {
        let (picked, out) = run("banana\n2\n");
        assert_eq!(picked, Some(SONNET_MODEL.to_string()));
        assert!(out.contains("not a valid choice"));
    }

    #[test]
    fn out_of_range_then_valid() {
        let (picked, _) = run("99\n1\n");
        assert_eq!(picked, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn zero_is_invalid() {
        let (picked, out) = run("0\n1\n");
        assert_eq!(picked, Some(OPUS_MODEL.to_string()));
        assert!(out.contains("not a valid choice"));
    }

    #[test]
    fn three_invalid_gives_up() {
        let (picked, out) = run("x\ny\nz\n");
        assert_eq!(picked, None);
        assert!(out.contains("too many invalid inputs"));
    }

    #[test]
    fn empty_choices_returns_none() {
        let reader = Cursor::new(b"1\n".to_vec());
        let mut output = Vec::new();
        let picked = select_model_interactive(reader, &mut output, &[]).unwrap();
        assert_eq!(picked, None);
        let out = String::from_utf8(output).unwrap();
        assert!(out.contains("no models available"));
    }

    #[test]
    fn displays_tiers_and_notes() {
        let (_, out) = run("\n");
        assert!(out.contains(OPUS_MODEL));
        assert!(out.contains("[Opus]"));
        assert!(out.contains("(2025-10-01)"));
    }
}
