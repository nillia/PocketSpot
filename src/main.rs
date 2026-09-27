use pocketspot::platform::{Platform, Profile, SystemEnvironment};
use std::process::ExitCode;

const USAGE: &str = "usage: pocketspot profile [--platform nextui|development]
       pocketspot --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--version"] => {
            println!("pocketspot {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["profile"] => profile(None),
        ["profile", "--platform", name] => match name.parse::<Platform>() {
            Ok(platform) => profile(Some(platform)),
            Err(error) => fail(&error.to_string()),
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Resolve and prepare the profile, then show it.
fn profile(explicit: Option<Platform>) -> ExitCode {
    let ready = match Profile::resolve(&SystemEnvironment, explicit).and_then(Profile::prepare) {
        Ok(ready) => ready,
        Err(error) => return fail(&error.to_string()),
    };
    println!("platform:    {}", ready.platform());
    println!("state dir:   {}", ready.state_dir().display());
    println!("runtime dir: {}", ready.runtime_dir().display());
    println!("audio:       {:?}", ready.audio());
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("pocketspot: {message}");
    ExitCode::FAILURE
}
