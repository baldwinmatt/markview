use std::fs;
use std::io::{self, Read};
use std::process::ExitCode;

use markview::{help, render, Cli};

fn main() -> ExitCode {
    match run() {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("markview: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, Box<dyn std::error::Error>> {
    let cli = Cli::parse(std::env::args().skip(1))?;

    if cli.help {
        return Ok(help().to_owned());
    }

    let markdown = match cli.input {
        Some(path) => fs::read_to_string(path)?,
        None => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            input
        }
    };

    Ok(render(&markdown, cli.options))
}
