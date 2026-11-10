use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use korin::compile::compile_path;
use korin::diag::Diag;
use korin::theme::{Choice, Theme};

/// A guided runbook language: every command is shown before it runs.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// When to use color: `auto` colors a terminal unless `NO_COLOR` is set.
    #[arg(long, value_enum, default_value_t = ColorArg::Auto, global = true)]
    color: ColorArg,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum ColorArg {
    Auto,
    Always,
    Never,
}

impl From<ColorArg> for Choice {
    fn from(arg: ColorArg) -> Self {
        match arg {
            ColorArg::Auto => Choice::Auto,
            ColorArg::Always => Choice::Always,
            ColorArg::Never => Choice::Never,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse and check a wizard without running anything.
    Check {
        file: PathBuf,
        /// Output format for diagnostics.
        #[arg(long, value_enum, default_value_t = Format::Human)]
        format: Format,
        /// Also check that the run can start at this stage (title or number).
        #[arg(long)]
        from: Option<String>,
    },
    /// Print every stage and command, with placeholders for values not yet known.
    Plan { file: PathBuf },
    /// Run a wizard. Each stage is shown, and approved at its gate, before it runs.
    Run {
        file: PathBuf,
        /// A value for a param or an `ask`, as `name=value`. Repeatable.
        #[arg(long = "param", value_name = "NAME=VALUE", value_parser = parse_param)]
        params: Vec<(String, String)>,
        /// Approve every gate. Allowed only without a terminal, for wizards that declare `unattended allowed`.
        #[arg(long)]
        yes: bool,
        /// Start at this stage (title or number), skipping earlier ones.
        #[arg(long)]
        from: Option<String>,
        /// Seconds between SIGTERM and SIGKILL when korin ends a command (at most 3600).
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(0..=3600))]
        kill_grace: u64,
    },
    /// Format wizard and module files in place.
    Fmt {
        files: Vec<PathBuf>,
        /// Report files that are not formatted, without changing them.
        #[arg(long)]
        check: bool,
    },
    /// List the built-in modules and their templates.
    Modules,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Human,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let choice = Choice::from(cli.color);
    let diagnostics = Theme::stderr(choice).diagnostics();
    // Diagnostics are always graphical; the theme alone decides color. `force_graphical(false)`
    // would mean "narrated", the screen-reader format, so it is never set.
    let _ = miette::set_hook(Box::new(move |_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .graphical_theme(diagnostics.clone())
                .build(),
        )
    }));
    match cli.command {
        Cmd::Check { file, format, from } => {
            check(&file, format, from.as_deref(), Theme::stderr(choice))
        }
        Cmd::Plan { file } => plan(&file, Theme::stdout(choice)),
        Cmd::Run {
            file,
            params,
            yes,
            from,
            kill_grace,
        } => run(&file, params, yes, from, kill_grace, Theme::stdout(choice)),
        Cmd::Fmt { files, check } => fmt(&files, check),
        Cmd::Modules => {
            print!("{}", korin::stdlib::listing());
            ExitCode::SUCCESS
        }
    }
}

fn report(diags: &[Diag], format: Format) {
    match format {
        Format::Human => {
            for diag in diags {
                eprintln!("{:?}", miette::Report::new(diag.clone()));
            }
        }
        Format::Json => {
            let json: Vec<_> = diags.iter().map(Diag::to_json).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json).expect("diagnostics serialize")
            );
        }
    }
}

fn check(file: &std::path::Path, format: Format, from: Option<&str>, theme: Theme) -> ExitCode {
    match compile_path(file) {
        Ok((program, mut diags)) => {
            if let Some(from) = from {
                match korin::run::resolve_from(&program, from) {
                    Ok(_) => {}
                    Err(diag) => diags.push(*diag),
                }
            }
            let failed = diags.iter().any(Diag::is_error);
            report(&diags, format);
            if failed {
                ExitCode::FAILURE
            } else {
                if matches!(format, Format::Human) {
                    eprintln!("{}", theme.lines(&format!("✓ {} is valid", file.display())));
                }
                ExitCode::SUCCESS
            }
        }
        Err(diags) => {
            report(&diags, format);
            ExitCode::FAILURE
        }
    }
}

fn plan(file: &std::path::Path, theme: Theme) -> ExitCode {
    match compile_path(file) {
        Ok((program, diags)) => {
            report(&diags, Format::Human);
            print!("{}", theme.lines(&korin::render::plan(&program)));
            ExitCode::SUCCESS
        }
        Err(diags) => {
            report(&diags, Format::Human);
            ExitCode::FAILURE
        }
    }
}

fn parse_param(text: &str) -> Result<(String, String), String> {
    text.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected NAME=VALUE, got `{text}`"))
}

fn run(
    file: &std::path::Path,
    params: Vec<(String, String)>,
    yes: bool,
    from: Option<String>,
    kill_grace: u64,
    theme: Theme,
) -> ExitCode {
    let program = match compile_path(file) {
        Ok((program, diags)) => {
            report(&diags, Format::Human);
            program
        }
        Err(diags) => {
            report(&diags, Format::Human);
            return ExitCode::FAILURE;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let options = korin::run::Options {
        params,
        yes,
        from,
        grace: std::time::Duration::from_secs(kill_grace),
        runs_dir: cwd.join(".korin").join("runs"),
        cwd,
        theme,
        invocation: invocation(),
    };
    ExitCode::from(korin::run::run(&program, options).exit_code())
}

fn fmt(files: &[PathBuf], check: bool) -> ExitCode {
    let mut status = ExitCode::SUCCESS;
    for path in files {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("cannot read {}: {e}", path.display());
                status = ExitCode::FAILURE;
                continue;
            }
        };
        // Refuse to reformat a file that does not parse; its indentation may be the error.
        let file = korin::diag::SourceFile::new(path, path.display().to_string(), text.clone());
        // A wizard's first line of code is `korin <version>`; comments may come before it.
        let is_wizard = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .is_some_and(|l| l.starts_with("korin "));
        let parsed = if is_wizard {
            korin::parser::parse_wizard(&file).map(|_| ())
        } else {
            korin::parser::parse_module(&file).map(|_| ())
        };
        if let Err(diags) = parsed {
            report(&diags, Format::Human);
            status = ExitCode::FAILURE;
            continue;
        }
        let formatted = korin::fmt::format(&text);
        if formatted == text {
            continue;
        }
        if check {
            eprintln!("{} is not formatted", path.display());
            status = ExitCode::FAILURE;
        } else if let Err(e) = std::fs::write(path, formatted) {
            eprintln!("cannot write {}: {e}", path.display());
            status = ExitCode::FAILURE;
        }
    }
    status
}

/// The command line korin was started with, as the run's intro shows it.
fn invocation() -> String {
    std::iter::once("korin".to_string())
        .chain(std::env::args().skip(1).map(|a| korin::render::quote(&a)))
        .collect::<Vec<_>>()
        .join(" ")
}
