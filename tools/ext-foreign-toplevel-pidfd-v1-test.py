#!/usr/bin/env python3
"""Manual test client for ext-foreign-toplevel-pidfd-v1, plain Python, speaks the wire protocol.

Run it inside the jff session (WAYLAND_DISPLAY set) with some windows open:

    python3 tools/ext-foreign-toplevel-pidfd-test.py          # list once and exit
    python3 tools/ext-foreign-toplevel-pidfd-test.py --watch  # keep printing new windows

For every window it prints the identifier, app id, title, and what the pidfd is: the pid
(from /proc/self/fdinfo), the process name, and whether the process is alive
(pidfd_send_signal with signal 0). `no pidfd` is expected for X11 windows (xwayland-satellite).
Needs Python 3.9+.
"""

import os
import signal
import socket
import struct
import sys

DISPLAY, REGISTRY = 1, 2


def pad(data):
    return data + b"\0" * (-len(data) % 4)


def string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + pad(raw)


def read_string(body, at):
    (length,) = struct.unpack_from("<I", body, at)
    text = body[at + 4 : at + 4 + length - 1].decode(errors="replace")
    return text, at + 4 + ((length + 3) & ~3)


class Wire:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.buf = b""
        self.fds = []

    def send(self, obj, opcode, payload=b""):
        size = 8 + len(payload)
        self.sock.sendall(struct.pack("<II", obj, (size << 16) | opcode) + payload)

    def event(self):
        while len(self.buf) < 8 or len(self.buf) < (
            struct.unpack_from("<I", self.buf, 4)[0] >> 16
        ):
            data, ancillary, _, _ = self.sock.recvmsg(4096, socket.CMSG_LEN(16 * 4))
            if not data:
                sys.exit("connection closed")
            for level, kind, payload in ancillary:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    n = len(payload) // 4
                    self.fds += list(struct.unpack("<%di" % n, payload[: n * 4]))
            self.buf += data
        obj, word = struct.unpack_from("<II", self.buf)
        size, opcode = word >> 16, word & 0xFFFF
        body, self.buf = self.buf[8:size], self.buf[size:]
        return obj, opcode, body


def describe_pidfd(fd):
    try:
        info = open("/proc/self/fdinfo/%d" % fd).read()
        pid = next(int(l.split()[1]) for l in info.splitlines() if l.startswith("Pid:"))
        kind = os.readlink("/proc/self/fd/%d" % fd)
        name = open("/proc/%d/comm" % pid).read().strip() if pid > 0 else "?"
        try:
            signal.pidfd_send_signal(fd, 0)
            alive = "alive"
        except OSError as err:
            alive = "signal 0 failed: %s" % err
        return "pidfd %s pid=%d (%s) %s" % (kind, pid, name, alive)
    except Exception as err:
        return "pidfd fd=%d, could not inspect: %s" % (fd, err)


def main():
    watch = "--watch" in sys.argv
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
    wire = Wire(os.path.join(runtime, os.environ.get("WAYLAND_DISPLAY", "wayland-0")))

    next_id = [3]

    def new_id():
        next_id[0] += 1
        return next_id[0] - 1

    wire.send(DISPLAY, 1, struct.pack("<I", REGISTRY))
    globals_ = {}
    cb = new_id()
    wire.send(DISPLAY, 0, struct.pack("<I", cb))
    while True:
        obj, op, body = wire.event()
        if obj == REGISTRY and op == 0:
            (name,) = struct.unpack_from("<I", body)
            iface, at = read_string(body, 4)
            (version,) = struct.unpack_from("<I", body, at)
            globals_[iface] = (name, version)
        elif obj == cb:
            break
    for need in ("ext_foreign_toplevel_list_v1", "ext_foreign_toplevel_pidfd_list_v1"):
        if need not in globals_:
            sys.exit("FAIL: the compositor does not advertise %s" % need)
    print("ok: both globals are advertised")

    def bind(iface):
        name, _ = globals_[iface]
        obj = new_id()
        wire.send(
            REGISTRY,
            0,
            struct.pack("<I", name) + string(iface) + struct.pack("<II", 1, obj),
        )
        return obj

    toplist = bind("ext_foreign_toplevel_list_v1")
    pidlist = bind("ext_foreign_toplevel_pidfd_list_v1")
    wire.send(pidlist, 1, struct.pack("<I", toplist))

    handles = {}
    pid_to_parent = {}
    result = {}

    def roundtrips(count=2):
        for _ in range(count):
            sync = new_id()
            wire.send(DISPLAY, 0, struct.pack("<I", sync))
            while True:
                if not pump(sync):
                    break

    def pump(stop_at=None):
        obj, op, body = wire.event()
        if obj == DISPLAY and op == 0:
            target, code = struct.unpack_from("<II", body)
            message, _ = read_string(body, 8)
            sys.exit(
                "FAIL: protocol error on object %d, code %d: %s"
                % (target, code, message)
            )
        if obj == stop_at:
            return False
        if obj == toplist and op == 0:
            (hid,) = struct.unpack_from("<I", body)
            handles[hid] = {"identifier": "?", "app_id": "", "title": ""}
        elif obj == toplist and op == 1:
            print("note: the toplevel list sent finished")
        elif obj in handles:
            if op == 2:
                handles[obj]["title"], _ = read_string(body, 0)
            elif op == 3:
                handles[obj]["app_id"], _ = read_string(body, 0)
            elif op == 4:
                handles[obj]["identifier"], _ = read_string(body, 0)
            elif op == 0:
                handles[obj]["closed"] = True
        elif obj == pidlist and op == 0:
            pid_id, parent = struct.unpack_from("<II", body)
            pid_to_parent[pid_id] = parent
        elif obj in pid_to_parent:
            parent = pid_to_parent[obj]
            if op == 0:
                if not wire.fds:
                    sys.exit("FAIL: pidfd event without a file descriptor")
                result[parent] = describe_pidfd(wire.fds.pop(0))
            elif op == 1:
                result[parent] = "no pidfd"
        return True

    roundtrips(3)

    def report(seen):
        for hid, info in handles.items():
            if hid in seen or hid not in result:
                continue
            seen.add(hid)
            print(
                "%-12s %-24s %-32s %s"
                % (
                    info["identifier"],
                    info["app_id"][:24],
                    info["title"][:32],
                    result[hid],
                )
            )

    seen = set()
    report(seen)
    missing = [h["identifier"] for hid, h in handles.items() if hid not in result]
    if missing:
        print("FAIL: windows with no pidfd or no_fd event: %s" % ", ".join(missing))
    elif not handles:
        print("note: no windows are open")
    else:
        print("ok: every window got a pidfd or no_fd event")

    if watch:
        print("watching, Ctrl-C to stop")
        while True:
            pump()
            report(seen)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
