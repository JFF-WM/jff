#!/usr/bin/env python3
"""Manual test client for zwp_input_timestamps_v1. Plain Python 3, speaks the wire protocol itself.

Opens a plain window and asks for high-resolution timestamps on its wl_pointer, wl_keyboard and
wl_touch (zwp_input_timestamps_manager_v1). The compositor must send a timestamp event right
before each input event it belongs to. You have to produce the input: move the mouse over the
window, click, scroll, and type with the window focused. For every event the client checks:

  * a timestamp came before the event (button, axis, key and touch events always need one;
    pointer motion may go without, when the compositor moved the pointer itself)
  * the timestamp agrees with the millisecond time of the event it belongs to (within 1 ms)
  * the timestamp is on CLOCK_MONOTONIC, which jff uses, and never in the future
  * how long it took from the hardware time to the client getting the event
  * timestamps of one device never go backwards, and none is left without its event

Once a second it prints the event counts and latency, and at the end it prints a summary with
PASS / WARN / FAIL per check.

Run it inside the jff session (WAYLAND_DISPLAY set):

    python3 tools/input-timestamps-test.py              # 30 s
    python3 tools/input-timestamps-test.py --fullscreen
    python3 tools/input-timestamps-test.py --verbose    # one line per event

Click the window once so it has keyboard focus. The window closes itself after --seconds
(default 30; 0 = never).
"""

import argparse
import array
import collections
import mmap
import os
import select
import socket
import struct
import sys
import time

DISPLAY, REGISTRY = 1, 2
WL_SHM_XRGB8888 = 1
CLOCK_MONOTONIC = 1
NS = 1_000_000_000
KINDS = ("pointer", "keyboard", "touch")
CAPABILITY = {"pointer": 1, "keyboard": 2, "touch": 4}
TABLES = {
    "pointer": {2: ("motion", 0), 3: ("button", 4), 4: ("axis", 0)},
    "keyboard": {3: ("key", 4)},
    "touch": {0: ("down", 4), 1: ("up", 4), 2: ("motion", 0)},
}
UNTIMED = {"pointer": {0: "enter", 1: "leave"}, "keyboard": {1: "enter", 2: "leave"}}
ORPHAN = "timestamp not followed by its event"
ORPHAN_ENTER = "timestamp sent before an enter or leave"
LATE_MS = 20.0


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
    sys.exit("input-timestamps-test: " + message)


def new_counters():
    return {kind: {"events": 0, "stamped": 0, "lat": []} for kind in KINDS}


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

        self.seat = None
        self.capabilities = None
        self.devices = {}
        self.stamp_kind = {}
        self.pending = {kind: [] for kind in KINDS}
        self.between = {kind: [] for kind in KINDS}
        self.last_stamp = {}
        self.untimed_motion = 0

        self.win = new_counters()
        self.tot = new_counters()
        self.window_problems = collections.Counter()
        self.problems = collections.Counter()
        self.examples = {}

    def now(self):
        return time.clock_gettime_ns(CLOCK_MONOTONIC)

    def alloc(self):
        i = self.next_id
        self.next_id += 1
        return i

    def send(self, *a, **kw):
        self.wire.send(*a, **kw)

    def problem(self, kind, text, detail):
        key = "%s: %s" % (kind, text)
        self.window_problems[key] += 1
        self.problems[key] += 1
        self.examples.setdefault(key, detail)

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
        elif obj == self.seat and opcode == 0:
            (self.capabilities,) = struct.unpack_from("<I", body)
        elif obj in self.stamp_kind and opcode == 0:
            self.on_stamp(self.stamp_kind[obj], body)
        elif obj in self.devices:
            self.on_input(self.devices[obj], opcode, body)
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

    def on_stamp(self, kind, body):
        sec_hi, sec_lo, nsec = struct.unpack_from("<3I", body)
        stamp = ((sec_hi << 32) | sec_lo) * NS + nsec
        if self.pending[kind]:
            seen = self.between[kind]
            if seen:
                self.problem(
                    kind,
                    ORPHAN_ENTER,
                    "%s came instead of an event with a time" % "+".join(seen),
                )
            else:
                self.problem(
                    kind,
                    ORPHAN,
                    "nothing came at all: the event may have gone to a compositor grab",
                )
            self.pending[kind].clear()
        self.between[kind] = []
        self.pending[kind].append(stamp)

    def on_input(self, kind, opcode, body):
        entry = TABLES[kind].get(opcode)
        if entry is None:
            name = UNTIMED.get(kind, {}).get(opcode)
            if name:
                self.between[kind].append(name)
            return
        name, at = entry
        (time_ms,) = struct.unpack_from("<I", body, at)
        received = self.now()
        label = "%s.%s" % (kind, name)
        for counters in (self.win, self.tot):
            counters[kind]["events"] += 1

        if not self.pending[kind]:
            if label == "pointer.motion":
                self.untimed_motion += 1
            else:
                self.problem(kind, "event without a timestamp", label)
            if self.args.verbose:
                print("%-16s time %u ms  (no timestamp)" % (label, time_ms))
            return
        stamp = self.pending[kind].pop(0)
        for counters in (self.win, self.tot):
            counters[kind]["stamped"] += 1

        diff = ((stamp // 1_000_000) - time_ms + 2**31) % 2**32 - 2**31
        if abs(diff) > 1:
            self.problem(
                kind,
                "timestamp doesn't match the event's ms time",
                "%s: timestamp %d ms, event time %u ms"
                % (label, stamp // 1_000_000, time_ms),
            )

        latency = received - stamp
        if abs(latency) > NS:
            self.problem(
                kind,
                "timestamp is not on CLOCK_MONOTONIC",
                "%s: %.3f s away from the client's clock" % (label, latency / 1e9),
            )
        else:
            if latency < -1_000_000:
                self.problem(
                    kind,
                    "timestamp in the future",
                    "%s: %.2f ms ahead of the client's clock" % (label, -latency / 1e6),
                )
            for counters in (self.win, self.tot):
                counters[kind]["lat"].append(latency / 1e6)

        last = self.last_stamp.get(kind)
        if last is not None and stamp < last:
            self.problem(
                kind,
                "timestamps went backwards",
                "%s: %.3f ms" % (label, (last - stamp) / 1e6),
            )
        self.last_stamp[kind] = stamp

        if self.args.verbose:
            print(
                "%-16s time %u ms  timestamp %.6f s  latency %.3f ms"
                % (label, time_ms, stamp / 1e9, latency / 1e6)
            )

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
        self.seat = self.bind("wl_seat", 1)
        manager = self.bind("zwp_input_timestamps_manager_v1", 1)
        if not compositor or not self.wm_base or not shm or not self.seat:
            die("wl_compositor / xdg_wm_base / wl_shm / wl_seat missing")
        if not manager:
            die("zwp_input_timestamps_manager_v1 is not advertised by the compositor")
        self.roundtrip()
        if self.capabilities is None:
            die("the seat announced no capabilities")

        requests = {"pointer": (0, 2), "keyboard": (1, 1), "touch": (2, 3)}
        have = []
        for kind in KINDS:
            if not self.capabilities & CAPABILITY[kind]:
                continue
            get_device, get_stamps = requests[kind]
            device = self.alloc()
            self.send(self.seat, get_device, u32(device))
            self.devices[device] = kind
            stamps = self.alloc()
            self.send(manager, get_stamps, u32(stamps, device))
            self.stamp_kind[stamps] = kind
            have.append(kind)
        if not have:
            die("the seat has no pointer, keyboard or touch")
        print("timestamps requested for: " + ", ".join(have))

        self.surface = self.alloc()
        self.send(compositor, 0, u32(self.surface))
        self.xdg_surface = self.alloc()
        self.send(self.wm_base, 2, u32(self.xdg_surface, self.surface))
        self.toplevel = self.alloc()
        self.send(self.xdg_surface, 1, u32(self.toplevel))
        self.send(self.toplevel, 2, string("input-timestamps-test"))
        self.send(self.toplevel, 3, string("input-timestamps-test"))
        if args.fullscreen:
            self.send(self.toplevel, 11, u32(0))

        self.send(self.surface, 6)
        while not self.configured:
            self.pump(None)
        self.size = self.size or (args.width, args.height)
        print(
            "window: %dx%d%s" % (*self.size, " fullscreen" if args.fullscreen else "")
        )
        self.show(shm)

    def show(self, shm):
        width, height = self.size
        stride = width * 4
        size = stride * height
        fd = os.memfd_create("input-timestamps-test", os.MFD_CLOEXEC)
        os.ftruncate(fd, size)
        mem = mmap.mmap(fd, size)
        mem[:] = b"\x40\x40\x40\xff" * (width * height)
        pool = self.alloc()
        self.send(shm, 0, u32(pool, size), fds=[fd])
        buf = self.alloc()
        self.send(
            pool, 0, u32(buf, 0) + i32(width, height, stride) + u32(WL_SHM_XRGB8888)
        )
        self.send(pool, 1)
        os.close(fd)
        self.mem = mem
        self.send(self.surface, 1, u32(buf) + i32(0, 0))
        self.send(self.surface, 9, i32(0, 0, width, height))
        self.send(self.surface, 6)

    def report(self):
        parts = []
        for kind in KINDS:
            c = self.win[kind]
            if not c["events"]:
                continue
            text = "%s %d events (%d with timestamp)" % (
                kind,
                c["events"],
                c["stamped"],
            )
            if c["lat"]:
                text += " latency avg %.2f ms max %.2f" % (
                    sum(c["lat"]) / len(c["lat"]),
                    max(c["lat"]),
                )
            parts.append(text)
        if parts and not self.args.verbose:
            print("  |  ".join(parts))
        for key, count in sorted(self.window_problems.items()):
            print("  PROBLEM x%d: %s (%s)" % (count, key, self.examples[key]))
        self.win = new_counters()
        self.window_problems = collections.Counter()

    def check_line(self, level, text):
        print("  %-5s %s" % (level, text))

    def summary(self):
        print()
        total = sum(c["events"] for c in self.tot.values())
        print("summary: %d input events" % total)
        if total == 0:
            self.check_line(
                "INFO",
                "no input arrived: move the mouse over the window, click it, type",
            )
            return
        checks = (
            ("event without a timestamp", "every event had its timestamp"),
            (ORPHAN, "no timestamp was left without its event"),
            (ORPHAN_ENTER, "no timestamp was sent for an enter or leave"),
            (
                "timestamp doesn't match the event's ms time",
                "timestamps match the events' ms time",
            ),
            (
                "timestamp is not on CLOCK_MONOTONIC",
                "timestamps are on CLOCK_MONOTONIC",
            ),
            ("timestamp in the future", "no timestamp in the future"),
            ("timestamps went backwards", "timestamps never went backwards"),
        )
        for kind in KINDS:
            c = self.tot[kind]
            if not c["events"]:
                self.check_line("INFO", "no %s events" % kind)
                continue
            print(
                "  %s: %d events, %d with timestamp" % (kind, c["events"], c["stamped"])
            )
            if not c["stamped"]:
                self.check_line("FAIL", "%s: not one timestamp arrived" % kind)
                continue
            for text, ok in checks:
                key = "%s: %s" % (kind, text)
                if self.problems[key]:
                    self.check_line(
                        "FAIL",
                        "%s x%d (%s)" % (key, self.problems[key], self.examples[key]),
                    )
                else:
                    self.check_line("PASS", "%s: %s" % (kind, ok))
            lat = c["lat"]
            if lat:
                avg = sum(lat) / len(lat)
                line = "%s latency avg %.2f ms, max %.2f ms" % (kind, avg, max(lat))
                self.check_line("WARN" if avg > LATE_MS else "INFO", line)
        if self.untimed_motion:
            stamped_motion = self.tot["pointer"]["stamped"] > 0
            self.check_line(
                "INFO" if stamped_motion else "WARN",
                "%d pointer motions came without a timestamp%s"
                % (
                    self.untimed_motion,
                    " (expected for motion the compositor made itself)"
                    if stamped_motion
                    else ": check that device motion is stamped",
                ),
            )

    def run(self):
        print("running: move the mouse over the window, click it, scroll and type")
        start = last = time.monotonic()
        while True:
            now = time.monotonic()
            if self.args.seconds and now - start >= self.args.seconds:
                break
            if now - last >= 1.0:
                self.report()
                last = now
            self.pump(0.25)
        self.finish()

    def finish(self):
        self.report()
        self.summary()


def main():
    parser = argparse.ArgumentParser(
        description="Asks for input timestamps and checks them against the input events."
    )
    parser.add_argument("--fullscreen", action="store_true")
    parser.add_argument("--verbose", action="store_true", help="print every event")
    parser.add_argument(
        "--seconds", type=float, default=30, help="exit after this long (0 = never)"
    )
    parser.add_argument(
        "--width",
        type=int,
        default=640,
        help="fallback if the compositor sends no size",
    )
    parser.add_argument("--height", type=int, default=480)
    args = parser.parse_args()

    client = Client(args)
    client.setup()
    try:
        client.run()
    except KeyboardInterrupt:
        print()
        client.finish()


if __name__ == "__main__":
    main()
