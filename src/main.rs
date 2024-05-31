use std::{env::args, fs::{File, OpenOptions}, os::{fd::OwnedFd, unix::fs::OpenOptionsExt}, path::Path, process, sync::{Arc, Mutex}};
use communicator::{Communicator, CommunicatorResultFuture};
use dbus::{channel::MatchingReceiver, message::MatchRule, nonblock, MethodErr};
use dbus_crossroads::{Crossroads, IfaceBuilder};
use dbus_tokio::connection;
use input::{Libinput, LibinputInterface};
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
use manager::MouseManager;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = args().skip(1).collect::<Vec<String>>();
    if arguments.len() == 0 || arguments[0] == "--server" {
        server().await;
    }
    //client
    
    // Setup DBus connection
    let (resource, conn) = match connection::new_session_sync() {
        Ok(val) => {val}, 
        Err(err) => {panic!("Failed to connect to the D-Bus Session: {}", err)}
    };
    let _handle = tokio::spawn(async {
        let err = resource.await;
        panic!("Lost connection to D-Bus: {}", err);
    });

    let mut data_source = Libinput::new_from_path(Interface);
    let device = data_source.path_add_device("/dev/input/by-path/pci-0000:00:15.0-platform-i2c_designware.0-event-mouse");
    println!("device: {:?}", device);

    let proxy = nonblock::Proxy::new("com.cowsociety.virtual_mouse", "/", std::time::Duration::from_secs(2), conn.clone());
    let (name, input_id, output_id): (String, u32, u32) = proxy.method_call("com.cowsociety.virtual_mouse", "CreateNewMouse", ("Test", "/dev/input/by-path/pci-0000:00:15.0-platform-i2c_designware.0-event-mouse")).await.expect("Error");
    println!("New Mouse: {} {} {}", name, input_id, output_id);
    let (list,): (Vec<(String, u32, u32)>,) = proxy.method_call("com.cowsociety.virtual_mouse", "ListMice", ()).await.expect("Error");
    println!("Mice: {:?}", list);

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
    let _handle = tokio::spawn(async {
        let err = resource.await;
        panic!("Lost connection to D-Bus: {}", err);
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
    cr.insert("/", &[process_interface], communicator);

    // Add Crossroads to connection
    conn.start_receive(MatchRule::new_method_call(), Box::new(move |msg, conn| {
        cr.handle_message(msg, conn).unwrap();
        true
    }));

    //update mice endlessly
    manager.update_loop().await;
}
