# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Drive a local libvirt Windows VM for testing the Tauri build: SPICE
screenshots and QEMU input events, no agent needed inside Windows.

Needs: virsh access to qemu:///system, the VM's SPICE display on
127.0.0.1:5900, and spicy-screenshot plus ImageMagick in the `marktext`
distrobox (dnf install spice-gtk-tools ImageMagick). Screenshots go to
$VM_SHOTS (default: the system temp folder). VM_DOMAIN picks the VM
(default win11). Typing assumes the VM's UK keyboard layout.

Usage:
  vm.py shot NAME [SCALE]       screenshot to NAME.png (SCALE percent, default 50)
  vm.py move X Y                move the pointer to screen pixel X, Y
  vm.py click X Y [right|double]
  vm.py drag X1 Y1 X2 Y2       press at X1, Y1, move to X2, Y2, release
  vm.py key COMBO [COMBO...]    e.g. ctrl+v  meta+r  ret  esc  alt+f4
  vm.py type TEXT               type text on the VM's UK keyboard layout
"""

import json
import os
import subprocess
import sys
import tempfile
import time

DOMAIN = os.environ.get("VM_DOMAIN", "win11")
SCRATCH = os.environ.get("VM_SHOTS", tempfile.gettempdir())
SIZE_FILE = os.path.join(SCRATCH, "vm-size.json")

# UK layout: backslash and | are on the key left of Z ("less"), # and ~ on the
# key left of Enter ("backslash"), " on 2 and @ on the apostrophe key.
SHIFTED = {
    "!": "1", '"': "2", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8",
    "(": "9", ")": "0", "_": "minus", "+": "equal", "{": "bracket_left",
    "}": "bracket_right", "|": "less", ":": "semicolon", "@": "apostrophe",
    "<": "comma", ">": "dot", "?": "slash", "~": "backslash",
}
PLAIN = {
    " ": "spc", "-": "minus", "=": "equal", "[": "bracket_left", "]": "bracket_right",
    "\\": "less", "#": "backslash", ";": "semicolon", "'": "apostrophe", ",": "comma",
    ".": "dot", "/": "slash", "`": "grave_accent", "\n": "ret", "\t": "tab",
}
NAMES = {"ctrl": "ctrl", "shift": "shift", "alt": "alt", "meta": "meta_l", "win": "meta_l",
         "enter": "ret", "escape": "esc"}


def qmp(command: dict) -> str:
    result = subprocess.run(
        ["virsh", "-c", "qemu:///system", "qemu-monitor-command", DOMAIN, json.dumps(command)],
        capture_output=True, text=True, check=True,
    )
    return result.stdout


def send(events: list) -> None:
    qmp({"execute": "input-send-event", "arguments": {"events": events}})


def key_event(qcode: str, down: bool) -> dict:
    return {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}


def screen_size() -> tuple[int, int]:
    try:
        with open(SIZE_FILE) as f:
            width, height = json.load(f)
            return width, height
    except OSError:
        return 1920, 1080


def shot(name: str, scale: int = 50) -> None:
    ppm = os.path.join(SCRATCH, name + ".ppm")
    png = os.path.join(SCRATCH, name + ".png")
    subprocess.run(
        ["distrobox", "enter", "marktext", "--", "bash", "-c",
         f"spicy-screenshot -h 127.0.0.1 -p 5900 -o '{ppm}' >/dev/null 2>&1 && "
         f"magick '{ppm}' -resize {scale}% '{png}'"],
        check=True,
    )
    size = subprocess.run(
        ["distrobox", "enter", "marktext", "--", "identify", "-format", "%w %h", ppm],
        capture_output=True, text=True, check=True,
    ).stdout.split()
    with open(SIZE_FILE, "w") as f:
        json.dump([int(size[0]), int(size[1])], f)
    print(png, "screen", size[0], "x", size[1])


def move(x: int, y: int) -> None:
    width, height = screen_size()
    send([
        {"type": "abs", "data": {"axis": "x", "value": round(x * 32767 / (width - 1))}},
        {"type": "abs", "data": {"axis": "y", "value": round(y * 32767 / (height - 1))}},
    ])


def click(x: int, y: int, how: str = "left") -> None:
    move(x, y)
    time.sleep(0.15)
    button = "right" if how == "right" else "left"
    for _ in range(2 if how == "double" else 1):
        send([{"type": "btn", "data": {"down": True, "button": button}}])
        time.sleep(0.05)
        send([{"type": "btn", "data": {"down": False, "button": button}}])
        time.sleep(0.08)


def drag(x1: int, y1: int, x2: int, y2: int) -> None:
    move(x1, y1)
    time.sleep(0.15)
    send([{"type": "btn", "data": {"down": True, "button": "left"}}])
    for step in range(1, 11):
        time.sleep(0.04)
        move(x1 + (x2 - x1) * step // 10, y1 + (y2 - y1) * step // 10)
    time.sleep(0.15)
    send([{"type": "btn", "data": {"down": False, "button": "left"}}])


def key(combo: str) -> None:
    codes = [NAMES.get(part, part) for part in combo.lower().split("+")]
    send([key_event(code, True) for code in codes])
    time.sleep(0.05)
    send([key_event(code, False) for code in reversed(codes)])
    time.sleep(0.05)


def type_text(text: str) -> None:
    for ch in text:
        if ch.isalpha() and ch.isascii():
            codes = ["shift", ch.lower()] if ch.isupper() else [ch]
        elif ch.isdigit():
            codes = [ch]
        elif ch in SHIFTED:
            codes = ["shift", SHIFTED[ch]]
        elif ch in PLAIN:
            codes = [PLAIN[ch]]
        else:
            raise SystemExit(f"cannot type {ch!r}")
        send([key_event(code, True) for code in codes])
        send([key_event(code, False) for code in reversed(codes)])
        time.sleep(0.02)


def main() -> None:
    command, *args = sys.argv[1:]
    if command == "shot":
        shot(args[0], int(args[1]) if len(args) > 1 else 50)
    elif command == "move":
        move(int(args[0]), int(args[1]))
    elif command == "click":
        click(int(args[0]), int(args[1]), args[2] if len(args) > 2 else "left")
    elif command == "drag":
        drag(*(int(arg) for arg in args[:4]))
    elif command == "key":
        for combo in args:
            key(combo)
    elif command == "type":
        type_text(" ".join(args))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
