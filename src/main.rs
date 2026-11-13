use std::io::{IsTerminal, Write};

use acusage::app::{self, Env, UsageError};
use acusage::cli::Cli;
use acusage::render::{self, RenderOpts};
use clap::Parser;

fn main() {
    // clap exits with 2 on bad arguments and 0 on --help/--version.
    let cli = Cli::parse();
    match app::run(&cli, &Env::from_process()) {
        Ok(out) => {
            let tty = std::io::stdout().is_terminal();
            let opts = RenderOpts {
                json: cli.opts.json,
                utc: cli.opts.utc,
                no_cost: cli.opts.no_cost,
                color: render::use_color(
                    cli.opts.color,
                    cli.opts.no_color,
                    std::env::var_os("NO_COLOR").is_some(),
                    tty,
                ),
                compact: cli.opts.compact,
                full: cli.opts.full,
                width: tty.then(terminal_width),
            };
            let text = render::render(&out, &opts);
            // A closed pipe (e.g. `| head`) is not an error worth a message.
            let _ = std::io::stdout().write_all(text.as_bytes());
            if cli.opts.verbose {
                for s in &out.stats {
                    eprintln!(
                        "acusage: {}: {} files, {} unreadable, {} skipped lines, records {} -> {} after dedup, {} ms",
                        s.source,
                        s.files,
                        s.unreadable_files,
                        s.skipped_lines,
                        s.records_before_dedup,
                        s.records_after_dedup,
                        s.elapsed_ms
                    );
                }
                for a in &out.attribution {
                    eprintln!(
                        "acusage: {}: records by account source: owner {}, profile {}, unknown {}",
                        a.tool, a.owner, a.profile, a.unknown
                    );
                }
                eprintln!(
                    "acusage: skipped {} bad lines, {} unreadable files",
                    out.skipped_lines, out.unreadable_files
                );
            }
        }
        Err(e) => {
            eprintln!("acusage: {e:#}");
            let code = if e.downcast_ref::<UsageError>().is_some() {
                2
            } else {
                1
            };
            std::process::exit(code);
        }
    }
}

/// Columns of the terminal: `$COLUMNS`, else `stty size` on the tty, else 120.
fn terminal_width() -> usize {
    if let Some(n) = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    std::fs::File::open("/dev/tty")
        .ok()
        .and_then(|tty| {
            std::process::Command::new("stty")
                .arg("size")
                .stdin(tty)
                .output()
                .ok()
        })
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(120)
}
