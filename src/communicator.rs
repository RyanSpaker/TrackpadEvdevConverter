use std::{collections::{HashMap, HashSet}, future::Future, sync::{Arc, Mutex}, task::{Poll, Waker}};

use crate::mouse::{MouseCreationError, MouseInfo};


/// A struct used to facilitate communication between the non send mouse driver, and the DBus listener threads
#[derive(Debug, Default)]
pub struct Communicator{
    /// Hashmap of queued mice, name -> evdev event path
    pub queued_mice: HashMap<String, String>,
    /// Hashmap of currently simulated mice, name -> mouse info
    pub current_mice: HashMap<String, MouseInfo>,
    /// Hashmap of errors from the mouse creation process, name -> error message
    pub errors: HashMap<String, MouseCreationError>,
    /// Handle to a waker that should be called any time a new queued mice is added.
    pub work_waker: Option<Waker>,
    /// Handle to wakers that should be called when a queued mice has finished being processed
    pub result_wakers: HashMap<String, Waker>,
    /// whether or not to shutdown the system, and a waker to call when you set the bool to true
    pub shutdown: (bool, Option<Waker>),
    /// Set of mice names to stop
    pub dequeued_mice: HashSet<String>,
    /// Waker that should be called when mice are added to dequeued_mice
    pub dequeue_waker: Option<Waker>
}

/// Future which waits for the communicator to request a shutdown. places a waker into the communicator which should be used by anything that sets shutdown to true
pub struct CommunicatorShutdownFuture{
    pub com: Arc<Mutex<Communicator>>
}
impl Future for CommunicatorShutdownFuture{
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut communicator = self.com.lock().unwrap();
        if communicator.shutdown.0 {return Poll::Ready(());}
        communicator.shutdown.1 = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Future which waits for the communicator to have mice to stop. Places waker in the communicator, needs to be woken by anything that adds dequeued mice
pub struct CommunicatorDequeueFuture{
    pub com: Arc<Mutex<Communicator>>
}
impl Future for CommunicatorDequeueFuture{
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut communicator = self.com.lock().unwrap();
        if !communicator.dequeued_mice.is_empty() {return Poll::Ready(());}
        communicator.dequeue_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Future which waits for the communicator to have queued mice. Places waker in the communicator, needs to be woken by anyhting that adds queued mice
pub struct CommunicatorWorkFuture{
    pub com: Arc<Mutex<Communicator>>
}
impl Future for CommunicatorWorkFuture{
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut communicator = self.com.lock().unwrap();
        if !communicator.queued_mice.is_empty() {return Poll::Ready(());}
        communicator.work_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Struct used to represent a future that waits until the communicator has finished processing a specific queued mouse
pub struct CommunicatorResultFuture{
    /// Name of the mouse this future is waiting on
    pub name: String,
    pub handle: Arc<Mutex<Communicator>>
}
impl Future for CommunicatorResultFuture{
    type Output = Result<MouseInfo, MouseCreationError>;
    // Aquires the mutex, checks to see if the communicator has either the mouse data or an error message
    // Waker should be called by the Mouse Manager when a queued mice is finished being processed
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut communicator  = self.handle.lock().unwrap();
        if communicator.queued_mice.contains_key(&self.name) {
            communicator.result_wakers.insert(self.name.clone(), cx.waker().clone());
            return Poll::Pending;
        }
        if let Some(info) = communicator.current_mice.get(&self.name) {
            return Poll::Ready(Ok(info.clone()));
        }
        if let Some(err) = communicator.errors.remove(&self.name){
            return Poll::Ready(Err(err));
        }
        Poll::Ready(Err(MouseCreationError::AsyncProgramError))
    }
}
