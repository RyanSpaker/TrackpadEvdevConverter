use std::{env::args, fs::{File, OpenOptions}, os::{fd::OwnedFd, unix::fs::OpenOptionsExt}, path::Path, process, sync::{Arc, Mutex}};
use communicator::{Communicator, CommunicatorResultFuture};
use dbus::{channel::MatchingReceiver, message::MatchRule, nonblock, MethodErr};
use dbus_crossroads::{Crossroads, IfaceBuilder};
use dbus_tokio::connection;
use input::LibinputInterface;
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
use manager::MouseManager;
use tokio::task;

pub mod mouse;
pub mod manager;
pub mod communicator;
struct Interface;
impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        OpenOptions::new()
            .custom_flags(flags)
            .read((flags & O_RDONLY != 0) | (flags & O_RDWR != 0))
            .write((flags & O_WRONLY != 0) | (flags & O_RDWR != 0))
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

/// Prints the help message
pub fn print_help() -> Result<(), Box<dyn std::error::Error>>{
    println!("Trackpad to Mouse evdev Conversion Utility: ");
    println!("Usage: trackpad-evdev-converter [function] [parameter]");
    println!("\"\", \"--server\" : Starts a process to handle all mice conversions");
    println!("\"-n\", \"--new\" : Tells the server to create a new mouse with parameters: name path_to_evdev_event");
    println!("\"-l\", \"--list\" : Queries the server and prints all currently active mice, (name input_event_id output_event_id)");
    println!("\"-s\", \"--stop\" : Tells the server to stop a mouse with parameter: name");
    println!("\"--shutdown\" : Tells the server to stop all mice and exit");
    println!("\"--server-pid\" : print the server pid");
    println!("The program may require sudo privaliges in order to work.");
    return Ok(());
}

/// The command was malformed
pub fn malformed() -> Result<(), Box<dyn std::error::Error>>{
    println!("Malformed Usage.");
    return print_help();
}

/// Enum representing the different functions of the client side app
pub enum AppFunction{
    New(String, String),
    List,
    Stop(String),
    Shutdown,
    PID
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = args().skip(1).collect::<Vec<String>>();
    if arguments.len() == 0 || arguments[0] == "--server" {
        server().await;
        return Ok(());
    }

    let function: AppFunction = match arguments[0].as_str() {
        "-n" | "--new" => {
            if arguments.len() != 3 {return malformed();}
            AppFunction::New(arguments[1].clone(), arguments[2].clone())
        }
        "-l" | "--list" => {
            if arguments.len() != 1 {return malformed();}
            AppFunction::List
        }
        "-s" | "--stop" => {
            if arguments.len() != 2 {return malformed();}
            AppFunction::Stop(arguments[1].clone())
        }
        "--shutdown" => {
            if arguments.len() != 1 {return malformed();}
            AppFunction::Shutdown
        }
        "--server-pid" => {
            if arguments.len() != 1 {return malformed();}
            AppFunction::PID
        }
        "--help" => {return print_help();}
        _ => {return malformed();}
    };

    //client
    
    // Setup DBus connection
    let (resource, conn) = match connection::new_session_sync() {
        Ok(val) => {val}, 
        Err(err) => {panic!("Failed to connect to the D-Bus Session: {}", err)}
    };
    let dbus_handle = tokio::spawn(async {
        let err = resource.await;
        panic!("Lost connection to D-Bus: {}", err);
    });

    let proxy = nonblock::Proxy::new("com.cowsociety.virtual_mouse", "/", std::time::Duration::from_secs(2), conn.clone());
    if let Err(_) = proxy.method_call::<(Vec<(String, u32, u32)>,), (), &str, &str>(
        "com.cowsociety.virtual_mouse", 
        "ListMice", 
        ()
    ).await {
        println!("Could not find or connect to server. (make sure to start it with --server)");
        return Ok(());
    }
    match function {
        AppFunction::New(name, path) => {
            let (name, input_id, output_id): (String, u32, u32) = proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "CreateNewMouse", 
                (name.as_str(), path.as_str())
            ).await?;
            println!("Success: (name input_id output_id)");
            println!("{} {} {}", name, input_id, output_id);
        }
        AppFunction::List => {
            let (list,): (Vec<(String, u32, u32)>,) = proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "ListMice", 
                ()).await?;
            println!("Mice: (name input_id output_id)");
            for (name, input_id, output_id) in list.into_iter() {
                println!("{} {} {}", name, input_id, output_id);
            }
        }
        AppFunction::Stop(name) => {
            proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "StopMouse", 
                (name, )).await?;
        }
        AppFunction::Shutdown => {
            proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "Shutdown", 
                ()).await?;
        }
        AppFunction::PID => {
            let (pid,): (u32,) = proxy.method_call(
                "com.cowsociety.virtual_mouse", 
                "GetProcessID", 
                ()).await?;
            println!("Server Process ID:");
            println!("{}", pid);
        }
    }
    dbus_handle.abort();
    Ok(())
}

async fn server() {
    //create mouse structures
    let communicator = Arc::new(Mutex::new(Communicator::default()));
    let mut manager = MouseManager::new(communicator.clone());

    // Setup DBus connection
    let (resource, conn) = match connection::new_session_sync() {
        Ok(val) => {val}, 
        Err(err) => {panic!("Failed to connect to the D-Bus Session: {}", err)}
    };
    let dbus_handle = tokio::spawn(async {
        resource.await;
    });
    if let Err(err) = conn.request_name("com.cowsociety.virtual_mouse", false, true, false).await {
        panic!("Failed to get name: com.cowsociety.virtual-mouse: {}", err);
    }
    // Setup Crossroads for managing objects and interfaces
    let mut cr = Crossroads::new();
    cr.set_async_support(Some((conn.clone(), Box::new(|x| {tokio::spawn(x);}))));

    
    // General Server commands
    let process_interface = cr.register("com.cowsociety.virtual_mouse", |b: &mut IfaceBuilder<Arc<Mutex<Communicator>>>| {
        b.method_with_cr_async("CreateNewMouse", ("name", "input-path",), ("name", "input-event-id", "output-event-id"), |mut ctx, cr, (name, path,): (String, String,)| {
            let data = cr.data_mut::<Arc<Mutex<Communicator>>>(&"/".into()).unwrap();
            let future = CommunicatorResultFuture{name: name.clone(), handle: data.clone()};
            let mut guard = data.lock().unwrap();
            guard.queued_mice.insert(name.clone(), path.clone());
            if let Some(waker) = guard.work_waker.take() {waker.wake();}
            drop(guard);
            // Create a new mouse object
            async move {
                match future.await{
                    Ok(data) => {
                        return ctx.reply(Ok((data.name, data.input_id, data.output_id,)));
                    },
                    Err(err) => {
                        return ctx.reply(Err(MethodErr::failed(&err.to_string())));
                    }
                }
            }
        });
        b.method("StopMouse", ("name",), (), |_, data,  (name,): (String,)| {
            let mut guard = data.lock().unwrap();
            guard.dequeued_mice.insert(name.to_owned());
            if let Some(waker) = guard.dequeue_waker.take() {waker.wake();}
            Ok(())
        });
        b.method("ListMice", (), ("mice-list",), |_, data, ()| {
            let guard = data.lock().unwrap();
            let mut mice = vec![];
            for (_, info) in guard.current_mice.iter(){
                mice.push((info.name.clone(), info.input_id, info.output_id));
            }
            // Return list of Mice objects
            Ok((mice,))
        });
        b.method("GetProcessID", (), ("pid",), |_, _, ()| {
            // Return the server's process id
            Ok((process::id(),))
        });
        b.method("Shutdown", (), (), |_, data, ()| {
            let mut guard = data.lock().unwrap();
            guard.shutdown.0 = true;
            if let Some(waker) = guard.shutdown.1.take() {waker.wake();}
            Ok(())
        });
    });
    cr.insert("/", &[process_interface], communicator.clone());

    // Add Crossroads to connection
    conn.start_receive(MatchRule::new_method_call(), Box::new(move |msg, conn| {
        cr.handle_message(msg, conn).unwrap();
        true
    }));

    //update mice endlessly
    let local = task::LocalSet::new();
    local.run_until(async move {
        manager.update_loop().await;
    }).await;

    // Disconnect DBus
    dbus_handle.abort();
}
