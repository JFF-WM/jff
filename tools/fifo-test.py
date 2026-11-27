#!/usr/bin/env python3
"""Manual test client for wp_fifo_v1. Plain Python 3, speaks the wire protocol itself.

Opens a window of moving vertical stripes and commits a new frame whenever a buffer is free,
with a fifo barrier on every commit (wait_barrier, then set_barrier): the compositor holds each
commit back until the previous one has been on screen, so the client ends up paced to the
display. Each frame moves the stripes 8 px, so at the display's refresh rate they glide
smoothly; stutter means the pacing is off. Once a second it prints how many commits it made
and how many buffers the compositor gave back. With the barriers both are about the display's
refresh rate; with --no-fifo the commit rate is far higher.

Run it inside the jff session (WAYLAND_DISPLAY set):

    python3 tools/fifo-test.py                  # barriers on a window
    python3 tools/fifo-test.py --fullscreen     # same, fullscreen
    python3 tools/fifo-test.py --no-fifo        # no barriers, for comparison

While it runs, in another terminal:

    jff msg stats

Expect, with barriers:

    client_asked.sync_protocols.fifo   true
    client commits per second          about display_hz

and with --no-fifo `fifo` stays false and the commit rate is much higher. If the commit rate is
paced but `fifo` stays false, the barriers are being cleared without jff seeing them; if
wp_fifo_manager_v1 is not advertised, the compositor doesn't offer wp_fifo_v1 at all.

The window closes itself after --seconds (default 20; 0 = never).
"""

import argparse
import array
import mmap
import os
import select
import socket
import struct
import sys
import time

DISPLAY, REGISTRY = 1, 2
WL_SHM_XRGB8888 = 1
PERIOD = 128
STEP = 8
STRIPE = 24
BUFFERS = PERIOD // STEP
DARK = b"\x50\x50\x50\xff"
LIGHT = b"\xe0\xe0\xe0\xff"


def u32(*values):
    return b"".join(struct.pack("<I", v & 0xFFFFFFFF) for v in values)


def i32(*values):
    return b"".join(struct.pack("<i", v) for v in values)


def pad(data):
    return data + b"\0" * (-len(data) % 4)


def string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + pad(raw)


def read_string(body, at):
    (length,) = struct.unpack_from("<I", body, at)
    text = body[at + 4 : at + 4 + length - 1].decode()
    return text, at + 4 + ((length + 3) & ~3)


def die(message):
    sys.exit("fifo-test: " + message)


class Wire:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.sock.connect(path)
        except OSError as err:
            die("can't connect to %s: %s" % (path, err))
        self.rx = b""

    def send(self, obj, opcode, payload=b"", fds=()):
        msg = struct.pack("<II", obj, ((8 + len(payload)) << 16) | opcode) + payload
        if fds:
            anc = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds))]
            self.sock.sendmsg([msg], anc)
        else:
            self.sock.sendall(msg)

    def fileno(self):
        return self.sock.fileno()

    def read(self):
        chunk = self.sock.recv(65536)
        if not chunk:
            die("compositor closed the connection")
        self.rx += chunk

    def events(self):
        while len(self.rx) >= 8:
            obj, word = struct.unpack_from("<II", self.rx)
            size, opcode = word >> 16, word & 0xFFFF
            if len(self.rx) < size:
                return
            body, self.rx = self.rx[8:size], self.rx[size:]
            yield obj, opcode, body


class Client:
    def __init__(self, args):
        self.args = args
        runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
        display = os.environ.get("WAYLAND_DISPLAY", "wayland-0")
        path = display if os.path.isabs(display) else os.path.join(runtime, display)
        self.wire = Wire(path)
        self.next_id = 3
        self.globals = {}
        self.callbacks = set()
        self.done = set()
        self.wm_base = None
        self.xdg_surface = None
        self.toplevel = None
        self.configured = False
        self.size = None
        self.buffers = []
        self.by_id = {}
        self.cursor = 0
        self.commits = 0
        self.released = 0
        self.stamps = []

    def alloc(self):
        i = self.next_id
        self.next_id += 1
        return i

    def send(self, *a, **kw):
        self.wire.send(*a, **kw)

    def handle(self, obj, opcode, body):
        if obj == DISPLAY and opcode == 0:
            target, code = struct.unpack_from("<II", body)
            die(
                "protocol error on object %d, code %d: %s"
                % (target, code, read_string(body, 8)[0])
            )
        elif obj == REGISTRY and opcode == 0:
            (num,) = struct.unpack_from("<I", body)
            interface, at = read_string(body, 4)
            (version,) = struct.unpack_from("<I", body, at)
            self.globals[interface] = (num, version)
        elif opcode == 0 and obj in self.callbacks:
            self.done.add(obj)
        elif obj == self.wm_base and opcode == 0:
            self.send(self.wm_base, 3, body[:4])
        elif obj == self.xdg_surface and opcode == 0:
            self.send(self.xdg_surface, 4, body[:4])
            self.configured = True
        elif obj == self.toplevel:
            if opcode == 0:
                width, height = struct.unpack_from("<ii", body)
                if width > 0 and height > 0 and self.size is None:
                    self.size = (width, height)
            elif opcode == 1:
                print("closed by the compositor")
                sys.exit(0)
        elif obj in self.by_id and opcode == 0:
            self.by_id[obj]["busy"] = False
            self.released += 1

    def pump(self, timeout):
        ready, _, _ = select.select([self.wire], [], [], timeout)
        if not ready:
            return
        self.wire.read()
        for obj, opcode, body in self.wire.events():
            self.handle(obj, opcode, body)

    def roundtrip(self):
        cb = self.alloc()
        self.callbacks.add(cb)
        self.send(DISPLAY, 0, u32(cb))
        while cb not in self.done:
            self.pump(None)

    def bind(self, interface, version):
        if interface not in self.globals:
            return None
        name, advertised = self.globals[interface]
        obj = self.alloc()
        self.send(
            REGISTRY,
            0,
            u32(name) + string(interface) + u32(min(version, advertised), obj),
        )
        return obj

    def setup(self):
        args = self.args
        self.send(DISPLAY, 1, u32(REGISTRY))
        self.roundtrip()

        compositor = self.bind("wl_compositor", 4)
        self.wm_base = self.bind("xdg_wm_base", 1)
        shm = self.bind("wl_shm", 1)
        fifo_mgr = self.bind("wp_fifo_manager_v1", 1)
        if not compositor or not self.wm_base or not shm:
            die("wl_compositor / xdg_wm_base / wl_shm missing")
        if not fifo_mgr and not args.no_fifo:
            die("wp_fifo_manager_v1 is not advertised by the compositor")
        self.roundtrip()

        self.surface = self.alloc()
        self.send(compositor, 0, u32(self.surface))
        self.xdg_surface = self.alloc()
        self.send(self.wm_base, 2, u32(self.xdg_surface, self.surface))
        self.toplevel = self.alloc()
        self.send(self.xdg_surface, 1, u32(self.toplevel))
        self.send(self.toplevel, 2, string("fifo-test"))
        self.send(self.toplevel, 3, string("fifo-test"))
        if args.fullscreen:
            self.send(self.toplevel, 11, u32(0))

        self.fifo = None
        if fifo_mgr and not args.no_fifo:
            self.fifo = self.alloc()
            self.send(fifo_mgr, 1, u32(self.fifo, self.surface))
            print("fifo: barriers on every commit")
        else:
            print("fifo: off (no barriers)")

        self.send(self.surface, 6)
        while not self.configured:
            self.pump(None)
        self.size = self.size or (args.width, args.height)
        print(
            "window: %dx%d%s" % (*self.size, " fullscreen" if args.fullscreen else "")
        )
        self.make_buffers(shm)

    def make_buffers(self, shm):
        width, height = self.size
        stride = width * 4
        size = stride * height
        fd = os.memfd_create("fifo-test", os.MFD_CLOEXEC)
        os.ftruncate(fd, size * BUFFERS)
        mem = mmap.mmap(fd, size * BUFFERS)
        pool = self.alloc()
        self.send(shm, 0, u32(pool, size * BUFFERS), fds=[fd])
        for i in range(BUFFERS):
            shift = i * STEP
            cycle = b"".join(
                LIGHT if (x - shift) % PERIOD < STRIPE else DARK for x in range(PERIOD)
            )
            row = (cycle * (width // PERIOD + 1))[: width * 4]
            mem[i * size : (i + 1) * size] = row * height
            buf = self.alloc()
            self.send(
                pool,
                0,
                u32(buf, i * size) + i32(width, height, stride) + u32(WL_SHM_XRGB8888),
            )
            entry = {"id": buf, "busy": False}
            self.buffers.append(entry)
            self.by_id[buf] = entry
        self.send(pool, 1)
        os.close(fd)
        self.mem = mem
        self.order = list(range(len(self.buffers)))

    def draw(self):
        entry = self.buffers[self.order[self.cursor]]
        self.cursor = (self.cursor + 1) % len(self.order)
        width, height = self.size
        self.send(self.surface, 1, u32(entry["id"]) + i32(0, 0))
        self.send(self.surface, 9, i32(0, 0, width, height))
        if self.fifo:
            self.send(self.fifo, 1)
            self.send(self.fifo, 0)
        self.send(self.surface, 6)
        entry["busy"] = True
        self.commits += 1
        self.stamps.append(time.monotonic())

    def report(self, since, now):
        stamps, released = self.stamps, self.released
        self.stamps = []
        self.released = 0
        if released == 0 and self.commits >= len(self.buffers):
            print(
                "WARNING: %d commits and no buffer given back: the compositor is not showing "
                "the window or is holding the commits (check jff msg stats and the jff log)"
                % self.commits
            )
            return
        if len(stamps) < 2:
            print(
                "client commits: %d, released: %d in the last second"
                % (len(stamps), released)
            )
            return
        gaps = [(b - a) * 1000 for a, b in zip(stamps, stamps[1:])]
        print(
            "client commits: %.1f/s  released: %.1f/s  gap avg %.2f ms  min %.2f  max %.2f"
            % (
                len(stamps) / (now - since),
                released / (now - since),
                sum(gaps) / len(gaps),
                min(gaps),
                max(gaps),
            )
        )

    def run(self):
        print("running; check `jff msg stats` now (Ctrl-C or --seconds to stop)")
        start = last = time.monotonic()
        while True:
            now = time.monotonic()
            if self.args.seconds and now - start >= self.args.seconds:
                print("done")
                return
            if now - last >= 1.0:
                self.report(last, now)
                last = now
            if not self.buffers[self.order[self.cursor]]["busy"]:
                self.draw()
                self.pump(0)
            else:
                self.pump(0.25)


def main():
    parser = argparse.ArgumentParser(
        description="Commits with wp_fifo_v1 barriers and shows the pacing."
    )
    parser.add_argument(
        "--no-fifo", action="store_true", help="don't use wp_fifo_v1, for comparison"
    )
    parser.add_argument("--fullscreen", action="store_true")
    parser.add_argument(
        "--seconds", type=float, default=20, help="exit after this long (0 = never)"
    )
    parser.add_argument(
        "--width",
        type=int,
        default=1280,
        help="fallback if the compositor sends no size",
    )
    parser.add_argument("--height", type=int, default=720)
    args = parser.parse_args()

    client = Client(args)
    client.setup()
    try:
        client.run()
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
