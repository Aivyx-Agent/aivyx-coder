use std::io::{self, Stdout};

use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// RAII guard around raw-mode + the alternate screen. A coding agent that
/// panics mid-tool-execution must never strand the user's shell in raw/alt
/// mode, so restoration happens both here (`Drop`, for the ordinary exit
/// path) and in the panic hook installed by `init` (for the panicking path,
/// which never reaches `Drop` in time to matter to the user before the
/// panic message prints).
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        // If anything below fails, raw mode is already on and possibly the
        // alternate screen too, but no `TerminalGuard` exists yet for `Drop`
        // to clean it up — restore both ourselves before propagating the
        // error. `restore_terminal` is safe to call even if
        // `EnterAlternateScreen` never actually ran: emitting
        // `LeaveAlternateScreen` when not in the alternate screen is a
        // harmless no-op on real terminals.
        match Self::init_after_raw_mode() {
            Ok(guard) => Ok(guard),
            Err(err) => {
                let _ = restore_terminal();
                Err(err)
            }
        }
    }

    fn init_after_raw_mode() -> io::Result<Self> {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        install_panic_hook();
        Ok(Self { terminal })
    }

    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

fn restore_terminal() -> io::Result<()> {
    // Both steps are attempted independently — a failure in one must not
    // skip the other, since they restore unrelated pieces of terminal
    // state (raw mode vs. the alternate screen buffer).
    let raw_mode_result = disable_raw_mode();
    let alt_screen_result = execute!(io::stdout(), LeaveAlternateScreen);
    raw_mode_result?;
    alt_screen_result?;
    Ok(())
}

fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        original_hook(panic_info);
    }));
}
