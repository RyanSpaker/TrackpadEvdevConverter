/*  
    The client is a user systemd service which is part of the graphical-session.target
    It is reponsible for automatically disabling/enabling mice that the server creates
*/
use std::{error::Error, fmt::Display, time::Duration};
use dbus::nonblock;
use dbus_tokio::connection;


/// Enum representing the different functions of the client side app
pub enum Command{
    New(String, String),
    List,
    Stop(String),
    StopAll
}

/// Error representing ways the client can fail
#[derive(Debug)]
pub enum CliError{
    FailedToConnectToSystemBus(dbus::Error),
    MethodCallFailed(dbus::Error)
}
impl Display for CliError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f.write_str(&match self {
            CliError::FailedToConnectToSystemBus(err) => format!("Could not connect to the system dbus: {}", *err),
            CliError::MethodCallFailed(err) => format!("Method call failed: {}", *err)
        });
        Ok(())
    }
}
impl Error for CliError{}

/// Client code
pub async fn cli(command: Command) -> Result<(), CliError> {
    // Setup DBus connection
    let (r, conn) = connection::new_system_sync()
        .map_err(|err| CliError::FailedToConnectToSystemBus(err))?;
    let dbus_handle = tokio::spawn(r);
    // Setup proxy
    let proxy = nonblock::Proxy::new("org.cws.VirtualMouse", "/org/cws/VirtualMouse", Duration::from_secs(2), conn.clone());
    // Do the command
    match command {
        Command::New(name, path) => {
            let (name, inputpath, outputpath): (String, String, String) = proxy.method_call(
                "org.cws.VirtualMouse.Manager", 
                "CreateMouse", 
                (name.as_str(), path.as_str())
            ).await.map_err(|err| CliError::MethodCallFailed(err))?;
            println!("Success: (name input-path output-path)");
            println!("{} {} {}", name, inputpath, outputpath);
        }
        Command::List => {
            let (list,): (Vec<(String, String, String)>,) = proxy.method_call(
                "org.cws.VirtualMouse.Manager", 
                "ListMice", 
                ()).await.map_err(|err| CliError::MethodCallFailed(err))?;
            println!("Mice: (name input-path output-path)");
            for (name, input_id, output_id) in list.into_iter() {
                println!("{} {} {}", name, input_id, output_id);
            }
        }
        Command::Stop(name) => {
            let _: (String, String, String) = proxy.method_call(
                "org.cws.VirtualMouse.Manager", 
                "DestroyMouse", 
                (name,)).await.map_err(|err| CliError::MethodCallFailed(err))?;
        }
        Command::StopAll => {
            proxy.method_call(
                "org.cws.VirtualMouse.Manager", 
                "ClearMice", 
                ()).await.map_err(|err| CliError::MethodCallFailed(err))?;
        }
    }
    dbus_handle.abort();
    Ok(())
}
