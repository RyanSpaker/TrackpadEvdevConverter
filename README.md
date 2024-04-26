# TrackpadEvdevConverter
Tool to convert a trackpad evdev device into a normal mouse, specifically for use with qemu evdev passthrough

# usage:
The compiled program is a command line tool
trackpad-evdev-converter /path/to/trackpad/event
The program will create the virtual mouse until the environment variable "STOP_VIRTUAL_TRACKPAD" is set to 1
Upon starting, the environment variable "VIRTUAL_MOUSE_EVENT_ID" will contain the path to the /dev/input/event* corresponding to the fake trackpad