use anyhow::{Result, bail};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
};
use std::{
    io::{self, Write},
    path::Path,
    process::Command,
};
pub fn suspend() {
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
}
pub fn resume(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    *terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    terminal.clear()?;
    Ok(())
}
pub fn launch(path: &Path, edit: bool, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    anyhow::ensure!(
        std::fs::metadata(path)?.is_file(),
        "Only regular files can be viewed or edited"
    );
    suspend();
    let result = (|| -> Result<()> {
        let mut command = if edit {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| {
                    if cfg!(windows) {
                        "notepad".into()
                    } else {
                        "vi".into()
                    }
                });
            let args = shell_words::split(&editor)?;
            if args.is_empty() {
                bail!("Empty editor command");
            }
            let mut c = Command::new(&args[0]);
            c.args(&args[1..]);
            c
        } else {
            let mut c = Command::new("cat");
            c.arg("--");
            c
        };
        let status = command.arg(path).status()?;
        if !edit {
            print!("\nPress Enter to return to mc…");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
        }
        if !status.success() {
            bail!("External program exited with {status}");
        }
        Ok(())
    })();
    resume(terminal)?;
    result
}

/// Virtual files are streamed into cat's stdin; no plaintext temporary file is created.
pub fn cat_stream(
    mut reader: Box<dyn io::Read + Send>,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<()> {
    suspend();
    let result = (|| -> Result<()> {
        let mut child = Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        let copied = io::copy(&mut reader, &mut stdin);
        drop(stdin);
        let status = child.wait()?;
        print!("\nPress Enter to return to mc…");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        copied?;
        anyhow::ensure!(status.success(), "cat exited with {status}");
        Ok(())
    })();
    resume(terminal)?;
    result
}
