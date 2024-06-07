# TrackpadEvdevConverter
Tool to convert a trackpad evdev device into a normal mouse, specifically for use with qemu evdev passthrough

### Requirements
The tool uses evdev, libinput, and the xinput command line tool. if these things are not available the program wont work.

### How it works
The program's main server is started with the --server flag, and needs either root user or the input group.
The secondary server is started with --session-server, and is needed to disable/enable mice with the xinput tool, and requires being run in an X session.
The client is used to interact with the main server, and corresponds to the rest of the flags.

The program takes in a file location for an evdev event file corresponding to a trackpad.
It then creates a libinput context using this device to automatically generate relative mouse events. It then converts these mouse events into the corresponding evdev events, and creates a new evdev device to output these events to. 

It also uses the xinput command line tool to find out which libinput device is recieving input from the evdev file specified, and disables the device, to prevent the mouse from being duplicated.

### Usage
The service first needs to be started using the --server flag. It requires access to the system bus, so dbus configuration is probably needed. I added a service conf file structure to the flake output, so that you can just add the package to services.dbus.packages to have it setup the correct permission. 

Next, add devices using --new or -n, specifying a name and file path.

Stop devices using --stop and then the mouse name.

List devices with --list

Stop all devices with --shutdown

Get the server's pid with --server-pid

I recommend creating systemd services to manage the session and main server programs.
The session program should be started anytime the session is running, and the server can be started whenever it is needed.