use std::{collections::HashMap, sync::{Arc, Mutex}, task::{Poll, Waker}};
use futures::Future;
use tokio::task::JoinHandle;

use crate::{communicator::{Communicator, CommunicatorDequeueFuture, CommunicatorShutdownFuture, CommunicatorWorkFuture}, mouse::{MouseCreationError, MouseDriver, MouseDriverUpdateError, MouseInfo}};

/// Struct holding mouse information used by mouse manager
pub struct ManagedMouse{
    pub metadata: MouseInfo,
    pub driver: Arc<tokio::sync::Mutex<MouseDriver>>,
    pub task: Option<JoinHandle<()>>,
    pub abort: Arc<Mutex<AbortData>>
}

/// Struct holding abort data for a managed mouse.
pub struct AbortData{
    /// Whether or not this mouse needs to be aborted
    pub abort: bool,
    /// The error returned by the mouse driver update function
    pub err: Option<MouseDriverUpdateError>
}

/// Future which waits for abort to be true
pub struct ManagerAbortFuture{
    abort: Arc<Mutex<bool>>,
    waker: Arc<Mutex<Option<Waker>>>
}
impl Future for ManagerAbortFuture{
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        if *self.abort.lock().unwrap() {return Poll::Ready(());}
        let mut waker = self.waker.lock().unwrap();
        *waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Struct to hold and update all virtual mice
pub struct MouseManager{
    /// Map from mouse name to mouse driver
    mice: HashMap<String, ManagedMouse>,
    communicator: Arc<Mutex<Communicator>>,
    /// bool for whether or not a mouse needs to be aborted
    abort: Arc<Mutex<bool>>,
    /// waker used to inform the system that the abort value changed
    abort_waker: Arc<Mutex<Option<Waker>>>
}
impl MouseManager{
    /// Returns empty new mouse manager
    pub fn new(com: Arc<Mutex<Communicator>>) -> Self{
        MouseManager { mice: HashMap::default(), communicator: com, abort: Arc::new(Mutex::new(false)), abort_waker: Arc::new(Mutex::new(None)) }
    }
    /// creates any queued mice
    pub fn create_queued_mice(&mut self) {
        let mut com = self.communicator.lock().unwrap();
        let queued: Vec<(String, String)> = com.queued_mice.drain().collect();
        for (name, path) in queued {
            if self.mice.contains_key(&name) {
                com.errors.insert(name.to_owned(), MouseCreationError::NameInUse);
            }else{
                match MouseDriver::new(name.clone(), path){
                    Ok(mouse) => {
                        let info = mouse.metadata.clone();
                        mouse.lock();
                        let handle = Arc::new(tokio::sync::Mutex::new(mouse));
                        let abort = Arc::new(Mutex::new(AbortData{abort: false, err: None}));
                        let future_handle = handle.clone();
                        let future_abort = abort.clone();
                        let future_uni_abort = self.abort.clone();
                        let future_uni_abort_waker = self.abort_waker.clone();
                        let task = tokio::task::spawn_local(async move {
                            let mut mouse = future_handle.lock().await;
                            let err = mouse.update_loop().await;
                            drop(mouse);
                            let mut abort = future_abort.lock().unwrap();
                            abort.abort = true;
                            abort.err = Some(err);
                            let mut abort = future_uni_abort.lock().unwrap();
                            *abort = true;
                            let mut abort_waker = future_uni_abort_waker.lock().unwrap();
                            if let Some(waker) = abort_waker.take(){
                                waker.wake();
                            }
                        });
                        self.mice.insert(name.clone(), ManagedMouse{metadata: info.clone(), driver: handle, task: Some(task), abort});
                        com.current_mice.insert(name.clone(), info);
                    },
                    Err(err) => {
                        com.errors.insert(name.clone(), err);
                    }
                };
            }
            if let Some(waker) = com.result_wakers.remove(&name) {waker.wake();}
        }
    }
    /// Aborts all mice that need to be
    pub fn abort_mice(&mut self) {
        let mut aborted_mice: Vec<String> = vec![];
        for (name, mouse) in self.mice.iter_mut(){
            let mut abort = mouse.abort.lock().unwrap();
            if !abort.abort {continue;}
            let error = abort.err.take();
            drop(abort);
            if let Some(task) = mouse.task.take(){
                task.abort();
            }
            let driver = mouse.driver.blocking_lock();
            driver.unlock();
            if let Some(err) = error{
                println!("Mouse {} Aborted with error: {:?}", *name, err);
            }
            aborted_mice.push(name.to_owned());
        }
        aborted_mice.into_iter().for_each(|name| {self.mice.remove(&name);});
    }
    /// Aborts all mice
    pub async fn shutdown(&mut self) {
        for (name, mouse) in self.mice.iter_mut(){
            let mut abort = mouse.abort.lock().unwrap();
            let error = abort.err.take();
            drop(abort);
            if let Some(task) = mouse.task.take(){
                task.abort();
            }
            let driver = mouse.driver.lock().await;
            driver.unlock();
            if let Some(err) = error{
                println!("Mouse {} Aborted with error: {:?}", *name, err);
            }
        }
        self.mice.clear();
    }
    /// Removes any dequeued mice from the system
    pub async fn stop_mice(&mut self) {
        let mut com = self.communicator.lock().unwrap();
        let queued: Vec<String> = com.dequeued_mice.drain().collect();
        for name in queued {
            com.current_mice.remove(&name);
            let mut managed_mouse = if let Some(mouse) = self.mice.remove(&name) {mouse} else {continue;};
            if let Some(task) = managed_mouse.task.take(){
                task.abort();
            }
            let driver = managed_mouse.driver.lock().await;
            driver.unlock();
        }
    }
    /// asynchronous update loop for the mouse manager
    pub async fn update_loop(&mut self) {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        loop{
            let queued_future = CommunicatorWorkFuture{com: self.communicator.clone()};
            let abort_future = ManagerAbortFuture{abort: self.abort.clone(), waker: self.abort_waker.clone()};      
            let shutdown_future = CommunicatorShutdownFuture{com: self.communicator.clone()};  
            let dequeue_future = CommunicatorDequeueFuture{com: self.communicator.clone()};
            tokio::select! {
                _ = queued_future => {
                    self.create_queued_mice();
                }
                _ = abort_future => {
                    self.abort_mice();
                }
                _ = shutdown_future => {
                    self.shutdown().await;
                    break;
                }
                _ = dequeue_future => {
                    self.stop_mice().await;
                }
                _ = tokio::signal::ctrl_c() => {
                    self.shutdown().await;
                    break;
                }
                _ = sigterm.recv() => {
                    self.shutdown().await;
                    break;
                }
            }
        }
    }
}