use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "mc — dual-panel file manager\nUsage: mc [LEFT_DIRECTORY] [RIGHT_DIRECTORY]\nF1 help · F3 cat · F4 editor · F5 copy · F6 move · F7 mkdir · F8 delete · F9 menu · F10 quit\nArchives: ZIP, RAR, tar, 7z, gzip (read-only). Requires libarchive; cat must be on PATH."
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--version") {
        println!("mc {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    anyhow::ensure!(
        args.len() <= 2,
        "Usage: mc [LEFT_DIRECTORY] [RIGHT_DIRECTORY]"
    );
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "mc requires an interactive terminal (try --help)"
    );
    let cwd = std::env::current_dir()?;
    let path = |i| -> Result<PathBuf> {
        let p = args
            .get(i)
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone());
        let p = std::fs::canonicalize(p)?;
        anyhow::ensure!(p.is_dir(), "Expected directory: {}", p.display());
        Ok(p)
    };
    ctrlc::set_handler(|| {})?; // Let foreground children handle Ctrl+C without killing the TUI owner.
    let mut app = mc::app::App::new(path(0)?, path(1)?);
    let mut terminal = ratatui::init();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
        hook(info);
    }));
    let result = (|| {
        execute!(io::stdout(), EnableMouseCapture)?;
        app.run(&mut terminal)
    })();
    mc::external::suspend();
    result
}
