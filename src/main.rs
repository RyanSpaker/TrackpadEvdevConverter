use std::{fs::{self, File}, io::Write, path::Path, sync::{atomic::{AtomicBool, Ordering}, Arc}};

use evdev::{uinput::VirtualDeviceBuilder, AbsoluteAxisType, AttributeSet, EventType, InputEvent, InputEventKind, Key, RelativeAxisType};

pub enum MouseState{
    None,
    StartedTracking,
    Tracking,
    StartedScrolling,
    Scrolling
}
fn main() -> std::io::Result<()>{
    let args = std::env::args_os().map(|os_string| os_string.into_string().unwrap()).collect::<Vec<String>>();
    if args.len() <= 1 {
        eprintln!("Error: Tool must be run with path to trackpad evdev event input");
        return Err(std::io::Error::from_raw_os_error(1));
    }
    let mut trackpad = match evdev::Device::open(&args[1]) {
        Ok(dev) => {dev},
        Err(er) => {
            eprintln!("Error: could not open the trackpad at: {}", args[1]);
            return Err(er);
        }
    };
    trackpad.grab().expect("Failed to grab trackpad");
    // Create fake mouse
    let mut fake_mouse = VirtualDeviceBuilder::new()?
        .name("virtual-mouse")
        .with_relative_axes(&AttributeSet::from_iter([
            RelativeAxisType::REL_X,
            RelativeAxisType::REL_Y,
            RelativeAxisType::REL_WHEEL,
            RelativeAxisType::REL_WHEEL_HI_RES
        ]))?
        .with_keys(&AttributeSet::from_iter([
            Key::BTN_LEFT,
            Key::BTN_RIGHT
        ]))?.build()?;
    // Export location  of fake mouse evdev event
    let file_path = Path::new("/tmp/virtual-trackpad");
    if let Some(event) = fake_mouse.get_syspath()?.as_path().read_dir()?.flatten().find(|child| child.file_name().into_string().unwrap().starts_with("event")){
        let mut file = File::create(file_path)?;
        file.write_all(("/dev/input/".to_owned() + event.file_name().to_str().unwrap()).as_bytes())?;
    }else {return Err(std::io::Error::from_raw_os_error(2));}
    let mut state = MouseState::None;
    let mut old_pos: (i32, i32) = (0, 0);
    let mut cur_pos: (i32, i32) = (0, 0);
    let mut relative: (i32, i32) = (0, 0);
    let mut send_button_left = false;
    let mut button_val_left = 0;
    let mut send_button_right = false;
    let mut button_val_right = 0;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).unwrap();
    while running.load(Ordering::SeqCst) {
        //Update fake mouse
        trackpad.fetch_events()?.for_each(|event| {
            match event.kind(){
                InputEventKind::Key(Key::BTN_TOUCH) => {if event.value() == 0 {state = MouseState::None;}},
                InputEventKind::Key(Key::BTN_TOOL_FINGER) => {if event.value() == 1 {state = MouseState::StartedTracking;}},
                InputEventKind::Key(Key::BTN_TOOL_DOUBLETAP) => {if event.value() == 1 {state = MouseState::StartedScrolling;}},
                InputEventKind::AbsAxis(AbsoluteAxisType::ABS_X) => {cur_pos.0 = event.value();},
                InputEventKind::AbsAxis(AbsoluteAxisType::ABS_Y) => {cur_pos.1 = event.value();},
                InputEventKind::Key(Key::BTN_LEFT) => {send_button_left = true; button_val_left = event.value();}
                InputEventKind::Key(Key::BTN_RIGHT) => {send_button_right = true; button_val_right = event.value();}
                _ => {}
            };
        });
        relative.0 = cur_pos.0 - old_pos.0; relative.1 = cur_pos.1 - old_pos.1;
        match state{
            MouseState::StartedTracking => {state = MouseState::Tracking;},
            MouseState::StartedScrolling => {state = MouseState::Scrolling;},
            MouseState::Tracking => {fake_mouse.emit(&[
                InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, relative.0),
                InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, relative.1)
            ])?;},
            MouseState::Scrolling => {fake_mouse.emit(&[
                InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, relative.1),
                InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL_HI_RES.0, relative.1*100),
            ])?;},
            MouseState::None => {}
        }
        if send_button_left {
            fake_mouse.emit(&[
                InputEvent::new(EventType::KEY, Key::BTN_LEFT.code(), button_val_left)
            ])?;
        }
        if send_button_right {
            fake_mouse.emit(&[
                InputEvent::new(EventType::KEY, Key::BTN_RIGHT.code(), button_val_right)
            ])?;
        }
        old_pos = cur_pos;
        send_button_left = false; send_button_right = false;
        // exit program if STOP_TRACKPAD is set
        if !file_path.exists() {break;}
    }
    trackpad.ungrab().unwrap();
    fs::remove_file("/tmp/virtual-trackpad").ok();
    return Ok(());
}
