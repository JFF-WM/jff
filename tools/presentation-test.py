#!/usr/bin/env python3
"""Manual test client for wp_presentation. Plain Python 3, speaks the wire protocol itself.

Opens a window of moving stripes and asks for presentation feedback on every commit
(wp_presentation.feedback before wl_surface.commit). For each answer it checks what the
compositor promises:

  * presented timestamps are on the clock given by wp_presentation.clock_id (CLOCK_MONOTONIC),
    never earlier than the commit they answer, never in the future, and never go backwards
  * seq (the display's msc) only goes up, and its steps agree with the time between frames
  * refresh is the display's refresh interval, the same every time (0 = unknown)
  * the flags make sense: vsync, hw_clock and hw_completion on the TTY backend, none of them
    on winit. zero_copy can't show up here, this client uses shm buffers, never scanout
  * every feedback gets exactly one answer: presented or discarded

Once a second it prints the rates, the display refresh seen in the answers, how many refreshes
were skipped between two presented frames, and any problems found. At the end it prints a
summary with PASS / WARN / FAIL per check.

Run it inside the jff session (WAYLAND_DISPLAY set):

    python3 tools/presentation-test.py                  # one commit per refresh, 10 s
    python3 tools/presentation-test.py --fullscreen
    python3 tools/presentation-test.py --burst 4        # 4 commits back to back, then wait:
                                                        # the replaced ones are discarded
    python3 tools/presentation-test.py --depth 1        # one feedback outstanding at a time

Things worth trying while it runs:

  * move the window to another workspace or open the overview: nothing is shown, so no answers
    come; when the window is shown again they arrive (or are discarded, if superseded)
  * move the window to another monitor with a different refresh rate: `refresh` should follow
  * close it with Ctrl-C while feedback is outstanding: jff must not complain

The window closes itself after --seconds (default 10; 0 = never).
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
PERIOD = 128
STEP = 8
STRIPE = 24
BUFFERS = PERIOD // STEP
DARK = b"\x50\x50\x50\xff"
LIGHT = b"\xe0\xe0\xe0\xff"
NS = 1_000_000_000
TOLERANCE_NS = 1_000_000

FLAG_NAMES = ((1, "vsync"), (2, "hw_clock"), (4, "hw_completion"), (8, "zero_copy"))


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
    sys.exit("presentation-test: " + message)


def flag_text(flags):
    names = [name for bit, name in FLAG_NAMES if flags & bit]
    return "+".join(names) or "none"


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


class Stats:
    def __init__(self):
        self.commits = 0
        self.presented = 0
        self.discarded = 0
        self.skipped = 0
        self.gaps = []
        self.latency = []
        self.flags = collections.Counter()
        self.problems = collections.Counter()


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
        self.clock = None
        self.feedbacks = {}
        self.sync_outputs = 0

        self.last_when = None
        self.last_seq = None
        self.refresh_ns = None
        self.refresh_changes = 0
        self.zero_refresh = 0
        self.seq_unused = 0
        self.examples = {}

        self.window = Stats()
        self.total = Stats()
        self.pause_until = 0.0

    def now(self):
        return time.clock_gettime_ns(
            self.clock if self.clock is not None else CLOCK_MONOTONIC
        )

    def alloc(self):
        i = self.next_id
        self.next_id += 1
        return i

    def send(self, *a, **kw):
        self.wire.send(*a, **kw)

    def problem(self, key, detail):
        self.window.problems[key] += 1
        self.total.problems[key] += 1
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

    def handle_feedback(self, obj, opcode, body):
        if opcode == 0:
            self.sync_outputs += 1
            return
        committed = self.feedbacks.pop(obj)
        if opcode == 2:
            self.window.discarded += 1
            self.total.discarded += 1
            return
        if opcode != 1:
            return
        received = self.now()
        sec_hi, sec_lo, nsec, refresh, seq_hi, seq_lo, flags = struct.unpack_from(
            "<7I", body
        )
        when = ((sec_hi << 32) | sec_lo) * NS + nsec
        seq = (seq_hi << 32) | seq_lo

        for stats in (self.window, self.total):
            stats.presented += 1
            stats.flags[flag_text(flags)] += 1
            stats.latency.append((when - committed) / 1e6)

        if when < committed - TOLERANCE_NS:
            self.problem(
                "presented before it was committed",
                "%.2f ms before the commit" % ((committed - when) / 1e6),
            )
        if when > received + TOLERANCE_NS:
            self.problem(
                "presented in the future",
                "%.2f ms after the answer arrived" % ((when - received) / 1e6),
            )

        if refresh == 0:
            self.zero_refresh += 1
        else:
            if self.refresh_ns is not None and refresh != self.refresh_ns:
                self.refresh_changes += 1
            self.refresh_ns = refresh

        if self.last_when is not None:
            gap = when - self.last_when
            if gap < 0:
                self.problem("timestamp went backwards", "%.2f ms" % (gap / 1e6))
            else:
                for stats in (self.window, self.total):
                    stats.gaps.append(gap / 1e6)
            self.check_seq(seq, gap, refresh)
        self.last_when = when
        if seq != 0:
            self.last_seq = seq
        else:
            self.seq_unused += 1

    def check_seq(self, seq, gap, refresh):
        if seq == 0 or self.last_seq is None:
            return
        step = seq - self.last_seq
        if step < 0:
            self.problem("seq went backwards", "%d -> %d" % (self.last_seq, seq))
            return
        if step == 0:
            return
        if step > 1:
            for stats in (self.window, self.total):
                stats.skipped += step - 1
        if refresh and gap >= 0:
            expected = round(gap / refresh)
            if abs(expected - step) >= 1 and step < 10_000:
                self.problem(
                    "seq step doesn't match the time gap",
                    "seq advanced %d, the time gap is %.2f refreshes"
                    % (step, gap / refresh),
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
        self.presentation = self.bind("wp_presentation", 1)
        if not compositor or not self.wm_base or not shm:
            die("wl_compositor / xdg_wm_base / wl_shm missing")
        if not self.presentation:
            die("wp_presentation is not advertised by the compositor")
        self.roundtrip()
        if self.clock is None:
            self.problem("no clock_id", "wp_presentation sent no clock_id when bound")
        print(
            "presentation clock: %s"
            % (
                "CLOCK_MONOTONIC"
                if self.clock == CLOCK_MONOTONIC
                else "id %s" % self.clock
            )
        )

        self.surface = self.alloc()
        self.send(compositor, 0, u32(self.surface))
        self.xdg_surface = self.alloc()
        self.send(self.wm_base, 2, u32(self.xdg_surface, self.surface))
        self.toplevel = self.alloc()
        self.send(self.xdg_surface, 1, u32(self.toplevel))
        self.send(self.toplevel, 2, string("presentation-test"))
        self.send(self.toplevel, 3, string("presentation-test"))
        if args.fullscreen:
            self.send(self.toplevel, 11, u32(0))

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
        fd = os.memfd_create("presentation-test", os.MFD_CLOEXEC)
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

    def entry(self, ahead=0):
        return self.buffers[(self.cursor + ahead) % len(self.buffers)]

    def draw(self):
        entry = self.entry()
        self.cursor = (self.cursor + 1) % len(self.buffers)
        width, height = self.size

        fb = self.alloc()
        self.send(self.presentation, 1, u32(self.surface, fb))
        self.feedbacks[fb] = self.now()

        self.send(self.surface, 1, u32(entry["id"]) + i32(0, 0))
        self.send(self.surface, 9, i32(0, 0, width, height))
        self.send(self.surface, 6)
        entry["busy"] = True
        self.window.commits += 1
        self.total.commits += 1

    def describe_window(self, elapsed):
        w = self.window
        line = "commits %.1f/s  presented %.1f/s" % (
            w.commits / elapsed,
            w.presented / elapsed,
        )
        if w.discarded:
            line += "  discarded %d" % w.discarded
        if self.refresh_ns:
            line += "  refresh %.3f ms (%.2f Hz)" % (
                self.refresh_ns / 1e6,
                NS / self.refresh_ns,
            )
        elif w.presented:
            line += "  refresh unknown (0)"
        if w.latency:
            line += "  commit->shown avg %.2f ms max %.2f" % (
                sum(w.latency) / len(w.latency),
                max(w.latency),
            )
        if w.gaps:
            line += "  gap avg %.2f min %.2f max %.2f" % (
                sum(w.gaps) / len(w.gaps),
                min(w.gaps),
                max(w.gaps),
            )
        if w.skipped:
            line += "  refreshes skipped %d" % w.skipped
        if w.flags:
            line += "  flags " + ", ".join(
                "%s x%d" % (text, count) for text, count in w.flags.most_common()
            )
        return line

    def report(self, elapsed):
        w = self.window
        if (
            w.presented == 0
            and w.discarded == 0
            and self.total.commits >= self.args.depth
        ):
            print(
                "WARNING: %d commits and no feedback answered: the window is not shown "
                "(another workspace, overview, locked?) or feedback is never answered"
                % w.commits
            )
        else:
            print(self.describe_window(elapsed))
        for key, count in sorted(w.problems.items()):
            print("  PROBLEM x%d: %s (%s)" % (count, key, self.examples[key]))
        self.window = Stats()

    def check_line(self, level, text):
        print("  %-5s %s" % (level, text))

    def summary(self):
        t = self.total
        print()
        print(
            "summary: %d commits, %d presented, %d discarded"
            % (t.commits, t.presented, t.discarded)
        )
        unanswered = len(self.feedbacks)
        if unanswered:
            self.check_line(
                "WARN",
                "%d feedback requests still unanswered at exit (a frame in flight is normal, "
                "more than --depth is not)" % unanswered,
            )
        else:
            self.check_line("PASS", "every feedback request got an answer")

        if t.presented == 0:
            self.check_line("FAIL", "nothing was presented")
            return

        bad = {
            key: t.problems[key]
            for key in (
                "presented before it was committed",
                "presented in the future",
                "timestamp went backwards",
                "seq went backwards",
                "seq step doesn't match the time gap",
                "no clock_id",
            )
            if t.problems[key]
        }
        for key in (
            "presented before it was committed",
            "presented in the future",
            "timestamp went backwards",
        ):
            if key in bad:
                self.check_line(
                    "FAIL", "%s: %d times (%s)" % (key, bad[key], self.examples[key])
                )
            else:
                self.check_line("PASS", "no timestamp problem: " + key)
        if self.clock != CLOCK_MONOTONIC:
            self.check_line("WARN", "clock_id is not CLOCK_MONOTONIC")
        else:
            self.check_line("PASS", "clock_id is CLOCK_MONOTONIC")

        if self.seq_unused == t.presented:
            self.check_line(
                "WARN",
                "seq is always 0 (normal on winit; on the TTY backend it should count)",
            )
        else:
            for key, ok in (
                ("seq went backwards", "seq never goes backwards"),
                (
                    "seq step doesn't match the time gap",
                    "seq steps match the time gaps",
                ),
            ):
                if key in bad:
                    self.check_line(
                        "FAIL",
                        "%s: %d times (%s)" % (key, bad[key], self.examples[key]),
                    )
                else:
                    self.check_line("PASS", ok)

        if self.zero_refresh == t.presented:
            self.check_line("WARN", "refresh is always 0 (unknown)")
        elif self.refresh_changes:
            self.check_line(
                "WARN",
                "refresh changed %d times (expected only if the window moved to another "
                "monitor)" % self.refresh_changes,
            )
        else:
            self.check_line(
                "PASS", "refresh is constant: %.3f ms" % (self.refresh_ns / 1e6)
            )

        text = ", ".join("%s x%d" % (k, v) for k, v in t.flags.most_common())
        flags_seen = set(t.flags)
        if any("zero_copy" in f for f in flags_seen):
            self.check_line("INFO", "zero_copy was reported: " + text)
        elif all(f == "none" for f in flags_seen):
            self.check_line("INFO", "no flags set (normal on winit): " + text)
        elif all("vsync" in f and "hw_completion" in f for f in flags_seen):
            self.check_line("PASS", "flags: " + text)
        else:
            self.check_line(
                "WARN", "some frames without vsync or hw_completion: " + text
            )

        if self.args.burst > 1:
            if t.discarded:
                self.check_line(
                    "PASS", "superseded commits were discarded (%d)" % t.discarded
                )
            else:
                self.check_line(
                    "WARN",
                    "burst mode, but no feedback was discarded: every commit was shown",
                )
        elif t.discarded:
            self.check_line(
                "INFO",
                "%d feedbacks discarded (window hidden or replaced?)" % t.discarded,
            )

    def burst_ready(self):
        return all(not self.entry(i)["busy"] for i in range(self.args.burst))

    def run(self):
        print("running (Ctrl-C or --seconds to stop)")
        start = last = time.monotonic()
        while True:
            mono = time.monotonic()
            if self.args.seconds and mono - start >= self.args.seconds:
                break
            if mono - last >= 1.0:
                self.report(mono - last)
                last = mono
            if mono < self.pause_until:
                self.pump(min(self.pause_until - mono, 0.25))
                continue

            outstanding = len(self.feedbacks)
            if self.args.burst > 1:
                if outstanding == 0 and self.burst_ready():
                    for _ in range(self.args.burst):
                        self.draw()
                    self.pause_until = time.monotonic() + self.args.pause / 1000
                    self.pump(0)
                else:
                    self.pump(0.05)
            else:
                if not self.entry()["busy"] and outstanding < self.args.depth:
                    self.draw()
                    self.pump(0)
                else:
                    self.pump(0.05)
        self.finish()

    def finish(self):
        deadline = time.monotonic() + 0.5
        while self.feedbacks and time.monotonic() < deadline:
            self.pump(0.05)
        self.summary()


def main():
    parser = argparse.ArgumentParser(
        description="Asks wp_presentation for feedback on every commit and checks the answers."
    )
    parser.add_argument("--fullscreen", action="store_true")
    parser.add_argument(
        "--depth",
        type=int,
        default=2,
        help="how many feedback requests are outstanding at once (1-%d)"
        % (BUFFERS - 2),
    )
    parser.add_argument(
        "--burst",
        type=int,
        default=1,
        help="commit this many frames back to back, then wait for all answers; the commits "
        "replaced before they were shown should be discarded (1 = off)",
    )
    parser.add_argument(
        "--pause",
        type=float,
        default=0,
        help="burst mode: milliseconds to wait between bursts",
    )
    parser.add_argument(
        "--seconds", type=float, default=10, help="exit after this long (0 = never)"
    )
    parser.add_argument(
        "--width",
        type=int,
        default=1280,
        help="fallback if the compositor sends no size",
    )
    parser.add_argument("--height", type=int, default=720)
    args = parser.parse_args()
    if not 1 <= args.depth <= BUFFERS - 2:
        die("--depth must be between 1 and %d" % (BUFFERS - 2))
    if not 1 <= args.burst <= BUFFERS - 2:
        die("--burst must be between 1 and %d" % (BUFFERS - 2))

    client = Client(args)
    client.setup()
    try:
        client.run()
    except KeyboardInterrupt:
        print()
        client.finish()


if __name__ == "__main__":
    main()
