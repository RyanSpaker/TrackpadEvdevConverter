pub mod mouse;
pub mod server;
pub mod session;
pub mod cli;

use std::{env::args, error::Error, fmt::Display};
use cli::{CliError, Command};
use nix::unistd::Uid;
use server::ServerError;
use session::SessionError;

/// Prints the help message
pub fn print_help() -> Result<(), AppError>{
    println!("Trackpad to Mouse evdev Conversion Utility: ");
    println!("Usage: trackpad-evdev-converter [function] [parameter]\n");
    println!("--server    : Starts the main root process which handles mice conversion. Should only be run by systemd as a service start command.");
    println!("--session   : Starts the user session process which handles xinput configuration. Should only be run by systemd as a user service start command.");
    println!("-n, --new   : Tells the server to create a new mouse with parameters: name path_to_evdev_event_file");
    println!("-l, --list  : Queries the server and prints all currently active mice");
    println!("-s, --stop  : Tells the server to stop a mouse with parameter: name");
    println!("-c, --clear : Tells the server to stop all mice");
    return Ok(());
}

/// The command was malformed
pub fn malformed() -> Result<(), AppError>{
    println!("Malformed Usage."); print_help()?;
    return Err(AppError::MalformedCommand)
}

/// Enum representing app errors
#[derive(Debug)]
pub enum AppError{
    MalformedCommand,
    ServerNotRunAsRoot,
    ServerError(ServerError),
    SessionError(SessionError),
    CliError(CliError)
}
impl Display for AppError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            AppError::MalformedCommand => format!("Command was Malformed"),
            AppError::ServerNotRunAsRoot => format!("The Server was not run as root"),
            AppError::ServerError(err) => format!("The system server returned with err: {}", *err),
            AppError::SessionError(err) => format!("Session server returned with err: {}", *err),
            AppError::CliError(err) => format!("The command failed with err: {}", *err)
        })?;
        Ok(())
    }
}
impl Error for AppError{}

pub async fn app() -> Result<(), AppError> {
    let arguments = args().skip(1).collect::<Vec<String>>();

    if arguments.len() == 0 {return print_help();}

    //server
    if arguments[0] == "--server" {
        // make sure we are root
        if !Uid::effective().is_root() {
            return Err(AppError::ServerNotRunAsRoot);
        }
        let server_state = server::server().await.map_err(|err| AppError::ServerError(err))?;
        let err = mouse::MouseManager::new(server_state.data.clone()).spawn_update_loop().await;
        // killing is the only correct way to end the program, as it shouldnt end by itself
        return Err(AppError::ServerError(err));
    }

    //session server
    if arguments[0] == "--session" {
        return Err(AppError::SessionError(session::run_session().await));
    }

    // cli
    let function: Command = match arguments[0].as_str() {
        "-n" | "--new" => {
            if arguments.len() != 3 {return malformed();}
            Command::New(arguments[1].clone(), arguments[2].clone())
        }
        "-l" | "--list" => {
            if arguments.len() != 1 {return malformed();}
            Command::List
        }
        "-s" | "--stop" => {
            if arguments.len() != 2 {return malformed();}
            Command::Stop(arguments[1].clone())
        }
        "-c" | "--clear" => {
            if arguments.len() != 1 {return malformed();}
            Command::StopAll
        }
        "--help" => {return print_help();}
        _ => {return malformed();}
    };

    //client
    return cli::cli(function).await.map_err(|err| AppError::CliError(err));
}

/// Main function. Run server, or client commands
#[tokio::main]
async fn main() -> Result<(), AppError> {
    app().await
}
