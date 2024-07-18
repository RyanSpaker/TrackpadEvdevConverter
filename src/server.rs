/*
This is the server module
It is responsible for setting up the dbus server
The server allows the cli tool to request new mice, delete mice, list mice, and clear mice
It also sets up the object, ServerData, which is used for communication with the mouse manager
*/
use std::{collections::{HashMap, HashSet}, error::Error, fmt::Display, sync::{Arc, Mutex}, task::{Poll, Waker}, time::Duration};
use dbus::{channel::MatchingReceiver, message::MatchRule, nonblock::{Proxy, SyncConnection}, MethodErr};
use dbus_crossroads::{Crossroads, IfaceBuilder};
use dbus_tokio::connection::{self, IOResourceError};
use futures::Future;
use tokio::task::JoinHandle;
use crate::mouse::MouseError;

/// Error representing ways the server can fail
#[derive(Debug)]
pub enum ServerError{
    DBusConnectionFailed(dbus::Error),
    ServerRequestNameFailed(dbus::Error),
    FailedToFindServerData,
    FailedToLockServerData,
    SpecifiedMouseDNE(String),
    SpecifiedNameTaken(String),
    MouseError(MouseError),
    MouseLost(String)
}
impl Display for ServerError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f.write_str(&match self {
            ServerError::DBusConnectionFailed(err) => format!("Could not create system dbus connection. DBus error: {}", err),
            ServerError::ServerRequestNameFailed(err) => format!("Could not aqcuire the dbus name, the server may already be running, or dbus permissions are not configured correctly. DBus Error: {:?}", err),
            ServerError::FailedToFindServerData => format!("Could not find the ServerData object from the crossroads object"),
            ServerError::FailedToLockServerData => format!("Could not lock ServerData mutex due to poison error"),
            ServerError::SpecifiedMouseDNE(name) => format!("The Specified mouse: {}, was not found", *name),
            ServerError::SpecifiedNameTaken(name) => format!("The name specified: {}, is already taken by a mouse", *name),
            ServerError::MouseError(err) => format!("Failure due to MouseError: {}", *err),
            ServerError::MouseLost(name) => format!("Mouse: {}, was not created, but no error was reported", *name)
        });
        Ok(())
    }
}
impl Error for ServerError{}

/// Holds all structs needed for the server
pub struct ServerState{
    pub data: Arc<Mutex<ServerData>>,
    pub conn: Arc<SyncConnection>,
    pub dbus_handle: JoinHandle<IOResourceError>,
    pub update_error_handle: JoinHandle<ServerError>
}

/// Server code
pub async fn server() -> Result<ServerState, ServerError> {
    // Setup DBus connection
    let (r, conn) = connection::new_system_sync()
        .map_err(|err| ServerError::DBusConnectionFailed(err))?;
    let dbus_handle = tokio::spawn(r);

    //Setup server
    let data = define_server(conn.clone()).await?;

    // setup update error handler
    let data_copy = data.clone();
    let proxy = Proxy::new("org.cws.VirtualMouse", "/org/cws/VirtualMouse", Duration::from_secs(2), conn.clone());
    let update_error_handle = tokio::spawn(async move {
        loop{
            let errs = match (UpdateErrorFuture{data: data_copy.clone()}).await{
                Ok(errs) => errs,
                Err(err) => {return err}
            };
            for (name, (inputpath, outputpath, err)) in errs{
                println!("Mouse: {}, with inputpath {}, and outpath {}, failed to update with err {}", name, inputpath, outputpath, err);
                let _ = proxy.method_call::<(), _, _, _>("org.cws.VirtualMouse.Manager", "SendRemovedSignal", (name, inputpath, outputpath)).await;
            }
        }
    });

    Ok(ServerState { data, conn, dbus_handle, update_error_handle })
}

/// Struct to store data held by the server
#[derive(Default)]
pub struct ServerData{
    /// Map of mice: name -> (inputpath, outputpath)
    pub mice: HashMap<String, (String, String)>,
    /// Set of mice names that are queued to be destroyed. maps name -> waker
    pub destroy_queue: HashMap<String, Vec<Waker>>,
    /// Map of mice to be created, name -> (inputpath, waker)
    pub create_queue: HashMap<String, (String, Option<Waker>)>,
    /// waker used by the mouse manager to await any mice that need to be destroyed or created
    pub work_waker: Option<Waker>,
    /// Map of mouse name -> error thrown while creating, or updating
    pub creation_errors: HashMap<String, MouseError>,
    /// Map of mouse name -> (inputpath, outputpath, error) filled when mice error out while updating
    pub update_errors: HashMap<String, (String, String, MouseError)>,
    /// Waker for when update errors occur
    pub update_error_waker: Option<Waker>
} 
pub trait ServerOperations{
    /// queues a mouse for creation, returns a future which waits for the mouse to be created
    fn create_mouse(&self, name: String, inputpath: String) -> impl std::future::Future<Output = Result<(String, String, String), ServerError>> + Send;
    /// queues a mouse for destruction, returns a future which waits for the mouse to be destroyed
    fn destroy_mouse(&self, name: String) -> impl std::future::Future<Output = Result<(String, String, String), ServerError>> + Send;
    /// queues a mouse for destruction, returns a future which waits for the mouse to be destroyed
    fn clear_mice(&self) -> impl std::future::Future<Output = Result<Vec<(String, String, String)>, ServerError>> + Send;
}
impl ServerOperations for Arc<Mutex<ServerData>>{
    async fn create_mouse(&self, name: String, inputpath: String) -> Result<(String, String, String), ServerError> {
        if let Ok(mut guard) = self.lock() {
            if guard.mice.contains_key(&name) || guard.create_queue.contains_key(&name) {
                return Err(ServerError::SpecifiedNameTaken(name));
            }
            guard.create_queue.insert(name.clone(), (inputpath, None));
            if let Some(waker) = guard.work_waker.take() {waker.wake();}
        }else {return Err(ServerError::FailedToLockServerData);}
        CreateMouseFuture{name, data: self.clone()}.await
    }
    async fn destroy_mouse(&self, name: String) -> Result<(String, String, String), ServerError> {
        let mouse = if let Ok(mut guard) = self.lock(){
            let mouse = if let Some((inputpath, outputpath)) = guard.mice.get(&name) {
                (name.clone(), inputpath.to_owned(), outputpath.to_owned())
            } else {
                if let Some((inputpath, waker)) = guard.create_queue.remove(&name) {
                    if let Some(waker) = waker {waker.wake();}
                    guard.creation_errors.insert(name.clone(), MouseError::MouseDestroyedeBeforeCreation(name.clone(), inputpath.clone()));
                    return Err(ServerError::MouseError(MouseError::MouseDestroyedeBeforeCreation(name, inputpath)));
                } else {
                    return Err(ServerError::SpecifiedMouseDNE(name));
                }
            };
            if !guard.destroy_queue.contains_key(&name) {guard.destroy_queue.insert(name, vec![]);}
            if let Some(waker) = guard.work_waker.take() {waker.wake();}
            mouse
        }else {return Err(ServerError::FailedToLockServerData);};
        DestroyMouseFuture{mouse, data: self.clone()}.await
    }
    async fn clear_mice(&self) -> Result<Vec<(String, String, String)>, ServerError> {
        let (mice, names) = if let Ok(mut guard) = self.lock(){
            let half_mice = guard.create_queue.drain().collect::<Vec<(String, (String, Option<Waker>))>>();
            for (name, (inputpath, waker)) in half_mice {
                guard.creation_errors.insert(name.clone(), MouseError::MouseDestroyedeBeforeCreation(name, inputpath));
                if let Some(waker) = waker {waker.wake();}
            }
            let mice = guard.mice.clone().into_iter().map(|(a, (b, c))| (a, b, c)).collect::<Vec<(String, String, String)>>();
            let names = mice.iter().map(|(name, _, _)| name.to_owned()).collect::<HashSet<String>>();
            for name in names.iter(){
                if !guard.destroy_queue.contains_key(name) {guard.destroy_queue.insert(name.to_owned(), vec![]);}
            }
            if let Some(waker) = guard.work_waker.take() {waker.wake();}
            (mice, names)
        }else {return Err(ServerError::FailedToLockServerData);};
        DestroyMiceFuture{mice, names, data: self.clone()}.await
    }
}

/// Future which waits for any work to be ready
pub struct WorkFuture{
    pub data: Arc<Mutex<ServerData>>
}
impl Future for WorkFuture{
    type Output = Result<(), ServerError>;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if let Ok(mut guard) = self.data.lock() {
            if guard.create_queue.is_empty() && guard.destroy_queue.is_empty() {
                let _ = guard.work_waker.insert(cx.waker().clone());
                Poll::Pending
            }else {Poll::Ready(Ok(()))}
        }else {Poll::Ready(Err(ServerError::FailedToLockServerData))}
    }
}

/// Future which waits for new update errors, and then returns them
pub struct UpdateErrorFuture{
    pub data: Arc<Mutex<ServerData>>
}
impl Future for UpdateErrorFuture{
    type Output = Result<HashMap<String, (String, String, MouseError)>, ServerError>;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let Ok(mut guard) = self.data.lock() else {return Poll::Ready(Err(ServerError::FailedToLockServerData))};
        if guard.update_errors.is_empty() {
            let _ = guard.update_error_waker.insert(cx.waker().clone()); 
            Poll::Pending
        } else {
            Poll::Ready(Ok(guard.update_errors.drain().collect()))
        }
    }
}

/// Future which waits for a mouse to be deleted, then returns the now unused, metadata
pub struct CreateMouseFuture{
    pub name: String,
    pub data: Arc<Mutex<ServerData>>
}
impl Future for CreateMouseFuture{
    type Output = Result<(String, String, String), ServerError>;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let Ok(mut guard) = self.data.lock() else {return Poll::Ready(Err(ServerError::FailedToLockServerData));};
        if let Some((_, waker)) = guard.create_queue.get_mut(&self.name) {
            let _ = waker.insert(cx.waker().clone());
            Poll::Pending
        }else {
            // Return the mouse, or a creation err, or if all else fails, a mouse lost err
            Poll::Ready(guard.mice.get(&self.name).cloned()
                .map(|(i, o)| Ok((self.name.clone(), i, o)))
                .or_else(|| {guard.creation_errors.remove(&self.name).map(|err| Err(ServerError::MouseError(err)))})
                .unwrap_or(Err(ServerError::MouseLost(self.name.clone()))))
        }
    }
}

/// Future which waits for a mouse to be deleted, then returns the now unused, metadata
pub struct DestroyMouseFuture{
    pub mouse: (String, String, String),
    pub data: Arc<Mutex<ServerData>>
}
impl Future for DestroyMouseFuture{
    type Output = Result<(String, String, String), ServerError>;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let Ok(mut guard) = self.data.lock() else {return Poll::Ready(Err(ServerError::FailedToLockServerData));};
        if guard.destroy_queue.contains_key(&self.mouse.0) {
            guard.destroy_queue.get_mut(&self.mouse.0).unwrap().push(cx.waker().clone());
            Poll::Pending
        }else {
            Poll::Ready(Ok(self.mouse.clone()))
        }
    }
}

/// Future which waits for a mouse to be deleted, then returns the now unused, metadata
pub struct DestroyMiceFuture{
    pub mice: Vec<(String, String, String)>,
    pub names: HashSet<String>,
    pub data: Arc<Mutex<ServerData>>
}
impl Future for DestroyMiceFuture{
    type Output = Result<Vec<(String, String, String)>, ServerError>;
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let mut future = self.as_mut();
        let data = future.data.clone();
        let Ok(mut guard) = data.lock() else {return Poll::Ready(Err(ServerError::FailedToLockServerData))};
        future.names.retain(|name| guard.destroy_queue.contains_key(name));
        if !future.names.is_empty(){
            guard.destroy_queue.iter_mut().for_each(|(_, wakers)| {
                wakers.push(cx.waker().clone());
            });
            Poll::Pending
        }else {
            Poll::Ready(Ok(self.mice.clone()))
        }
    }
}

/// Sets up the server
pub async fn define_server(conn: Arc<SyncConnection>) -> Result<Arc<Mutex<ServerData>>, ServerError> {
    // get dbus name
    conn.request_name("org.cws.VirtualMouse", false, false, true).await
        .map_err(|err| ServerError::ServerRequestNameFailed(err))?;
    // setup crossroads for managing interface
    let mut cr = Crossroads::new();
    cr.set_async_support(Some((conn.clone(), Box::new(|x| {tokio::spawn(x);}))));
    // define main interface
    let manager = cr.register("org.cws.VirtualMouse.Manager", |b: &mut IfaceBuilder<Arc<Mutex<ServerData>>>| {
        b.signal::<(String, String, String),_>("MouseAdded", ("Name", "InputPath", "OutputPath"));
        b.signal::<(String, String, String),_>("MouseRemoved", ("Name", "InputPath", "OutputPath"));
        b.signal::<(String, String, String, String),_>("MouseFailed", ("Name", "InputPath", "OutputPath", "Error"));
        b.method_with_cr_async("CreateMouse", ("Name", "InputPath"), ("Name", "InputPath", "OutputPath",), 
        |mut ctx, cr, (name, path,): (String, String,)| {
            let object = cr.data_mut::<Arc<Mutex<ServerData>>>(&"/org/cws/VirtualMouse".into()).cloned();
            async move {
                let Some(data) = object else {return ctx.reply(Err(MethodErr::failed(&ServerError::FailedToFindServerData)));};
                let output = match data.create_mouse(name, path).await {
                    Ok(mouse) => {
                        ctx.push_msg(ctx.make_signal("MouseAdded", mouse.clone()));
                        Ok(mouse)
                    },
                    Err(err) => {Err(MethodErr::failed(&err))}
                };
                ctx.reply(output)
            }
        });
        b.method_with_cr_async("DestroyMouse", ("Name",), ("Name", "InputPath", "OutputPath"), 
        |mut ctx, cr, (name,): (String,)| {
            let object = cr.data_mut::<Arc<Mutex<ServerData>>>(&"/org/cws/VirtualMouse".into()).cloned();
            async move {
                let Some(data) = object else {return ctx.reply(Err(MethodErr::failed(&ServerError::FailedToFindServerData)));};
                let output = match data.destroy_mouse(name).await {
                    Ok(mouse) => {
                        ctx.push_msg(ctx.make_signal("MouseRemoved", mouse.clone()));
                        Ok(mouse)
                    },
                    Err(ServerError::MouseError(MouseError::MouseDestroyedeBeforeCreation(name, inputpath))) => {Ok((name, inputpath, "".to_string()))},
                    Err(err) => {Err(MethodErr::failed(&err))}
                };
                ctx.reply(output)
            }
        });
        b.method_with_cr_async("ClearMice", (), (), 
        |mut ctx, cr, _: ()| {
            let object = cr.data_mut::<Arc<Mutex<ServerData>>>(&"/org/cws/VirtualMouse".into()).cloned();
            async move {
                let Some(data) = object else {return ctx.reply(Err(MethodErr::failed(&ServerError::FailedToFindServerData)));};
                let output = match data.clear_mice().await {
                    Ok(mice) => {
                        mice.into_iter().for_each(|mouse| ctx.push_msg(ctx.make_signal("MouseRemoved", mouse)));
                        Ok(())
                    },
                    Err(err) => {Err(MethodErr::failed(&err))}
                };
                ctx.reply(output)
            }
        });
        b.method::<_, (Vec<(String, String, String)>,), _, _>("ListMice", (), ("Mice",), 
        |_, data, _: ()| {
            let guard = data.lock().map_err(|_| MethodErr::failed(&ServerError::FailedToLockServerData))?;
            Ok((guard.mice.to_owned().into_iter().map(|(a, (b, c))| (a, b, c)).collect(),))
        });
        b.method("GetMouse", ("Name",), ("Name", "InputPath", "OutputPath"), 
        |_, data, (name,): (String,)| {
            let guard = data.lock().map_err(|_| MethodErr::failed(&ServerError::FailedToLockServerData))?;
            match guard.mice.get(&name) {
                Some((inputpath, outputpath)) => {Ok((name, inputpath.to_owned(), outputpath.to_owned()))},
                None => {Err(MethodErr::failed(&ServerError::SpecifiedMouseDNE(name)))}
            }
        });
        b.method("SendRemovedSignal", ("Name", "InputPath", "OutputPath"), (), 
        |ctx, _, mouse: (String, String, String)| {
            ctx.push_msg(ctx.make_signal("MouseRemoved", mouse));
            Ok(())
        });
    });
    let server_data = Arc::new(Mutex::new(ServerData::default()));
    cr.insert("/org/cws/VirtualMouse", &[manager, cr.introspectable(), cr.properties()], server_data.clone());
    // start handling interface functions
    conn.start_receive(MatchRule::new_method_call(), Box::new(move |msg, conn| {
        cr.handle_message(msg, conn).unwrap();
        true
    }));
    Ok(server_data)
}