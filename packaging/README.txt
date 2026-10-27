PAUSE AT TOD
============

An X-Plane 11.50+ / 12 plugin that pauses the sim when a condition you have
armed comes true: top of descent, near a waypoint, at an altitude, or after
a set time. Step away during cruise and come back to a paused aircraft.

Works on Windows, macOS (Intel and Apple Silicon) and Linux.


INSTALLATION
------------

1. Quit X-Plane.
2. Copy the whole "PauseAtToD" folder from this zip into
   <X-Plane folder>/Resources/plugins/
   You should end up with:
       Resources/plugins/PauseAtToD/64/win.xpl
       Resources/plugins/PauseAtToD/64/mac.xpl
       Resources/plugins/PauseAtToD/64/lin.xpl
3. Start X-Plane. "Pause at ToD" now appears in the Plugins menu.

Updating: quit X-Plane and replace the PauseAtToD folder. Your settings
are kept.

macOS: if X-Plane refuses to load the plugin because it was downloaded,
run this in Terminal (adjust the path to your X-Plane folder):
    xattr -dr com.apple.quarantine "/path/to/X-Plane 12/Resources/plugins/PauseAtToD"


BASIC USAGE
-----------

1. Open Plugins > Pause at ToD > Settings...
2. Switch on one or more conditions (the toggle on each card).
3. Press Arm.

The first enabled condition that comes true pauses the sim. The plugin
then disarms itself and the window pops up showing why it paused. Unpause
as usual (P key) and arm again if you want another stop.

While armed, each active condition shows a live status line and a progress
bar. Settings are locked; press Disarm to change them.

Conditions:

  Top of descent  Pauses a chosen distance (default 5 nm) before the top
                  of descent. It uses the X-Plane FMS T/D when available,
                  otherwise the 3:1 rule toward the end-of-descent point,
                  and as a last resort the start of a sustained descent.
                  "Reach [arrival] at [auto] ft MSL" is the end-of-descent
                  point: by default the last airport in X-Plane's FMS
                  flight plan, crossed at field elevation + 1,500 ft.
                  Aircraft with their own FMS (Zibo, ToLiss, many
                  business jets...) are not visible to plugins: type the
                  arrival airport ICAO in that box instead.

  Near waypoint   Pauses within N nm of an airport, VOR, NDB or fix.

  Altitude        Pauses when you cross an altitude while climbing,
                  descending or either way, in MSL or AGL.

  Timer           Pauses after N minutes of sim time. Time spent paused
                  does not count.

Tips:

  - Hover over the "Reach ... at ... ft MSL" line for an explanation.
  - Plugins > Pause at ToD > Armed toggles arming from the menu.
  - Bind keys or joystick buttons in Settings > Keyboard / Joystick to:
        pauseattod/toggle_window
        pauseattod/toggle_arm
  - Loading a new flight or aircraft disarms the plugin.
  - Settings are saved in X-Plane/Output/preferences/pauseattod.prf


TROUBLESHOOTING
---------------

Look for lines starting with "PauseAtToD:" in X-Plane/Log.txt.


HELP AND FEATURE REQUESTS
-------------------------

https://flyvoyager.net/discord

Source code and full documentation:
https://github.com/emergencybutter/pauseattod


LICENSE
-------

MIT. See LICENSE.txt.
