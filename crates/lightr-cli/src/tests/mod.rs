use clap::Parser as _;

use crate::cli::cmd::{Cli, Cmd, ComposeCmd, EngineCmd, OciCmd};

pub(super) fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("lightr").chain(args.iter().copied()))
        .expect("parse failed")
}

pub(super) fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("lightr").chain(args.iter().copied()))
}

mod g1;
mod g2;
mod g3;
mod g4;
#[cfg(unix)]
mod net3;
