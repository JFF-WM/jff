#!/usr/bin/env python3
"""Manual test client for wp_commit_timing_v1. Plain Python 3, speaks the wire protocol itself.

Opens a window of moving vertical stripes and commits a new frame every 1/fps seconds, each
commit carrying a target time (wp_commit_timer_v1.set_timestamp): "do not show this before T".
The client commits a few frames ahead of time (--ahead), so the compositor has to hold them
back until their time has come. Every commit also asks wp_presentation for feedback, and the
time the frame really reached the screen is compared with its target:

    late = presented - target

A correct compositor never shows a frame early (late >= 0, give or take a little clock jitter)
and shows it at the first refresh after the target (late between 0 and one refresh interval).
At the default 30 fps on a 60 Hz display you should see every second refresh get a new frame,
and the stripes glide at a steady pace; irregular gaps mean the timing is off.

Run it inside the jff session (WAYLAND_DISPLAY set):

    python3 tools/commit-timing-test.py                 # 30 fps, 3 frames ahead
    python3 tools/commit-timing-test.py --fps 15        # slower, easier to see
    python3 tools/commit-timing-test.py --ahead 6       # queue more commits in advance
    python3 tools/commit-timing-test.py --fullscreen
    python3 tools/commit-timing-test.py --no-timing     # no timer: the client waits for each
                                                        # target itself, for comparison

While it runs, in another terminal:

    jff msg stats

Expect, with the timer:

    client_asked.sync_protocols.commit_timing   true
    presented per second                        about --fps
    early                                       0
    late                                        between 0 and one refresh interval

If wp_commit_timing_manager_v1 is not advertised, the compositor doesn't offer the protocol.
If `early` is not 0, commits are let through before their target time. If nothing is ever
presented ("WARNING" line), the compositor holds the commits and never releases them.
If `commit_timing` stays false in the stats while the commits are paced, the timers are being
honored without jff seeing them.

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
CLOCK_MONOTONIC = 1
PERIOD = 128
STEP = 8
STRIPE = 24
BUFFERS = PERIOD // STEP
DARK = b"\x50\x50\x50\xff"
LIGHT = b"\xe0\xe0\xe0\xff"
NS = 1_000_000_000
EARLY_TOLERANCE_MS = 1.0


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
    sys.exit("commit-timing-test: " + message)


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

        self.presentation = None
        self.timer = None
        self.clock = CLOCK_MONOTONIC
        self.interval = int(NS / args.fps)
        self.next_target = 0
        self.feedbacks = {}  # feedback object id -> target time (ns)

        # counters for the current one-second report
        self.commits = 0
        self.released = 0
        self.presented = 0
        self.discarded = 0
        self.lateness = []  # ms, presented - target
        self.early = 0
        self.presented_at = []  # ns
        self.last_presented = None
        self.refresh_ns = 0

    def now(self):
        return time.clock_gettime_ns(self.clock)

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
        elif obj == self.presentation and opcode == 0:
            (self.clock,) = struct.unpack_from("<I", body)
        elif obj in self.feedbacks:
            self.handle_feedback(obj, opcode, body)
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

    def handle_feedback(self, obj, opcode, body):
        if opcode == 0:  # sync_output
            return
        target = self.feedbacks.pop(obj)
        if opcode == 2:  # discarded
            self.discarded += 1
            return
        if opcode != 1:
            return
        sec_hi, sec_lo, nsec, refresh, _, _, _ = struct.unpack_from("<7I", body)
        when = ((sec_hi << 32) | sec_lo) * NS + nsec
        if refresh:
            self.refresh_ns = refresh
        late = (when - target) / 1e6
        self.presented += 1
        self.lateness.append(late)
        if late < -EARLY_TOLERANCE_MS:
            self.early += 1
        self.presented_at.append(when)

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
        timing_mgr = self.bind("wp_commit_timing_manager_v1", 1)
        self.presentation = self.bind("wp_presentation", 1)
        if not compositor or not self.wm_base or not shm:
            die("wl_compositor / xdg_wm_base / wl_shm missing")
        if not self.presentation:
            die(
                "wp_presentation is not advertised, can't measure when frames are shown"
            )
        if not timing_mgr and not args.no_timing:
            die("wp_commit_timing_manager_v1 is not advertised by the compositor")
        self.roundtrip()  # wp_presentation.clock_id arrives here
        if self.clock != CLOCK_MONOTONIC:
            print(
                "note: the presentation clock is %d, not CLOCK_MONOTONIC" % self.clock
            )

        self.surface = self.alloc()
        self.send(compositor, 0, u32(self.surface))
        self.xdg_surface = self.alloc()
        self.send(self.wm_base, 2, u32(self.xdg_surface, self.surface))
        self.toplevel = self.alloc()
        self.send(self.xdg_surface, 1, u32(self.toplevel))
        self.send(self.toplevel, 2, string("commit-timing-test"))
        self.send(self.toplevel, 3, string("commit-timing-test"))
        if args.fullscreen:
            self.send(self.toplevel, 11, u32(0))

        if timing_mgr and not args.no_timing:
            self.timer = self.alloc()
            self.send(timing_mgr, 1, u32(self.timer, self.surface))
            print(
                "commit timing: a target time on every commit, %.1f fps, %d ahead"
                % (args.fps, args.ahead)
            )
        else:
            print("commit timing: off, the client waits for each target itself")

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
        fd = os.memfd_create("commit-timing-test", os.MFD_CLOEXEC)
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
        target = self.next_target
        self.next_target += self.interval

        # Both requests have to come before the commit they belong to.
        fb = self.alloc()
        self.send(self.presentation, 1, u32(self.surface, fb))
        self.feedbacks[fb] = target
        if self.timer:
            sec, nsec = divmod(target, NS)
            self.send(self.timer, 0, u32(sec >> 32, sec, nsec))  # set_timestamp

        self.send(self.surface, 1, u32(entry["id"]) + i32(0, 0))
        self.send(self.surface, 9, i32(0, 0, width, height))
        self.send(self.surface, 6)
        entry["busy"] = True
        self.commits += 1

    def report(self, elapsed):
        commits, released = self.commits, self.released
        presented, discarded, early = self.presented, self.discarded, self.early
        late, stamps = self.lateness, self.presented_at
        self.commits = self.released = self.presented = self.discarded = self.early = 0
        self.lateness, self.presented_at = [], []

        if presented == 0 and commits >= self.args.ahead:
            print(
                "WARNING: %d commits and nothing presented: the compositor is holding the "
                "commits or not showing the window (check jff msg stats and the jff log)"
                % commits
            )
            return
        line = "commits %.1f/s  presented %.1f/s" % (
            commits / elapsed,
            presented / elapsed,
        )
        if discarded:
            line += "  discarded %d" % discarded
        if late:
            line += "  late avg %.2f ms  min %.2f  max %.2f  early %d" % (
                sum(late) / len(late),
                min(late),
                max(late),
                early,
            )
        gaps = [(b - a) / 1e6 for a, b in zip(stamps, stamps[1:])]
        if gaps:
            line += "  gap avg %.2f ms  min %.2f  max %.2f (want %.2f)" % (
                sum(gaps) / len(gaps),
                min(gaps),
                max(gaps),
                self.interval / 1e6,
            )
        print(line)
        if early:
            print(
                "  ^ %d frames were shown more than %.0f ms before their target time"
                % (early, EARLY_TOLERANCE_MS)
            )

    def run(self):
        print("running; check `jff msg stats` now (Ctrl-C or --seconds to stop)")
        if self.refresh_ns == 0:
            print("(refresh interval is learned from the first presented frame)")
        lead = max(3 * self.interval, 200_000_000)
        self.next_target = self.now() + lead
        start = last = time.monotonic()
        while True:
            mono = time.monotonic()
            if self.args.seconds and mono - start >= self.args.seconds:
                print("done")
                return
            if mono - last >= 1.0:
                self.report(mono - last)
                last = mono

            entry = self.buffers[self.order[self.cursor]]
            free = not entry["busy"] and len(self.feedbacks) < self.args.ahead
            wait = 0.05
            if free and self.timer is None:
                # No timer: the client holds the frame back itself.
                remaining = self.next_target - self.now()
                if remaining > 0:
                    free = False
                    wait = min(remaining / 1e9, 0.25)
            if free:
                self.draw()
                self.pump(0)
            else:
                self.pump(wait)


def main():
    parser = argparse.ArgumentParser(
        description="Commits with wp_commit_timing_v1 target times and shows when the frames "
        "are really presented."
    )
    parser.add_argument(
        "--no-timing",
        action="store_true",
        help="don't use wp_commit_timing_v1: the client waits for each target itself",
    )
    parser.add_argument("--fps", type=float, default=30.0, help="target frame rate")
    parser.add_argument(
        "--ahead",
        type=int,
        default=3,
        help="how many frames are committed in advance (1-%d)" % (BUFFERS - 2),
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
    if args.fps <= 0:
        die("--fps must be positive")
    if not 1 <= args.ahead <= BUFFERS - 2:
        die("--ahead must be between 1 and %d" % (BUFFERS - 2))

    client = Client(args)
    client.setup()
    try:
        client.run()
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
