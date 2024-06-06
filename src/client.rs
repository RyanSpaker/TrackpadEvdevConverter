use std::{error::Error, fmt::Display};

use dbus::nonblock;
use dbus_tokio::connection;


/// Enum representing the different functions of the client side app
pub enum ClientCommand{
    New(String, String),
    List,
    Stop(String),
    Shutdown,
    Reset,
    PID
}

/// Error representing ways the client can fail
#[derive(Debug)]
pub enum ClientError{
    DBusConnectionFailed(dbus::Error),
    ServerNotFound(dbus::Error),
    MethodCallFailed(dbus::Error)
}
impl Display for ClientError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let string = match self {
            ClientError::DBusConnectionFailed(err) => format!("Could not create system dbus connection. DBus error: {}", err),
            ClientError::ServerNotFound(err) => format!("Failed to find the server. DBus error: {}", err),
            ClientError::MethodCallFailed(err) => format!("Failed to call the method. DBus error: {}", err)
        };
        f.write_str(string.as_str())?;
        Ok(())
    }
}
impl Error for ClientError{}

/// Client code
pub async fn client(function: ClientCommand) -> Result<(), Box<dyn std::error::Error>> {
    // Setup DBus connection
    let (resource, conn) = connection::new_system_sync()
        .map_err(|err| ClientError::DBusConnectionFailed(err))?;
    let dbus_handle = tokio::spawn(async {
        resource.await
    });
    // Setup proxy
    let proxy = nonblock::Proxy::new("com.cowsociety.virtual_mouse", "/", std::time::Duration::from_secs(2), conn.clone());
    // make sure server is running
    proxy.method_call::<(u32,), (), &str, &str>("com.cowsociety.virtual_mouse", "GetProcessID", ()).await
        .map_err(|err| ClientError::ServerNotFound(err))?;
    // Do the command
    match function {
        ClientCommand::New(name, path) => {
            let (name, input_id, output_id, libinput_id): (String, u32, u32, u32) = proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "CreateNewMouse", 
                (name.as_str(), path.as_str())
            ).await.map_err(|err| ClientError::MethodCallFailed(err))?;
            println!("Success: (name input_id output_id, libinput_id)");
            println!("{} {} {} {}", name, input_id, output_id, libinput_id);
        }
        ClientCommand::List => {
            let (list,): (Vec<(String, u32, u32, u32)>,) = proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "ListMice", 
                ()).await.map_err(|err| ClientError::MethodCallFailed(err))?;
            println!("Mice: (name input_id output_id libinput_id)");
            for (name, input_id, output_id, libinput_id) in list.into_iter() {
                println!("{} {} {} {}", name, input_id, output_id, libinput_id);
            }
        }
        ClientCommand::Stop(name) => {
            proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "StopMouse", 
                (name, )).await.map_err(|err| ClientError::MethodCallFailed(err))?;
        }
        ClientCommand::Shutdown => {
            proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "Shutdown", 
                ()).await.map_err(|err| ClientError::MethodCallFailed(err))?;
        }
        ClientCommand::Reset => {
            proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "Reset", 
                ()).await.map_err(|err| ClientError::MethodCallFailed(err))?;
        }
        ClientCommand::PID => {
            let (pid,): (u32,) = proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "GetProcessID", 
                ()).await.map_err(|err| ClientError::MethodCallFailed(err))?;
            println!("Server Process ID:");
            println!("{}", pid);
        }
    }
    dbus_handle.abort();
    Ok(())
}
