use std::path::PathBuf;
use evdev::{uinput::{VirtualDevice, VirtualDeviceBuilder}, AttributeSet, Device, EventStream, EventType, InputEvent, InputEventKind, Key, RelativeAxisType, Synchronization};
use input::{event::{pointer::{ButtonState, PointerScrollEvent}, PointerEvent}, Event, Libinput};

/// Struct containing a virtual mouse's metadata.  
#[derive(Debug, Clone)]
pub struct MouseInfo{
    /// Name of the virtual mouse, either specified in the creation request, or auto generated from the output id
    pub name: String,
    /// evdev event number for the input device
    pub input_id: u32,
    /// evdev event number for the output device
    pub output_id: u32
}

/// Errors from the virtual mouse creation process
#[derive(Debug)]
pub enum MouseCreationError{
    /// The name specified was already in use by the system. Contains the conflicting name
    NameInUse,
    /// The path speicified was unable to be added to the libinput context as a device
    FailedToAddPathAsLibinputDevice,
    /// The path could not be opened by the evdev crate as an evdev device
    FailedToOpenEvdevDevice(std::io::Error),
    /// After opening the device, it could not be turned into an event stream
    FailedToCreateEventStream(std::io::Error),
    /// VirtualDeviceBuilder failed to create a virtual device
    FailedToCreateVirtualDevice(std::io::Error),
    /// Could not parse the sysname of the input device for an event id
    FailedToGetInputID(String),
    /// Could not get the virtual device's syspath
    FailedToGetOutputSyspath(std::io::Error),
    /// Could not get the output event id from the output's syspath
    FailedToGetOutputIDFromSyspath(PathBuf),
    /// The program had a future awaiting a mouse that is not queued, created, or returned an error
    AsyncProgramError
}
impl ToString for MouseCreationError{
    fn to_string(&self) -> String {
        match self {
            MouseCreationError::NameInUse => "Name is already used".to_string(),
            MouseCreationError::FailedToAddPathAsLibinputDevice => "Path was unable to be added to the libinput context as a device".to_string(),
            MouseCreationError::FailedToOpenEvdevDevice(err) => format!("Evdev device failed to open: {}", err),
            MouseCreationError::FailedToCreateEventStream(err) => format!("Event Stream could not be created: {}", err),
            MouseCreationError::FailedToCreateVirtualDevice(err) => format!("Virtual device could not be created: {}", err),
            MouseCreationError::FailedToGetInputID(err) => format!("Could not get input id: {}", err),
            MouseCreationError::FailedToGetOutputSyspath(err) => format!("Could not get output syspath: {}", err),
            MouseCreationError::FailedToGetOutputIDFromSyspath(err) => format!("Could not get output id from syspath: {:?}", err),
            MouseCreationError::AsyncProgramError => "Future created for mouse that is not queued, created, or failed".to_string(),
        }
    }
}
/// Error types returned by the Mouse Driver's poll update function
#[derive(Debug)]
pub enum MouseDriverUpdateError{
    /// Evdev event stream was unable to poll the events
    TestSourceReadError(std::io::Error),
    /// The libinput context was unable to dispatch for events
    DataSourceDispatchError(std::io::Error),
    /// The virtual device was unable to emit events
    EmitEventsError(std::io::Error)
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
    pub fn new(name: String, input_path: String) -> Result<Self, MouseCreationError>{
        // Get Libinput setup
        let mut data_source = Libinput::new_from_path(super::Interface);
        let device = data_source.path_add_device(&input_path).ok_or(MouseCreationError::FailedToAddPathAsLibinputDevice)?;
        // Get the input event id
        fn sysname_to_id(sysname: String) -> Result<u32, MouseCreationError> {
            sysname.clone().strip_prefix("event")
                .ok_or_else(|| MouseCreationError::FailedToGetInputID(sysname.clone()))
                .and_then(|val| val.parse::<u32>().or_else(|_| Err(MouseCreationError::FailedToGetInputID(sysname.clone()))))
        }
        let input_id = sysname_to_id(device.sysname().to_string())?;
        // Get evdev test source setup
        let test_source = Device::open(input_path.clone())
            .map_err(|err| {MouseCreationError::FailedToOpenEvdevDevice(err)})?
            .into_event_stream().map_err(|err| MouseCreationError::FailedToCreateEventStream(err))?;
        // Create the virtual mouse device
        fn create_virtual_device(name: String) -> std::io::Result<VirtualDevice> {
            VirtualDeviceBuilder::new()?.name(("TPtoMouse ".to_owned() + name.as_str()).as_str())
                .with_relative_axes(&AttributeSet::from_iter([
                    RelativeAxisType::REL_X,
                    RelativeAxisType::REL_Y,
                    RelativeAxisType::REL_WHEEL,
                    RelativeAxisType::REL_WHEEL_HI_RES,
                    RelativeAxisType::REL_HWHEEL,
                    RelativeAxisType::REL_HWHEEL_HI_RES
                ]))?
                .with_keys(&AttributeSet::from_iter([
                    Key::BTN_LEFT,
                    Key::BTN_RIGHT,
                    Key::BTN_MIDDLE
                ]))?
                .build()
        }
        let mut output = create_virtual_device(name.clone()).map_err(|err| MouseCreationError::FailedToCreateVirtualDevice(err))?;
        // Get the output event id
        fn get_output_id(device: &mut VirtualDevice) -> Result<u32, MouseCreationError>{
            let syspath = device.get_syspath().map_err(|err| MouseCreationError::FailedToGetOutputSyspath(err))?;
            match syspath.clone().file_name().and_then(|val| val.to_os_string().into_string().ok()) {
                Some(name) => {
                    return name.strip_prefix("event")
                        .ok_or(MouseCreationError::FailedToGetOutputIDFromSyspath(syspath.clone()))
                        .and_then(|val| val.parse::<u32>().map_err(|_| MouseCreationError::FailedToGetOutputIDFromSyspath(syspath)));
                },
                _ => {return Err(MouseCreationError::FailedToGetOutputIDFromSyspath(syspath))}
            }
        }
        let output_id = get_output_id(&mut output)?;

        let metadata = MouseInfo{name, input_id, output_id};

        Ok(Self{
            metadata,
            test_source,
            data_source,
            output,
            movement: MouseMovement::default()
        })
    }

    /// Asynchronously waits for the next syn report to happen for the trackpad input device
    pub async fn await_sync_event(&mut self) -> Result<(), MouseDriverUpdateError>{
        loop{
            match self.test_source.next_event().await {
                Err(err) => {return Err(MouseDriverUpdateError::TestSourceReadError(err));},
                Ok(event) => {if event.kind() == InputEventKind::Synchronization(Synchronization::SYN_REPORT) {return Ok(());}}
            }
        }
    }
    /// Poll function to update the mouse endlessly until it errors out
    pub async fn update_loop(&mut self) -> MouseDriverUpdateError {
        loop{
            if let Err(err) = self.await_sync_event().await {return err;};

            if let Err(err) = self.data_source.dispatch() {return MouseDriverUpdateError::DataSourceDispatchError(err);}

            let events: Vec<Event> = self.data_source.by_ref().collect();
            for event in events{
                self.movement.process_event(event);
            }
            // emit mouse events
            let events = self.movement.get_output_events();
            if events.len() > 0 {
                if let Err(err) = self.output.emit(&events) {return MouseDriverUpdateError::EmitEventsError(err);}
            }
        }
    }

    /// Locks the trackpad input device, preventing it from interacting with the computer
    pub fn lock(&self) {
        std::process::Command::new("xinput").args(["--disable".to_string(), self.metadata.input_id.to_string()]).spawn().unwrap().wait().unwrap();
    }
    /// Unlocks the trackpad input device
    pub fn unlock(&self) {
        std::process::Command::new("xinput").args(["--enable".to_string(), self.metadata.input_id.to_string()]).spawn().unwrap().wait().unwrap();
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
