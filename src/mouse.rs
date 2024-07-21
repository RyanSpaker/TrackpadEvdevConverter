use std::{collections::HashMap, error::Error, fmt::Display, fs::{File, OpenOptions}, os::{fd::OwnedFd, unix::fs::OpenOptionsExt}, path::Path, sync::{Arc, Mutex}};
use evdev::{uinput::{VirtualDevice, VirtualDeviceBuilder}, AttributeSet, Device, EventStream, EventType, InputEvent, Key, RelativeAxisType};
use input::{event::{pointer::{ButtonState, PointerScrollEvent}, PointerEvent}, Event, Libinput, LibinputInterface};
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
use tokio::task::{JoinHandle, LocalSet};
use crate::server::{ServerData, ServerError, WorkFuture};

/// Interface used by Libinput.
pub struct Interface;
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

/// Error representing ways the mouse manager can fail
#[derive(Debug)]
pub enum MouseError{
    MouseDestroyedeBeforeCreation(String, String),
    FailedToAddPathAsLibinputDevice(String),
    FailedToGetInputUdevDevice(String),
    FailedToGetInputDevNode(String),
    FailedToOpenEvdevDevice(String, std::io::Error),
    FailedToCreateEventStream(String, std::io::Error),
    FailedToCreateVirtualDeviceBuilder(std::io::Error),
    FailedToAddRelativeAxes(std::io::Error),
    FailedToAddKeys(std::io::Error),
    FailedToBuildVirtualMouse(std::io::Error),
    FailedToGetOutputPath(Option<std::io::Error>),
    TestSourceReadError(std::io::Error),
    LibinputDispatchError(std::io::Error),
    EmitEventsError(std::io::Error)
}
impl Display for MouseError{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f.write_str(&match self {
            MouseError::MouseDestroyedeBeforeCreation(name, inputpath) => format!("The mouse: {}, with inputpath: {}, was queued for destruction before finishing creation. Not necessarily an error.", *name, *inputpath),
            MouseError::FailedToAddPathAsLibinputDevice(path) => format!("Could not add input device path to the libinput context. Inputpath: {}", *path),
            MouseError::FailedToGetInputUdevDevice(path) => format!("Could not get the UDev device from the input device: {}", *path),
            MouseError::FailedToGetInputDevNode(path) => format!("Could not get devnode path from input udev device: {}", *path),
            MouseError::FailedToOpenEvdevDevice(path, err) => format!("Could not open evdev device: {}, with err: {}", *path, *err),
            MouseError::FailedToCreateEventStream(path, err) => format!("Failed to create event stream from evdev device: {}, with err: {}", *path, *err),
            MouseError::FailedToCreateVirtualDeviceBuilder(err) => format!("Could not create a VirtualDeviceBuilder: {}", *err),
            MouseError::FailedToAddRelativeAxes(err) => format!("Could not add relative axes to VirtualDeviceBuilder: {}", *err),
            MouseError::FailedToAddKeys(err) => format!("Could not add keys to VirtualDeviceBuilder: {}", *err),
            MouseError::FailedToBuildVirtualMouse(err) => format!("Could not build VirtualDeviceBuilder: {}", *err),
            MouseError::FailedToGetOutputPath(err) => format!("Could not get the output path from the virtual device with err: {:?}", *err),
            MouseError::TestSourceReadError(err) => format!("Could not read the next event from the evdev file: {}", *err),
            MouseError::LibinputDispatchError(err) => format!("Could not dispatch libinput source: {}", *err),
            MouseError::EmitEventsError(err) => format!("Could not emit events to the output device: {}", *err)
        });
        Ok(())
    }
}
impl Error for MouseError{}

/// Struct used to manager and update mice.
/// Interacts with the server to create mice, and notify when mice fail
pub struct MouseManager{
    pub mice: HashMap<String, JoinHandle<()>>,
    pub server: Arc<Mutex<ServerData>>
}
impl MouseManager{
    pub fn new(server: Arc<Mutex<ServerData>>) -> Self{Self{mice: HashMap::new(), server}}
    /// Runs the update loop in a local task set
    pub async fn spawn_update_loop(&mut self) -> Result<(), ServerError>{
        let local_set = LocalSet::new();
        let data = self.server.clone();
        local_set.run_until(self.update_loop(data)).await
    }
    /// Asynchronous function which continuosly handles mouse creation and deletion
    pub async fn update_loop(&mut self, server: Arc<Mutex<ServerData>>) -> Result<(), ServerError>{
        loop{
            WorkFuture{data: server.clone()}.await?;
            let Ok(mut guard) = server.lock() else {return Err(ServerError::FailedToLockServerData);};
            // destroy any mice the need to be by aborting their join handles
            let destroy_queue = guard.destroy_queue.clone(); guard.destroy_queue.clear();
            for (name, wakers) in destroy_queue {
                if let Some(handle) = self.mice.remove(&name) {handle.abort();}
                guard.mice.remove(&name);
                for waker in wakers {waker.wake();}
            }
            // create needed mice
            let create_queue = guard.create_queue.clone(); guard.create_queue.clear();
            for (name, (inputpath, waker)) in  create_queue{
                match MouseDriver::new(name.clone(), inputpath).await {
                    Ok(mut mouse) => {
                        guard.mice.insert(name.clone(), (mouse.metadata.input_path.clone(), mouse.metadata.output_path.clone()));
                        let server_copy = server.clone();
                        self.mice.insert(name.clone(), tokio::task::spawn_local(async move {
                            let err = mouse.update_loop().await;
                            if let Ok(mut guard) = server_copy.lock() {
                                guard.update_errors.insert(name, (mouse.metadata.input_path, mouse.metadata.output_path, err));
                                if let Some(waker) = guard.update_error_waker.take() {waker.wake();};
                            }
                        }));
                    },
                    Err(err) => {
                        guard.creation_errors.insert(name, err);
                    }
                }
                if let Some(waker) = waker {waker.wake();}
            }
        }
    }
}

/// Struct containing a virtual mouse's metadata.  
#[derive(Debug, Clone)]
pub struct MouseInfo{
    /// Name of the virtual mouse, either specified in the creation request, or auto generated from the output id
    pub name: String,
    /// evdev event file path for the input device
    pub input_path: String,
    /// evdev event file path for the output device
    pub output_path: String
}

/// Struct containing virtual mouse data.
pub struct MouseDriver{
    /// Name, and event ids of the mouse
    pub metadata: MouseInfo,
    /// Evdev event stream. used to asynchronously wait for input mouse events
    test_source: EventStream,
    /// Libinput event input.
    data_source: Libinput,
    /// Virtual device output
    output: VirtualDevice,
    /// Mouse Position and event tracking data
    movement: MouseMovement
}
impl MouseDriver{
    /// Create a new mouse driver
    pub async fn new(name: String, input: String) -> Result<Self, MouseError>{
        // Get Libinput setup
        let mut data_source = Libinput::new_from_path(Interface);
        let device = data_source.path_add_device(&input).ok_or(MouseError::FailedToAddPathAsLibinputDevice(input.clone()))?;
        // get real inputpath
        let input_path = unsafe{device.udev_device()}
            .ok_or(MouseError::FailedToGetInputUdevDevice(input.clone()))?.devnode()
            .ok_or(MouseError::FailedToGetInputDevNode(input))?.to_string_lossy().to_string();
        // Get evdev test source setup
        let test_source = Device::open(input_path.clone())
            .map_err(|err| {MouseError::FailedToOpenEvdevDevice(input_path.clone(), err)})?
            .into_event_stream().map_err(|err| MouseError::FailedToCreateEventStream(input_path.clone(), err))?;
        // Create the virtual mouse device
        let mut output = VirtualDeviceBuilder::new()
            .map_err(|err| MouseError::FailedToCreateVirtualDeviceBuilder(err))?
            .name(&("VirtualMouse-".to_owned()+&name))
            .with_relative_axes(&AttributeSet::from_iter([
                RelativeAxisType::REL_X,
                RelativeAxisType::REL_Y,
                RelativeAxisType::REL_WHEEL,
                RelativeAxisType::REL_WHEEL_HI_RES,
                RelativeAxisType::REL_HWHEEL,
                RelativeAxisType::REL_HWHEEL_HI_RES
            ])).map_err(|err| MouseError::FailedToAddRelativeAxes(err))?
            .with_keys(&AttributeSet::from_iter([
                Key::BTN_LEFT,
                Key::BTN_RIGHT,
                Key::BTN_MIDDLE
            ])).map_err(|err| MouseError::FailedToAddKeys(err))?
            .build().map_err(|err| MouseError::FailedToBuildVirtualMouse(err))?;
        // Get the output event id
        let output_path = output.enumerate_dev_nodes().await
            .map_err(|err| MouseError::FailedToGetOutputPath(Some(err)))?
            .next_entry().await.map_err(|err| MouseError::FailedToGetOutputPath(Some(err)))?
            .ok_or(MouseError::FailedToGetOutputPath(None))?.to_string_lossy().to_string();
        let output_path = "/dev/input/event".to_string()+output_path.split("event").last().ok_or(MouseError::FailedToGetOutputPath(None))?;

        let metadata = MouseInfo{name, input_path, output_path};

        Ok(Self{
            metadata,
            test_source,
            data_source,
            output,
            movement: MouseMovement::default()
        })
    }
    /// Asynchronously waits for the next event to happen for the trackpad input device
    pub async fn await_sync_event(&mut self) -> Result<(), MouseError>{
        self.test_source.next_event().await.map_err(|err| MouseError::TestSourceReadError(err))?;
        Ok(())
    }
    /// Poll function to update the mouse endlessly until it errors out
    pub async fn update_loop(&mut self) -> MouseError {
        loop{
            if let Err(err) = self.await_sync_event().await {return err;};

            if let Err(err) = self.data_source.dispatch().map_err(|err| MouseError::LibinputDispatchError(err)) {return err;};

            let events: Vec<Event> = self.data_source.by_ref().collect();
            for event in events{
                self.movement.process_event(event);
            }
            // emit mouse events
            let events = self.movement.get_output_events();
            if events.len() > 0 {
                if let Err(err) = self.output.emit(&events).map_err(|err| MouseError::EmitEventsError(err)) {return err;};
            }
        }
    }  
}

/// Struct containing Mouse tracking data
#[derive(Default, Debug, Clone)]
pub struct MouseMovement{
    /// Delta x of mouse pointer location since last event was sent
    relx: f64,
    /// Delta y of mouse pointer location since last event was sent
    rely: f64,
    /// Delta scroll of the mouse since the last event was sent
    rel_scroll: f64,
    /// Delta scroll of the mouse with high resolution (normal*120) since the last event was sent
    rel_scroll_hr: f64,
    /// Delta horizontal scroll fo the mouse since the last event was sent
    rel_hscroll: f64,
    /// Delta horizontal scroll of the mouse with high resolution (normal*120) since the last event was sent
    rel_hscroll_hr: f64,
    /// 0 if the left click has been released, 1 if pressed, none otherwise
    left_button_event: Option<i32>,
    /// 0 if the right click has been released, 1 if pressed, none otherwise
    right_button_event: Option<i32>,
    /// 0 if the middle click has been released, 1 if pressed, none otherwise
    middle_button_event: Option<i32>,
}
impl MouseMovement{
    /// Reads in an event, and updates the movement values accordingly
    pub fn process_event(&mut self, event: Event) {
        match event{
            Event::Pointer(PointerEvent::Motion(ev)) => {
                self.relx += ev.dx();
                self.rely += ev.dy();
            },
            Event::Pointer(PointerEvent::Button(ev)) => {
                match ev.button() {
                    272 => {self.left_button_event = Some(match ev.button_state() {ButtonState::Pressed => 1, ButtonState::Released => 0});}
                    273 => {self.right_button_event = Some(match ev.button_state() {ButtonState::Pressed => 1, ButtonState::Released => 0});}
                    274 => {self.middle_button_event = Some(match ev.button_state() {ButtonState::Pressed => 1, ButtonState::Released => 0});}
                    _ => {}
                };
            },
            Event::Pointer(PointerEvent::ScrollFinger(ev)) => {
                if ev.has_axis(input::event::pointer::Axis::Vertical) {
                    self.rel_scroll += ev.scroll_value(input::event::pointer::Axis::Vertical)*-0.05;
                    self.rel_scroll_hr += ev.scroll_value(input::event::pointer::Axis::Vertical)*120.0*-0.05;
                }
                if ev.has_axis(input::event::pointer::Axis::Horizontal) {
                    self.rel_hscroll += ev.scroll_value(input::event::pointer::Axis::Horizontal)*-0.05;
                    self.rel_hscroll_hr += ev.scroll_value(input::event::pointer::Axis::Horizontal)*120.0*-0.05;
                }
            },
            _ => {}
        };
    }
    /// reduce delta changes of the mouse, returning the list of input event containing the reduction
    pub fn get_output_events(&mut self) -> Vec<InputEvent>{
        let mut event_storage = Vec::with_capacity(8);
        if let Some(val) = self.left_button_event.take(){
            event_storage.push(InputEvent::new(EventType::KEY, Key::BTN_LEFT.code(), val));
        }
        if let Some(val) = self.right_button_event.take(){
            event_storage.push(InputEvent::new(EventType::KEY, Key::BTN_RIGHT.code(), val));
        }
        if let Some(val) = self.middle_button_event.take(){
            event_storage.push(InputEvent::new(EventType::KEY, Key::BTN_MIDDLE.code(), val));
        }
        if self.rel_scroll.abs() >= 1.0 {
            event_storage.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, self.rel_scroll.trunc() as i32));
            self.rel_scroll = self.rel_scroll.fract();
        }
        if self.rel_scroll_hr.abs() >= 1.0 {
            event_storage.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL_HI_RES.0, self.rel_scroll_hr.trunc() as i32));
            self.rel_scroll_hr = self.rel_scroll_hr.fract();
        }
        if self.rel_hscroll.abs() >= 1.0 {
            event_storage.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL.0, self.rel_hscroll.trunc() as i32));
            self.rel_hscroll = self.rel_hscroll.fract();
        }
        if self.rel_hscroll_hr.abs() >= 1.0 {
            event_storage.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL_HI_RES.0, self.rel_hscroll_hr.trunc() as i32));
            self.rel_hscroll_hr = self.rel_hscroll_hr.fract();
        }
        if self.relx.abs() >= 1.0 {
            event_storage.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, self.relx.trunc() as i32));
            self.relx = self.relx.fract();
        }
        if self.rely.abs() >= 1.0 {
            event_storage.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, self.rely.trunc() as i32));
            self.rely = self.rely.fract();
        }
        return event_storage;
    }
}
