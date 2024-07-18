# TrackpadEvdevConverter
Tool to convert a trackpad evdev device into a normal mouse, specifically made for use with qemu evdev passthrough on a laptop

### How it works
The program's main server is started with the --server flag, and needs either root user or the input group. The server is designed to be set up as a systemd service.

The secondary server is started with --session, and is needed to disable/enable mice with the xinput tool. It must be run in an X session, and is designed to be run as a user systemd service, under the graphical-session.target.

The cli tool is used to interact with the main server, and corresponds to the rest of the flags. use --help to find out more.

The program takes in a file location for an evdev event file corresponding to a trackpad. (usually something like /dev/input/event6, or /dev/input/by-path/...). 
It then creates a libinput context using this device to automatically generate relative mouse events. It then converts these mouse events into the corresponding evdev events, and creates a new evdev device to output these events to. 

It also uses the xinput command line tool to find out which libinput device is recieving input from the evdev file specified, and disables the device, to prevent the mouse from being duplicated (the original, plus the new virtual mouse).