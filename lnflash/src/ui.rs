//! Talking to the person running the tool.
//!
//! A trait rather than direct `println!` for two reasons. The obvious one is
//! that [`flow`](crate::flow) becomes testable. The other is that this tool
//! overwrites other people's devices as root, so "did the user agree?" is a
//! decision with exactly one implementation for interactive use and one for
//! `--yes`, and neither should be reachable by accident.

use std::io::{self, BufRead, Write};

pub trait Ui {
    /// Something the user should read.
    fn say(&mut self, line: &str);

    /// Ask before doing something irreversible. `false` means don't.
    fn confirm(&mut self, question: &str) -> io::Result<bool>;

    /// Ask the user to do something physical and wait for them to say they
    /// have. There is no software trigger that works on every board, so this
    /// is a real step, not a fallback nobody hits.
    fn wait_for_human(&mut self, instruction: &str) -> io::Result<()>;
}

/// Reads stdin, writes stdout.
pub struct Console {
    quiet: bool,
}

impl Console {
    pub fn new(quiet: bool) -> Self {
        Self { quiet }
    }
}

impl Default for Console {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Ui for Console {
    fn say(&mut self, line: &str) {
        if !self.quiet {
            println!("{line}");
        }
    }

    fn confirm(&mut self, question: &str) -> io::Result<bool> {
        print!("{question} [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        // EOF — a pipe with nothing in it — is not consent.
        if io::stdin().lock().read_line(&mut answer)? == 0 {
            println!();
            return Ok(false);
        }
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }

    fn wait_for_human(&mut self, instruction: &str) -> io::Result<()> {
        print!("{instruction}\n  press Enter when done: ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        Ok(())
    }
}

/// `--yes`: says yes to everything, and refuses anything that needs hands.
///
/// The refusal is the point. A double-tap cannot be automated, so an
/// unattended run that reaches one must fail loudly rather than block
/// forever on a prompt nobody will see.
pub struct Assumed {
    quiet: bool,
}

impl Assumed {
    pub fn new(quiet: bool) -> Self {
        Self { quiet }
    }
}

impl Ui for Assumed {
    fn say(&mut self, line: &str) {
        if !self.quiet {
            println!("{line}");
        }
    }

    fn confirm(&mut self, question: &str) -> io::Result<bool> {
        self.say(&format!("{question} yes (--yes)"));
        Ok(true)
    }

    fn wait_for_human(&mut self, instruction: &str) -> io::Result<()> {
        Err(io::Error::other(format!(
            "this board needs a step that cannot be automated, and --yes was given:\n{instruction}"
        )))
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Scripted answers, and a transcript of everything said.
    #[derive(Default)]
    pub struct Fake {
        pub said: Vec<String>,
        pub answers: Vec<bool>,
        pub human_steps: Vec<String>,
        pub human_refuses: bool,
    }

    impl Fake {
        pub fn agreeing() -> Self {
            Self {
                answers: vec![true; 8],
                ..Default::default()
            }
        }

        pub fn refusing() -> Self {
            Self {
                answers: vec![false; 8],
                ..Default::default()
            }
        }

        pub fn transcript(&self) -> String {
            self.said.join("\n")
        }
    }

    impl Ui for Fake {
        fn say(&mut self, line: &str) {
            self.said.push(line.to_string());
        }

        fn confirm(&mut self, question: &str) -> io::Result<bool> {
            self.said.push(question.to_string());
            Ok(if self.answers.is_empty() {
                false
            } else {
                self.answers.remove(0)
            })
        }

        fn wait_for_human(&mut self, instruction: &str) -> io::Result<()> {
            self.human_steps.push(instruction.to_string());
            if self.human_refuses {
                Err(io::Error::other("no human"))
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Fake;
    use super::*;

    #[test]
    fn yes_mode_agrees_but_will_not_pretend_a_human_pressed_reset() {
        let mut ui = Assumed::new(true);
        assert!(ui.confirm("write?").unwrap());
        let err = ui.wait_for_human("double-tap RESET").unwrap_err();
        assert!(format!("{err}").contains("cannot be automated"));
    }

    #[test]
    fn the_fake_records_what_the_user_was_told_before_being_asked() {
        let mut ui = Fake::refusing();
        ui.say("found: T114 in bootloader");
        assert!(!ui.confirm("write?").unwrap());
        assert!(ui.transcript().contains("found: T114"));
    }
}
