#!/usr/bin/env python3
"""Linux UI checks for an app running under X11 / Xwayland (GDK_BACKEND=x11).

GNOME on Wayland does not let other programs capture or type into native
Wayland windows, so the app is started with GDK_BACKEND=x11 and driven here
through Xwayland: GDK captures the window, XTEST sends keys and clicks.

  x11.py find  [title]                  window id of the app (default "TermWorker")
  x11.py shot  out.png [title]          capture the app window
  x11.py type  "text" [title]           type text (ASCII)
  x11.py key   ctrl+shift+t [title]     press a key combination (X keysym names)
  x11.py click X Y [title]              left click at window coordinates
  x11.py scroll X Y N [title]           mouse wheel at X Y: N notches down (negative: up)
  x11.py size  [title]                  window position and size

[title] may also be a window id such as 0x2017f7 (see `xwininfo -root -tree`).
"""
import ctypes
import ctypes.util
import os
import sys
import time

os.environ["GDK_BACKEND"] = "x11"  # this script talks to Xwayland, even inside a Wayland session

import gi

gi.require_version("Gdk", "3.0")
gi.require_version("GdkX11", "3.0")
from gi.repository import Gdk, GdkX11  # noqa: E402

x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
xtst = ctypes.CDLL(ctypes.util.find_library("Xtst"))
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XRaiseWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]

dpy = x11.XOpenDisplay(None)
if not dpy:
    sys.exit("cannot open X display (is DISPLAY set?)")


def find_window(title="TermWorker"):
    """Top-level window whose name is exactly `title` (falls back to a prefix match).
    A title like "0x2017f7" is taken as a window id."""
    if title.startswith("0x"):
        return int(title, 16)
    import subprocess

    best = None
    # GDK lists only its own windows; ask the X server for the tree.

    out = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        line = line.strip()
        if not line.startswith("0x") or '"' not in line:
            continue
        wid, rest = line.split(" ", 1)
        name = rest.split('"')[1]
        size = rest.split(")")[-1].strip().split()[0] if ")" in rest else ""
        if name == title:
            w, h = (int(v) for v in size.split("+")[0].split("x")) if size else (0, 0)
            if w > 100 and h > 100:
                return int(wid, 16)
        elif best is None and name.startswith(title):
            best = int(wid, 16)
    if best is None:
        sys.exit(f'no window titled "{title}"')
    return best


def foreign(wid):
    display = GdkX11.X11Display.get_default()
    return GdkX11.X11Window.foreign_new_for_display(display, wid)


def geometry(wid):
    w = foreign(wid)
    _, x, y = w.get_origin()
    return x, y, w.get_width(), w.get_height()


def focus(wid):
    x11.XRaiseWindow(dpy, wid)
    x11.XSetInputFocus(dpy, wid, 1, 0)  # RevertToPointerRoot, CurrentTime
    x11.XFlush(dpy)
    time.sleep(0.15)


def shot(path, wid):
    w = foreign(wid)
    pb = Gdk.pixbuf_get_from_window(w, 0, 0, w.get_width(), w.get_height())
    if pb is None:
        sys.exit("capture failed")
    pb.savev(path, "png", [], [])
    print(f"{path} {w.get_width()}x{w.get_height()}")


def keysym(name):
    aliases = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L", "super": "Super_L",
               "enter": "Return", "esc": "Escape", "tab": "Tab", "space": "space",
               "backspace": "BackSpace", "up": "Up", "down": "Down", "left": "Left", "right": "Right"}
    name = aliases.get(name.lower(), name)
    ks = x11.XStringToKeysym(name.encode())
    if ks == 0:
        sys.exit(f"unknown key {name}")
    return ks


def press(combo):
    codes = [x11.XKeysymToKeycode(dpy, keysym(k)) for k in combo.split("+")]
    for c in codes:
        xtst.XTestFakeKeyEvent(dpy, c, True, 0)
    for c in reversed(codes):
        xtst.XTestFakeKeyEvent(dpy, c, False, 0)
    x11.XFlush(dpy)
    time.sleep(0.03)


SHIFTED = {'!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0',
           '_': 'minus', '+': 'equal', '{': 'bracketleft', '}': 'bracketright', '|': 'backslash',
           ':': 'semicolon', '"': 'apostrophe', '<': 'comma', '>': 'period', '?': 'slash', '~': 'grave'}
PLAIN = {' ': 'space', '-': 'minus', '=': 'equal', '[': 'bracketleft', ']': 'bracketright', '\\': 'backslash',
         ';': 'semicolon', "'": 'apostrophe', ',': 'comma', '.': 'period', '/': 'slash', '`': 'grave',
         '\n': 'Return', '\t': 'Tab'}


def type_text(text):
    for ch in text:
        if ch.isalpha() and ch.isascii() and ch.isupper():
            press("shift+" + ch.lower())
        elif ch in SHIFTED:
            press("shift+" + SHIFTED[ch])
        elif ch in PLAIN:
            press(PLAIN[ch])
        else:
            press(ch)


def click(wid, x, y):
    ox, oy, _, _ = geometry(wid)
    xtst.XTestFakeMotionEvent(dpy, -1, ox + x, oy + y, 0)
    x11.XFlush(dpy)
    time.sleep(0.05)
    xtst.XTestFakeButtonEvent(dpy, 1, True, 0)
    xtst.XTestFakeButtonEvent(dpy, 1, False, 0)
    x11.XFlush(dpy)
    time.sleep(0.1)


def scroll(wid, x, y, notches):
    ox, oy, _, _ = geometry(wid)
    xtst.XTestFakeMotionEvent(dpy, -1, ox + x, oy + y, 0)
    x11.XFlush(dpy)
    time.sleep(0.05)
    button = 5 if notches > 0 else 4
    for _ in range(abs(notches)):
        xtst.XTestFakeButtonEvent(dpy, button, True, 0)
        xtst.XTestFakeButtonEvent(dpy, button, False, 0)
        x11.XFlush(dpy)
        time.sleep(0.03)


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd, args = argv[1], argv[2:]
    if cmd == "find":
        print(hex(find_window(*args[:1])))
    elif cmd == "size":
        print(geometry(find_window(*args[:1])))
    elif cmd == "shot":
        shot(args[0], find_window(*args[1:2]))
    elif cmd == "type":
        wid = find_window(*args[1:2])
        focus(wid)
        type_text(args[0])
    elif cmd == "key":
        wid = find_window(*args[1:2])
        focus(wid)
        for combo in args[0].split():
            press(combo)
    elif cmd == "click":
        wid = find_window(*args[2:3])
        focus(wid)
        click(wid, int(args[0]), int(args[1]))
    elif cmd == "scroll":
        wid = find_window(*args[3:4])
        scroll(wid, int(args[0]), int(args[1]), int(args[2]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
