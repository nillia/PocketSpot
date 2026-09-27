//! `pocketspotd`: the PocketSpot playback service.

use pocketspot::{
    platform::Platform,
    service::{self, Exit},
};
use std::process::ExitCode;

const USAGE: &str = "usage: pocketspotd [--platform nextui|development]
       pocketspotd --version

Exit codes: 0 stopped, 1 failure, 2 bad arguments or profile, 3 already running.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let explicit = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => None,
        ["--version"] => {
            println!("pocketspotd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        ["--platform", name] => match name.parse::<Platform>() {
            Ok(platform) => Some(platform),
            Err(error) => {
                eprintln!("pocketspotd: {error}");
                return ExitCode::from(Exit::Unusable as u8);
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(Exit::Unusable as u8);
        }
    };
    ExitCode::from(service::run(explicit) as u8)
}
