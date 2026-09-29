use r#override::{Edition, Result, Source, arm, item};

fn main() -> Result<()> {
    let mut source = Source::parse(
        r#"
use std::io::{self, Write};

pub enum Command {
    Print(String),
    Quit,
}

pub fn dispatch(command: Command, output: &mut impl Write) -> Result<bool, io::Error> {
    match command {
        Command::Print(message) => {
            writeln!(output, "{message}")?;
            output.flush()?;
        }
        Command::Quit => return Ok(false),
    }
    Ok(true)
}
"#,
        Edition::Edition2024,
    )?;

    source
        .select(item("dispatch").arm("Command::Print"))?
        .propagate()
        .extract(
            "pub fn write_line(message: &str, output: &mut impl Write)",
            &["&message", "output"],
        )?;
    source
        .select(item("Command"))?
        .add_variant("Repeat { message: String, count: usize }")?;
    source
        .select(item("dispatch").match_expr().has(arm("Command::Print")))?
        .at(arm("Command::Quit").before())
        .add_arm("Command::Repeat { message, count } => crate::repeat(&message, count, output)?")?;

    let application = r#"
fn repeat(message: &str, count: usize, output: &mut impl std::io::Write) -> std::io::Result<()> {
    for _ in 0..count {
        upstream::write_line(message, output)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let message = arguments.next().unwrap_or_else(|| "Hello".to_owned());
    let count = arguments.next().map(|value| value.parse()).transpose()?.unwrap_or(2);
    let mut output = std::io::stdout().lock();
    upstream::dispatch(upstream::Command::Repeat { message, count }, &mut output)?;
    Ok(())
}
"#;
    print!("pub mod upstream {{\n{source}\n}}\n{application}");
    Ok(())
}
