#!/usr/bin/env python3
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
# A GTK drag source for testing drops on Xvfb: dragging the button drops the
# files named on the command line, as a file manager would. Uses the system
# python3-gobject (installed in the marktext distrobox), so run it with the
# container's python3, e.g.
#   DISPLAY=:99 GDK_BACKEND=x11 python3 drag-source.py 1300 100 notes.md
# then drag from its button with xdotool (mousedown, mousemove, mouseup).
# Usage: drag-source.py X Y FILE...
import sys
from pathlib import Path

import gi

gi.require_version("Gdk", "3.0")
gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gtk  # noqa: E402

x, y = int(sys.argv[1]), int(sys.argv[2])
uris = [Path(p).resolve().as_uri() for p in sys.argv[3:]]

win = Gtk.Window(title="dragsource")
win.move(x, y)
win.set_default_size(200, 100)
button = Gtk.Button(label="drag me")
button.drag_source_set(Gdk.ModifierType.BUTTON1_MASK, [], Gdk.DragAction.COPY)
button.drag_source_add_uri_targets()
button.connect("drag-data-get", lambda _w, _c, data, _i, _t: data.set_uris(uris))
win.add(button)
win.connect("destroy", Gtk.main_quit)
win.show_all()
Gtk.main()
