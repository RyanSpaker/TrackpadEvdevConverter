pub mod mouse;
pub mod manager;
pub mod communicator;
pub mod server;
pub mod client;

use std::{env::args, error::Error, fmt::Display};

use client::ClientCommand;

/// Prints the help message
pub fn print_help() -> Result<(), Box<dyn std::error::Error>>{
    println!("Trackpad to Mouse evdev Conversion Utility: ");
    println!("Usage: trackpad-evdev-converter [function] [parameter]");
    println!("\"\", \"--server\" : Starts a process to handle all mice conversions");
    println!("\"-n\", \"--new\" : Tells the server to create a new mouse with parameters: name path_to_evdev_event");
    println!("\"-l\", \"--list\" : Queries the server and prints all currently active mice, (name input_event_id output_event_id)");
    println!("\"-s\", \"--stop\" : Tells the server to stop a mouse with parameter: name");
    println!("\"--shutdown\" : Tells the server to stop all mice and exit");
    println!("\"--reset\" : Tells the server to stop all mice and not exit");
    println!("\"--server-pid\" : print the server pid");
    println!("The program may require sudo privaliges in order to work.");
    return Ok(());
}

/// The command was malformed
pub fn malformed() -> Result<(), Box<dyn std::error::Error>>{
    println!("Malformed Usage."); print_help()?;
    return Err(Box::new(AppError::MalformedCommand))
}

/// Enum representing app errors
#[derive(Debug, Clone)]
pub enum AppError{
    MalformedCommand
}
impl Display for AppError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AppError::MalformedCommand => "Command was Malformed"
        })?;
        Ok(())
    }
}
impl Error for AppError{}

pub async fn app_logic() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = args().skip(1).collect::<Vec<String>>();

    //server
    if arguments.len() == 0 || arguments[0] == "--server" {
        return server::server().await;
    }

    let function: ClientCommand = match arguments[0].as_str() {
        "-n" | "--new" => {
            if arguments.len() != 3 {return malformed();}
            ClientCommand::New(arguments[1].clone(), arguments[2].clone())
        }
        "-l" | "--list" => {
            if arguments.len() != 1 {return malformed();}
            ClientCommand::List
        }
        "-s" | "--stop" => {
            if arguments.len() != 2 {return malformed();}
            ClientCommand::Stop(arguments[1].clone())
        }
        "--shutdown" => {
            if arguments.len() != 1 {return malformed();}
            ClientCommand::Shutdown
        }
        "--reset" => {
            if arguments.len() != 1 {return malformed();}
            ClientCommand::Reset
        }
        "--server-pid" => {
            if arguments.len() != 1 {return malformed();}
            ClientCommand::PID
        }
        "--help" => {return print_help();}
        _ => {return malformed();}
    };

    //client
    return client::client(function).await;
}

/// Main function. Run server, or client commands
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    test_logic()
}