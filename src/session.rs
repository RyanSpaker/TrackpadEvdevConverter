/* Session Server
    Should be run automatically as a user systemd service.
    Listens for mouse created and deleted broadcasts from the system server
    Runs xinput to turn on and off the mice when they are deleted/created
*/

use std::{error::Error, fmt::Display};
use dbus::message::MatchRule;
use dbus_tokio::connection;

/// Error representing ways the server can fail
#[derive(Debug)]
pub enum SessionServerError{
    DBusConnectionFailed(dbus::Error),
    XInputCallError(std::io::Error),
    XInputParseError
}
impl Display for SessionServerError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let string = match self {
            SessionServerError::DBusConnectionFailed(err) => format!("Could not create system dbus connection. DBus error: {}", err),
            SessionServerError::XInputCallError(err) => format!("Failed to call the xinput tool. IO Error: {}", err),
            SessionServerError::XInputParseError => format!("Failed to parse xinput data")
        };
        f.write_str(string.as_str())?;
        Ok(())
    }
}
impl Error for SessionServerError{}

/// Server code
pub async fn session_server() -> Result<(), Box<dyn Error>> {
    // Setup DBus connection
    // We use the system bus because that is where the broadcasts are. since we are only listening, we should be fine
    let (resource, conn) = connection::new_system_sync()
        .map_err(|err| SessionServerError::DBusConnectionFailed(err))?;
    let dbus_handle = tokio::spawn(async {
        resource.await
    });
    // Setup callbacks to handle mouse creation and deletion events
    let sig1 = conn.add_match(MatchRule::new_signal("com.cowsociety.virtual_mouse", "MouseCreated")).await?.cb(|_, (id,): (u32,)| {
        if let Err(err) = toggle_mouse(id, false) {
            println!("Error: {:?}", err);
        }
        true
    });
    let sig2 = conn.add_match(MatchRule::new_signal("com.cowsociety.virtual_mouse", "MouseDeleted")).await?.cb(|_, (id,): (u32,)| {
        if let Err(err) = toggle_mouse(id, true) {
            println!("Error: {:?}", err);
        }
        true
    });
    // Run forever
    dbus_handle.await?;
    conn.remove_match(sig1.token()).await?; conn.remove_match(sig2.token()).await?;
    Ok(())
}
// Helper function to take an input id and use xinput to disable/enable the corresponding mouse
pub fn toggle_mouse(input_id: u32, enable: bool) -> Result<(), SessionServerError> {
    let event_string = "event".to_owned() + &input_id.to_string();
    let output = std::process::Command::new("xinput").args(["list", "--id-only"]).output()
        .map_err(|err| SessionServerError::XInputCallError(err))?;
    let output = String::from_utf8(output.stdout).map_err(|_| SessionServerError::XInputParseError)?;
    let id = output.split("\n").map(|id| {
        if id.parse::<u32>().is_ok() {id.to_string()} else {id.strip_prefix("∼ ").unwrap_or("No").to_string()}
    }).filter(|id| {
        std::process::Command::new("xinput").args(["list-props", id]).output().ok().map(|output| {
            String::from_utf8(output.stdout).ok()
        }).flatten().is_some_and(|props| {
            if props.contains(event_string.as_str()) {
                true
            }else{
                false
            }
        })
    }).next().map(|id| id.parse::<u32>().ok()).flatten().ok_or(SessionServerError::XInputParseError)?;
    if enable {println!("Enabled mouse {}", id);} else {println!("Disabled mouse {}", id);}
    std::process::Command::new("xinput").args([(if enable {"--enable"} else {"--disable"}).to_string(), id.to_string()]).spawn().unwrap().wait().unwrap();
    Ok(())
}