# TrackpadEvdevConverter
Tool to convert a trackpad evdev device into a normal mouse, specifically for use with qemu evdev passthrough

# usage:
The compiled program is a command line tool
trackpad-evdev-converter /path/to/trackpad/event
The program will create a file /tmp/virtual-mouse containing a path to the /dev/input/event* representing the virtual mouse
the trackpad is automatically grabbed, and the application will close, releasing it when the virtual-mouse file is deleted