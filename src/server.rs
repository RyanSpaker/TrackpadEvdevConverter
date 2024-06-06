use std::{error::Error, fmt::Display, process, sync::{Arc, Mutex}};
use dbus::{message::MatchRule, MethodErr, channel::MatchingReceiver};
use dbus_crossroads::{Crossroads, IfaceBuilder};
use dbus_tokio::connection;
use tokio::task;
use crate::{communicator::{Communicator, CommunicatorResultFuture}, manager::MouseManager};

/// Error representing ways the server can fail
#[derive(Debug)]
pub enum ServerError{
    DBusConnectionFailed(dbus::Error),
    ServerRequestNameFailed(dbus::Error)
}
impl Display for ServerError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let string = match self {
            ServerError::DBusConnectionFailed(err) => format!("Could not create system dbus connection. DBus error: {}", err),
            ServerError::ServerRequestNameFailed(err) => format!("Could not aqcuire the dbus name, the server may already be running, or dbus permissions are not configured correctly. DBus Error: {:?}", err)
        };
        f.write_str(string.as_str())?;
        Ok(())
    }
}
impl Error for ServerError{}


/// Server code
pub async fn server() -> Result<(), Box<dyn Error>> {
    // Create mouse structures
    let communicator = Arc::new(Mutex::new(Communicator::default()));
    let mut manager = MouseManager::new(communicator.clone());

    // Setup DBus connection
    let (resource, conn) = connection::new_system_sync()
        .map_err(|err| ServerError::DBusConnectionFailed(err))?;
    let dbus_handle = tokio::spawn(async {
        resource.await
    });

    // Grab dbus name, fails if already taken or not configured
    conn.request_name("com.cowsociety.virtual_mouse", false, false, false).await
        .map_err(|err| ServerError::ServerRequestNameFailed(err))?;

    // Setup Crossroads for managing objects and interfaces
    let mut cr = Crossroads::new();
    cr.set_async_support(Some((conn.clone(), Box::new(|x| {tokio::spawn(x);}))));

    
    // General Server commands
    let process_interface = cr.register("com.cowsociety.virtual_mouse", |b: &mut IfaceBuilder<Arc<Mutex<Communicator>>>| {
        b.method_with_cr_async("CreateNewMouse", ("name", "input-path",), ("name", "input-event-id", "output-event-id", "libinput-id"), |mut ctx, cr, (name, path,): (String, String,)| {
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
                        return ctx.reply(Ok((data.name, data.input_id, data.output_id, data.libinput_id)));
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
                mice.push((info.name.clone(), info.input_id, info.output_id, info.libinput_id));
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
        b.method("Reset", (), (), |_, data, ()| {
            let mut guard = data.lock().unwrap();
            let names: Vec<String> = guard.current_mice.keys().cloned().collect();
            guard.dequeued_mice.extend(names);
            if let Some(waker) = guard.dequeue_waker.take() {waker.wake();}
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

    Ok(())
}
