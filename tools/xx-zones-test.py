#!/usr/bin/env python3
"""Manual test client for xx-zones-v1, plain Python, speaks the wire protocol itself.

Opens two toplevels, a red and a blue 300x200 rectangle, puts both in one zone, asks for
(100, 100) and (400, 300), and prints what jff says: the zone size, the handle, `item_entered`,
`frame_extents` and `position`. Then it keeps printing, so Alt+drag one rectangle (the other
should follow), Alt+drag after maximizing one (the other stays), and watch the `position` events.
Run it inside the jff session (WAYLAND_DISPLAY set).
"""

import mmap
import os
import socket
import struct
import sys


def pad(data):
    return data + b"\0" * (-len(data) % 4)


def string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + pad(raw)


class Wire:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.buf = b""

    def send(self, obj, opcode, payload=b"", fds=()):
        size = 8 + len(payload)
        data = struct.pack("<II", obj, (size << 16) | opcode) + payload
        if fds:
            socket.send_fds(self.sock, [data], list(fds))
        else:
            self.sock.sendall(data)

    def event(self):
        while len(self.buf) < 8 or len(self.buf) < (
            struct.unpack_from("<I", self.buf, 4)[0] >> 16
        ):
            chunk = self.sock.recv(4096)
            if not chunk:
                sys.exit("connection closed")
            self.buf += chunk
        obj, word = struct.unpack_from("<II", self.buf)
        size, opcode = word >> 16, word & 0xFFFF
        body, self.buf = self.buf[8:size], self.buf[size:]
        return obj, opcode, body


def read_string(body, at):
    (length,) = struct.unpack_from("<I", body, at)
    text = body[at + 4 : at + 4 + length - 1].decode()
    return text, at + 4 + ((length + 3) & ~3)


DISPLAY, REGISTRY, SYNC, COMPOSITOR, WM_BASE, MANAGER, ZONE = 1, 2, 3, 4, 5, 6, 7
SURFACE = [8, 12]
XDG_SURFACE = [9, 13]
TOPLEVEL = [10, 14]
ITEM = [11, 15]
SHM = 16
POOL = [17, 19]
BUFFER = [18, 20]
WIDTH, HEIGHT = 300, 200
COLOURS = [0xFFCC3333, 0xFF3366CC]  # ARGB8888
WANT = [(100, 100), (400, 300)]

NAMES = {
    ZONE: ["size", "handle", "done", "item_blocked", "item_entered", "item_left"],
    ITEM[0]: ["frame_extents", "position", "position_failed", "closed"],
    ITEM[1]: ["frame_extents", "position", "position_failed", "closed"],
}


def main():
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
    wire = Wire(os.path.join(runtime, os.environ.get("WAYLAND_DISPLAY", "wayland-0")))
    wire.send(DISPLAY, 1, struct.pack("<I", REGISTRY))
    wire.send(DISPLAY, 0, struct.pack("<I", SYNC))
    names = {}
    while True:
        obj, opcode, body = wire.event()
        if obj == REGISTRY and opcode == 0:
            (num,) = struct.unpack_from("<I", body)
            interface, at = read_string(body, 4)
            (version,) = struct.unpack_from("<I", body, at)
            names[interface] = (num, version)
        elif obj == DISPLAY and opcode == 0:
            sys.exit("protocol error: %r" % (read_string(body, 8)[0],))
        elif obj == SYNC:
            break
    for need in ("wl_compositor", "xdg_wm_base", "xx_zone_manager_v1", "wl_shm"):
        if need not in names:
            sys.exit("%s is not advertised" % need)

    def bind(interface, version, new_id):
        num = names[interface][0]
        wire.send(
            REGISTRY,
            0,
            struct.pack("<I", num)
            + string(interface)
            + struct.pack("<II", version, new_id),
        )

    bind("wl_compositor", 1, COMPOSITOR)
    bind("xdg_wm_base", 1, WM_BASE)
    bind("xx_zone_manager_v1", 1, MANAGER)

    wire.send(MANAGER, 2, struct.pack("<II", ZONE, 0))  # get_zone(id, no output)
    for i in range(2):
        wire.send(COMPOSITOR, 0, struct.pack("<I", SURFACE[i]))  # create_surface
        wire.send(
            WM_BASE, 2, struct.pack("<II", XDG_SURFACE[i], SURFACE[i])
        )  # get_xdg_surface
        wire.send(XDG_SURFACE[i], 1, struct.pack("<I", TOPLEVEL[i]))  # get_toplevel
        wire.send(TOPLEVEL[i], 3, string("org.example.zone-test"))  # set_app_id
        wire.send(MANAGER, 1, struct.pack("<II", ITEM[i], TOPLEVEL[i]))  # get_zone_item
        wire.send(ZONE, 1, struct.pack("<I", ITEM[i]))  # add_item
        wire.send(ITEM[i], 1, struct.pack("<ii", *WANT[i]))  # set_position
        wire.send(SURFACE[i], 6)  # commit

    bind("wl_shm", 1, SHM)
    keep = []
    for i in range(2):
        size = WIDTH * HEIGHT * 4
        fd = os.memfd_create("zone-test")
        os.ftruncate(fd, size)
        memory = mmap.mmap(fd, size)
        memory.write(struct.pack("<I", COLOURS[i]) * (WIDTH * HEIGHT))
        keep.append((fd, memory))
        wire.send(SHM, 0, struct.pack("<II", POOL[i], size), fds=[fd])  # create_pool
        wire.send(
            POOL[i],
            0,
            struct.pack("<IiiiiI", BUFFER[i], 0, WIDTH, HEIGHT, WIDTH * 4, 0),
        )  # create_buffer, argb8888
    mapped = [False, False]

    print(
        "two toplevels in one zone, wanting %r; Alt+drag one, Ctrl+C to quit" % (WANT,)
    )
    while True:
        obj, opcode, body = wire.event()
        if obj == DISPLAY and opcode == 0:
            sys.exit("protocol error: %r" % (read_string(body, 8)[0],))
        if obj in XDG_SURFACE and opcode == 0:  # configure: acknowledge
            wire.send(obj, 4, body[:4])
            i = XDG_SURFACE.index(obj)
            if not mapped[i]:
                mapped[i] = True
                wire.send(SURFACE[i], 1, struct.pack("<Iii", BUFFER[i], 0, 0))  # attach
                wire.send(
                    SURFACE[i], 2, struct.pack("<iiii", 0, 0, WIDTH, HEIGHT)
                )  # damage
                wire.send(SURFACE[i], 6)  # commit
            continue
        if obj == WM_BASE and opcode == 0:  # ping
            wire.send(WM_BASE, 3, body[:4])
            continue
        names_of = NAMES.get(obj)
        if names_of is None or opcode >= len(names_of):
            continue
        who = "zone" if obj == ZONE else "item %d" % ITEM.index(obj)
        event = names_of[opcode]
        if event == "handle":
            print("%s: handle %r" % (who, read_string(body, 0)[0]))
        elif event in ("size", "position"):
            print("%s: %s %d x %d" % ((who, event) + struct.unpack_from("<ii", body)))
        elif event == "frame_extents":
            print("%s: frame_extents %r" % (who, struct.unpack_from("<iiii", body)))
        else:
            print("%s: %s" % (who, event))


if __name__ == "__main__":
    main()
