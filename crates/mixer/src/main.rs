// SPDX-License-Identifier: MPL-2.0
mod app;
mod color;
mod connection;
mod model;
mod view;

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Options {
    socket: PathBuf,
    explicit_socket: bool,
    remote: Option<String>,
    config: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = None;
    let mut remote = None;
    let mut config = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "Cosmos Audio Mixer\nUsage: cosmos-audio-mixer [--socket PATH] [--remote NAME] [--config PATH]\n\nConnects to a separate engine. Use Start engine if it is not running.\nClosing the window does not stop audio. No recording or default-device changes."
                );
                return Ok(());
            }
            "--socket" => socket = Some(PathBuf::from(args.next().ok_or("--socket needs a path")?)),
            "--remote" => remote = Some(args.next().ok_or("--remote needs a name")?),
            "--config" => config = Some(PathBuf::from(args.next().ok_or("--config needs a path")?)),
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let options = Options {
        explicit_socket: socket.is_some(),
        socket: socket.map_or_else(cosmos_control::default_socket, Ok)?,
        remote,
        config,
    };
    let settings = cosmic::app::Settings::default()
        .size(cosmic::iced::Size::new(1220.0, 820.0))
        .exit_on_close(false);
    cosmic::app::run::<app::Mixer>(settings, options)?;
    Ok(())
}
