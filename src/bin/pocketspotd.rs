//! `pocketspotd`: the PocketSpot playback service.

use pocketspot::service::{self, Exit, Options};
use std::process::ExitCode;

const USAGE: &str = "usage: pocketspotd [--platform nextui|development] [--mock]
       pocketspotd --version

  --mock   play fictional music without an account (development builds;
           POCKETSPOT_MOCK_SIGNED_IN=1 skips pairing)

Exit codes: 0 stopped, 1 failure, 2 bad arguments or profile, 3 already running.";

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Ok(Some(options)) => ExitCode::from(service::run(options) as u8),
        Ok(None) => {
            println!("pocketspotd {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("pocketspotd: {message}\n{USAGE}");
            ExitCode::from(Exit::Unusable as u8)
        }
    }
}

/// `Ok(None)` for `--version`.
fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => return Ok(None),
            "--platform" => {
                let name = args.next().ok_or("--platform needs a name")?;
                options.platform = Some(name.parse().map_err(|e| format!("{e}"))?);
            }
            #[cfg(feature = "mock")]
            "--mock" => {
                let signed_in =
                    std::env::var_os("POCKETSPOT_MOCK_SIGNED_IN").is_some_and(|v| v == "1");
                options.mock = Some(pocketspot::engine::mock::MockOptions {
                    signed_in,
                    ..Default::default()
                });
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(Some(options))
}
