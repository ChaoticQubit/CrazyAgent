import os
import socket
import struct
import sys
import time

KEYS = {
    **{c: code for c, code in zip("qwertyuiop", range(16, 26))},
    **{c: code for c, code in zip("asdfghjkl", range(30, 39))},
    **{c: code for c, code in zip("zxcvbnm", range(44, 51))},
    "'": 40,
    "Return": 28,
    "Up": 103,
    "Down": 108,
    "Left": 105,
    "Right": 106,
    "Control_L": 29,
    "Shift_L": 42,
}
MODS = {"Shift_L": 1, "Control_L": 4}
BUTTONS = {"left": 0x110, "right": 0x111, "middle": 0x112}
DELAY = 0.08


def now():
    """The protocol timestamp: monotonic milliseconds, wrapped to 32 bits. Output: it."""
    return int(time.monotonic() * 1000) & 0xFFFFFFFF


def string(text):
    """Encodes a Wayland string argument. Input: the text. Output: its bytes."""
    data = text.encode() + b"\0"
    return struct.pack("<I", len(data)) + data + b"\0" * (-len(data) % 4)


class Wire:
    """A bare Wayland client on one socket: requests out, the few events it needs in.
    Input: the socket path. Output: the connection, whose globals map each interface to
    its registry name and version. A protocol error from the compositor raises."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.next_id = 2
        self.pending = b""
        self.callbacks = set()
        self.done = set()
        self.registry = self.new_id()
        self.globals = {}
        self.send(1, 1, struct.pack("<I", self.registry))
        self.roundtrip()

    def new_id(self):
        """Allocates the next client object id. Output: the id."""
        number = self.next_id
        self.next_id += 1
        return number

    def send(self, obj, opcode, payload=b"", fds=()):
        """Sends one request. Inputs: the object, the opcode, the packed arguments and any
        file descriptors. Output: none."""
        message = struct.pack("<II", obj, ((8 + len(payload)) << 16) | opcode) + payload
        rights = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, struct.pack(f"<{len(fds)}i", *fds))]
        sent = self.sock.sendmsg([message], rights if fds else [])
        assert sent == len(message), "a short write on the Wayland socket"

    def roundtrip(self):
        """Waits until the compositor has handled every request sent so far. Output: none."""
        callback = self.new_id()
        self.callbacks.add(callback)
        self.send(1, 0, struct.pack("<I", callback))
        while callback not in self.done:
            self.read()
        self.callbacks.discard(callback)
        self.done.discard(callback)

    def read(self):
        """Reads and handles the events that have come. Output: none."""
        chunk = self.sock.recv(65536)
        if not chunk:
            raise SystemExit("the compositor closed the connection")
        self.pending += chunk
        while len(self.pending) >= 8:
            obj, word = struct.unpack_from("<II", self.pending)
            size, opcode = word >> 16, word & 0xFFFF
            assert size >= 8, "a malformed Wayland event"
            if len(self.pending) < size:
                return
            body, self.pending = self.pending[8:size], self.pending[size:]
            self.event(obj, opcode, body)

    def event(self, obj, opcode, body):
        """Handles one event: a display error, a registry global or a callback's done.
        Inputs: the object, the opcode and the argument bytes. Output: none."""
        if obj == 1 and opcode == 0:
            failed, code, length = struct.unpack_from("<III", body)
            message = body[12 : 12 + length - 1].decode(errors="replace")
            raise SystemExit(f"protocol error on object {failed}, code {code}: {message}")
        if obj == self.registry and opcode == 0:
            name, length = struct.unpack_from("<II", body)
            interface = body[8 : 8 + length - 1].decode()
            (version,) = struct.unpack_from("<I", body, 8 + length + (-length % 4))
            self.globals[interface] = (name, version)
        elif obj in self.callbacks and opcode == 0:
            self.done.add(obj)

    def bind(self, interface, version):
        """Binds a global at a version it advertises. Inputs: the interface and the
        highest version wanted. Output: the new object id."""
        assert interface in self.globals, f"the compositor has no {interface}"
        name, advertised = self.globals[interface]
        obj = self.new_id()
        payload = struct.pack("<I", name) + string(interface)
        self.send(self.registry, 0, payload + struct.pack("<II", min(version, advertised), obj))
        return obj


class Seat:
    """One virtual keyboard and one virtual pointer that live for the whole cell, so the
    compositor never adds or removes an input device between steps. Inputs: the
    connection, the keymap text and the output size. Output: the seat."""

    def __init__(self, wire, keymap, width, height):
        self.wire = wire
        self.width = width
        self.height = height
        seat = wire.bind("wl_seat", 1)
        keyboards = wire.bind("zwp_virtual_keyboard_manager_v1", 1)
        pointers = wire.bind("zwlr_virtual_pointer_manager_v1", 1)
        self.keyboard = wire.new_id()
        wire.send(keyboards, 0, struct.pack("<II", seat, self.keyboard))
        data = keymap + b"\0"
        fd = os.memfd_create("farm-keymap")
        try:
            os.write(fd, data)
            wire.send(self.keyboard, 0, struct.pack("<II", 1, len(data)), fds=[fd])
        finally:
            os.close(fd)
        self.pointer = wire.new_id()
        wire.send(pointers, 0, struct.pack("<II", seat, self.pointer))
        wire.roundtrip()

    def tap(self, name):
        """Presses and releases one key. Input: its name. Output: none."""
        assert name in KEYS, f"no key {name!r}"
        for state in (1, 0):
            self.wire.send(self.keyboard, 1, struct.pack("<III", now(), KEYS[name], state))
            time.sleep(DELAY)

    def chord(self, names):
        """Holds keys down in order and lets them go in reverse, telling the compositor the
        modifiers held after each change, as a real keyboard's state would. Input: the key
        names, modifiers first. Output: none."""
        held = 0
        for name, state in [(n, 1) for n in names] + [(n, 0) for n in reversed(names)]:
            assert name in KEYS, f"no key {name!r}"
            self.wire.send(self.keyboard, 1, struct.pack("<III", now(), KEYS[name], state))
            held = held | MODS.get(name, 0) if state else held & ~MODS.get(name, 0)
            self.wire.send(self.keyboard, 2, struct.pack("<IIII", held, 0, 0, 0))
            time.sleep(DELAY)

    def frame(self):
        """Ends one pointer event group. Output: none."""
        self.wire.send(self.pointer, 4)

    def click(self, x, y, button):
        """Moves the pointer to a layout point and clicks one button there. Inputs: the
        point and the button name. Output: none."""
        assert 0 <= x < self.width and 0 <= y < self.height, f"({x},{y}) is off the output"
        assert button in BUTTONS, f"no button {button!r}"
        self.wire.send(self.pointer, 1, struct.pack("<IIIII", now(), x, y, self.width, self.height))
        self.frame()
        time.sleep(DELAY)
        for state in (1, 0):
            self.wire.send(self.pointer, 2, struct.pack("<III", now(), BUTTONS[button], state))
            self.frame()
            time.sleep(DELAY)

    def scroll(self, steps):
        """Turns a mouse wheel down by whole steps, up when negative. Input: the steps.
        Output: none."""
        assert steps != 0 and abs(steps) <= 10
        self.wire.send(self.pointer, 5, struct.pack("<I", 0))
        self.wire.send(self.pointer, 7, struct.pack("<IIii", now(), 0, 15 * 256 * steps, steps))
        self.frame()
        time.sleep(DELAY)


def run(seat, words):
    """Runs one command: type TEXT, key NAME..., chord NAME..., click X Y BUTTON or scroll
    STEPS.
    Input: the command's words. Output: none."""
    verb, args = words[0], words[1:]
    if verb == "type":
        assert len(args) == 1
        for char in args[0]:
            seat.tap(char)
    elif verb == "key":
        assert args
        for name in args:
            seat.tap(name)
    elif verb == "chord":
        assert args
        seat.chord(args)
    elif verb == "click":
        assert len(args) == 3
        seat.click(int(args[0]), int(args[1]), args[2])
    elif verb == "scroll":
        assert len(args) == 1
        seat.scroll(int(args[0]))
    else:
        raise SystemExit(f"no command {verb!r}")


def main():
    """Holds the farm's seat devices for one sway cell and runs the commands read from a
    FIFO, writing one done line per command to the acks file after the compositor
    handled it. Inputs: the FIFO, the acks file, the keymap file and WIDTHxHEIGHT.
    Output: exit 0 when the FIFO closes."""
    fifo, acks, keymap_path, size = sys.argv[1:5]
    width, height = (int(n) for n in size.split("x"))
    path = os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"])
    wire = Wire(path)
    with open(keymap_path, "rb") as keymap:
        seat = Seat(wire, keymap.read(), width, height)
    with open(acks, "a", buffering=1) as out:
        out.write("ready\n")
        count = 0
        with open(fifo, encoding="utf-8") as commands:
            for line in commands:
                words = line.split()
                if not words:
                    continue
                run(seat, words)
                wire.roundtrip()
                count += 1
                out.write(f"done {count} {words[0]}\n")


if __name__ == "__main__":
    main()
