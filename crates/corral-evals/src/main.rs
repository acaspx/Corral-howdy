//! `corral-evals`: run an eval suite and write a report.
//!
//! ```text
//! corral-evals --suite gate [--scenarios evals/scenarios] [--report out.json] [--baseline base.json]
//! ```
//! Exit code 0 when the gate passes, 1 when it fails, 2 on usage or input errors.

use corral_evals::{Report, Suite, build_report, load_scenarios, markdown_summary, run};
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    suite: Suite,
    scenarios: PathBuf,
    report: Option<PathBuf>,
    baseline: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut suite = None;
    let mut scenarios = PathBuf::from("evals/scenarios");
    let mut report = None;
    let mut baseline = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--suite" => {
                let v = value()?;
                suite = Some(Suite::parse(&v).ok_or(format!("unknown suite `{v}` (gate | full)"))?);
            }
            "--scenarios" => scenarios = PathBuf::from(value()?),
            "--report" => report = Some(PathBuf::from(value()?)),
            "--baseline" => baseline = Some(PathBuf::from(value()?)),
            "-h" | "--help" => {
                return Err("usage: corral-evals --suite <gate|full> [--scenarios DIR] [--report FILE] [--baseline FILE]".into());
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Args {
        suite: suite.ok_or("--suite is required")?,
        scenarios,
        report,
        baseline,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    let scenarios = match load_scenarios(&args.scenarios) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("invalid scenarios: {e}");
            return ExitCode::from(2);
        }
    };

    let baseline: Option<Report> = match &args.baseline {
        Some(path) if path.exists() => match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("{}: unreadable baseline: {e}", path.display());
                return ExitCode::from(2);
            }
        },
        Some(path) => {
            eprintln!(
                "note: baseline {} not found; comparing against nothing",
                path.display()
            );
            None
        }
        None => None,
    };

    let report = build_report(args.suite, run(&scenarios), baseline.as_ref());

    if let Some(path) = &args.report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("{}: {e}", parent.display());
            return ExitCode::from(2);
        }
        let json = match serde_json::to_string_pretty(&report) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("report: {e}");
                return ExitCode::from(2);
            }
        };
        if let Err(e) = std::fs::write(path, json + "\n") {
            eprintln!("{}: {e}", path.display());
            return ExitCode::from(2);
        }
    }

    print!("{}", markdown_summary(&report));
    if report.gate_passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
