/*  
    The client is a user systemd service which is part of the graphical-session.target
    It is reponsible for automatically disabling/enabling mice that the server creates
*/
use std::{collections::{HashMap, HashSet}, error::Error, fmt::Display, path::Path, process::Stdio, sync::{Arc, Mutex}, task::{Poll, Waker}, time::Duration};
use dbus::{message::MatchRule, nonblock};
use dbus_tokio::connection::{self, IOResourceError};
use futures::Future;
use tokio::task::JoinError;

/// Error representing ways the client can fail
#[derive(Debug)]
pub enum SessionError{
    DisplayNotSet,
    XAuthorityNotSet,
    DBusConnectionFailed(dbus::Error),
    FailedToAddSignalHandler(dbus::Error),
    FailedtoGetMiceList(dbus::Error),
    SystemConnectionLost(Result<IOResourceError, JoinError>),
    XInputToggleFailed(u32, String),
    FailedToCallXInputToggle(u32, std::io::Error),
    FailedToCallXInputList(std::io::Error),
    XInputListFailed(String),
    FailedToFindLibinputID(String, String),
    FailedToCallXInputListProps(std::io::Error)
}
impl Display for SessionError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f.write_str(&match self {
            SessionError::DisplayNotSet => format!("The Display Environment variable is either empty or not set"),
            SessionError::XAuthorityNotSet => format!("The XAuthority Environment variable is either not set, or does not point to an existing xauth file"),
            SessionError::DBusConnectionFailed(err) => format!("Could not create system dbus connection. DBus error: {}", *err),
            SessionError::FailedToAddSignalHandler(err) => format!("Could not add a signal handler to the dbus connection: {}", *err),
            SessionError::FailedtoGetMiceList(err) => format!("Could not get mice list from the server: {}", *err),
            SessionError::SystemConnectionLost(err) => format!("The system dbus connection was lost: {:?}", *err),
            SessionError::XInputToggleFailed(id, stderr) => format!("Call to xinput --enable or --disable for id {}, exited unsuccessfully with stderr: {}", *id, *stderr),
            SessionError::FailedToCallXInputToggle(id, err) => format!("Failed to call xinput --enable or --disable for id {} with err: {}", *id, *err),
            SessionError::FailedToCallXInputList(err) => format!("Failed to call xinput list --id-only with err: {}", *err),
            SessionError::XInputListFailed(stderr) => format!("Call to xinput list --id-only returned unsuccessfully with err: {}", *stderr),
            SessionError::FailedToFindLibinputID(path, output) => format!("Failed to find the libinput id of evdev device: {}, with xinput list -id-only output of: {}", *path, *output),
            SessionError::FailedToCallXInputListProps(err) => format!("Failed to call xinput list-props with err {}", *err)
        });
        Ok(())
    }
}
impl Error for SessionError{}

pub async fn run_session() -> SessionError{
    match session().await {
        Err(err) => {return err;},
        _ => {panic!("How did we get here? session.rs, run_session, session return Ok(())")}
    }
}

/// Client code
pub async fn session() -> Result<(), SessionError> {
    if !std::env::var("DISPLAY").is_ok_and(|value| value != "") {return Err(SessionError::DisplayNotSet);}
    if !std::env::var("XAUTHORITY").is_ok_and(|path| Path::new(&path).exists()) {return Err(SessionError::XAuthorityNotSet);}
    // Setup DBus connection
    let (r, conn) = connection::new_system_sync()
        .map_err(|err| SessionError::DBusConnectionFailed(err))?;
    let dbus_handle = tokio::spawn(r);
    // setup variables to store mice that need to be enable and disabled. stores the input id, and a optional waker to execute when adding mice
    let disable_mice_list: Arc<Mutex<(HashSet<String>, Option<Waker>)>> = Arc::new(Mutex::new((HashSet::new(), None)));
    let enable_mice_list: Arc<Mutex<(HashSet<String>, Option<Waker>)>> = Arc::new(Mutex::new((HashSet::new(), None)));
    // create signal handler for new mice and destroyed mice
    let mr = MatchRule::new_signal("org.cws.VirtualMouse.Manager", "MouseAdded");
    let dml = disable_mice_list.clone(); let eml = enable_mice_list.clone();
    let added_signal_handle = conn.add_match(mr).await
        .map_err(|err| SessionError::FailedToAddSignalHandler(err))?
        .cb(move |_, (name, input_path, output_path): (String, String, String)| {
            println!("Signal Received: Mouse Added! input_path: {}, output_path: {}, name: {}", input_path, output_path, name);
            if let (Ok(mut dml), Ok(mut eml)) = (dml.lock(), eml.lock()) {
                eml.0.remove(&input_path);
                dml.0.insert(input_path);
                if let Some(waker) = dml.1.take() {waker.wake();}
            }
            true
        });
    let mr = MatchRule::new_signal("org.cws.VirtualMouse.Manager", "MouseRemoved");
    let dml = disable_mice_list.clone(); let eml = enable_mice_list.clone();
    let removed_signal_handle = conn.add_match(mr).await
        .map_err(|err| SessionError::FailedToAddSignalHandler(err))?
        .cb(move |_, (name, input_path, output_path): (String, String, String)| {
            println!("Signal Received: Mouse Removed! input_id: {}, output_id: {}, name: {}", input_path, output_path, name);
            if let (Ok(mut dml), Ok(mut eml)) = (dml.lock(), eml.lock()) {
                dml.0.remove(&input_path);
                eml.0.insert(input_path);
                if let Some(waker) = eml.1.take() {waker.wake();}
            }
            true
        });
    // populate disable list with the current set of mice
    let proxy = nonblock::Proxy::new("org.cws.VirtualMouse", "/org/cws/VirtualMouse", Duration::from_secs(2), conn.clone());
    let mice_list: (Vec<(String, String, String)>,) = proxy.method_call("org.cws.VirtualMouse.Manager", "ListMice", ()).await
        .map_err(|err| SessionError::FailedtoGetMiceList(err))?;
    disable_mice_list.lock().unwrap().0.extend(mice_list.0.into_iter().map(|(i, _, _)| i));

    // setup loop to handle disable and enable mice lists
    let result = tokio::select! {
        result = dbus_handle => {Err(SessionError::SystemConnectionLost(result))},
        result = update_loop(disable_mice_list, enable_mice_list) => {result}
    };
    let _ = conn.remove_match(added_signal_handle.token()).await;
    let _ = conn.remove_match(removed_signal_handle.token()).await;
    result
}

/// asynchronous function which continuosly handles any mice that need their state toggled in xinput
pub async fn update_loop(dml: Arc<Mutex<(HashSet<String>, Option<Waker>)>>, eml: Arc<Mutex<(HashSet<String>, Option<Waker>)>>) -> Result<(), SessionError>{
    // map of input_path -> libinput id, contains all currently disabled mice
    let mut disabled_mice: HashMap<String, u32> = HashMap::new();
    loop{
        let (disable, enable) = MiceNeedToggleFuture{dml: dml.clone(), eml: eml.clone()}.await;
        // enable mice
        for libinput_id in enable.into_iter().filter_map(|id| disabled_mice.remove(&id)) {
            toggle_mouse(libinput_id, true).await?;
        }
        // disable mice
        for input_id in disable{
            let libinput_id = get_libinput_id(&input_id).await?;
            toggle_mouse(libinput_id, false).await?;
            disabled_mice.insert(input_id, libinput_id);
        }
    }
}

/// given a evdev id, finds the libinput id of a device using xinput
pub async fn get_libinput_id(input_path: &str) -> Result<u32, SessionError> {
    let result = tokio::process::Command::new("xinput")
        .args(["list", "--id-only"])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output().await
        .map_err(|err| SessionError::FailedToCallXInputList(err))?;
    let output = if result.status.success() {
        Ok(String::from_utf8_lossy(&result.stdout).to_string())
    } else {
        Err(SessionError::XInputListFailed(String::from_utf8_lossy(&result.stderr).to_string()))
    }?;
    let ids = output.split("\n").filter_map(|entry| entry.strip_prefix("∼ ").unwrap_or(entry).parse::<u32>().ok());
    for libinput_id in ids {
        if String::from_utf8_lossy(&tokio::process::Command::new("xinput")
            .args(["list-props", &libinput_id.to_string()])
            .stdout(Stdio::piped()).stderr(Stdio::piped()).output().await
            .map_err(|err| SessionError::FailedToCallXInputListProps(err))?.stdout)
            .contains(input_path) 
        {
            return Ok(libinput_id);
        }
    }
    Err(SessionError::FailedToFindLibinputID(input_path.to_owned(), output))
}

/// enables or disables a mouse using the xinput command, given its libinput id number
pub async fn toggle_mouse(libinput_id: u32, enable: bool) -> Result<(), SessionError> {
    let result = tokio::process::Command::new("xinput")
        .args([if enable {"--enable"} else {"--disable"}, &libinput_id.to_string()])
        .stdout(Stdio::null()).stderr(Stdio::piped()).output().await
        .map_err(|err| SessionError::FailedToCallXInputToggle(libinput_id, err))?;
    if result.status.success() {Ok(())}
    else {Err(SessionError::XInputToggleFailed(libinput_id, String::from_utf8_lossy(&result.stderr).to_string()))}
}

/// future which waits for any mice to be queued for disabling or enabling
pub struct MiceNeedToggleFuture{
    pub dml: Arc<Mutex<(HashSet<String>, Option<Waker>)>>, 
    pub eml: Arc<Mutex<(HashSet<String>, Option<Waker>)>>
}
impl Future for MiceNeedToggleFuture{
    type Output = (HashSet<String>, HashSet<String>);
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        if let (Ok(mut dml), Ok(mut eml)) = (self.dml.lock(), self.eml.lock()) {
            if dml.0.is_empty() && eml.0.is_empty() {
                let _ = eml.1.insert(cx.waker().clone()); let _ = dml.1.insert(cx.waker().clone());
                Poll::Pending
            } else {
                Poll::Ready((dml.0.drain().collect(), eml.0.drain().collect()))
            }
        }else {panic!("Failed to aqcuire mutex while polling Mice Need Toggle Future");}
    }
}
