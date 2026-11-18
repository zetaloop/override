mod config;
mod provider;

use std::{env, ffi::OsString, process};

use r#override::Result;
use provider::Provider;

const USAGE: &str = "\
Usage: override enable <provider>
       override disable <name>
       override list
       override with <provider> -- <cargo command> [arguments...]

Provider: <name>[@<version>] | --path <directory> | <name> --git <url> --rev <commit>";

fn main() {
    match run(&env::args_os().skip(1).collect::<Vec<_>>()) {
        Ok(code) => process::exit(code),
        Err(error) => {
            eprintln!("override: {error}");
            process::exit(1);
        }
    }
}

fn run(args: &[OsString]) -> Result<i32> {
    let Some((command, args)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match command.to_str() {
        Some("--help" | "-h") => println!("{USAGE}"),
        Some("--version" | "-V") => println!("override {}", env!("CARGO_PKG_VERSION")),
        Some("enable") => config::enable(Provider::load(args)?)?,
        Some("disable") if args.len() == 1 => config::disable(text(&args[0])?)?,
        Some("list") if args.is_empty() => config::list()?,
        Some("with") => {
            let pos = args.iter().position(|arg| arg == "--").ok_or(USAGE)?;
            let (provider, cargo) = (&args[..pos], &args[pos + 1..]);
            if cargo.is_empty() {
                return Err(USAGE.into());
            }
            let directory = tempfile::tempdir()?;
            let config = Provider::load(provider)?.write(directory.path())?;
            let status =
                process::Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
                    .arg("--config")
                    .arg(config)
                    .args(cargo)
                    .status()?;
            return Ok(status.code().unwrap_or(1));
        }
        _ => return Err(USAGE.into()),
    };
    Ok(0)
}

fn text(argument: &OsString) -> Result<&str> {
    argument
        .to_str()
        .ok_or_else(|| "expected a Unicode argument".into())
}
