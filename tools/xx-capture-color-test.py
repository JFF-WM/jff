#!/usr/bin/env python3
"""Manual test client for xx-image-capture-color-management-v1, plain Python, speaks the wire
protocol itself (like xx-zones-test.py). Run it inside the jff session (WAYLAND_DISPLAY set).

    tools/xx-capture-color-test.py [--output N] [--expect sdr|hdr] [--watch]

It captures one monitor (the first, or `--output N`) with ext-image-copy-capture and checks:

  1. the preferred description of the monitor's capture source: sRGB on an SDR monitor,
     PQ in BT.2020 on one in HDR mode, with the identity of the monitor's own description;
  2. frames with no description, and with the sRGB one, into an 8-bit buffer succeed;
  3. a frame described as PQ in BT.2020 into an 8-bit buffer fails (jff never mislabels);
  4. on an HDR monitor: PQ in BT.2020 into a 10-bit buffer succeeds, sRGB into one fails;
  5. the protocol errors: `image_description_not_ready` and `already_captured`.

The monitor is taken to be HDR when its preferred description is PQ; `--expect` makes a
mismatch a failure. `--watch` prints `preferred_changed` and the new description until Ctrl+C:
turn the monitor's HDR mode on and off meanwhile. Each check prints PASS, FAIL or INFO, the
exit status is 1 if anything failed.
"""

import argparse
import os
import socket
import struct
import sys

# ---- the wire ---------------------------------------------------------------------------------


def pad(data):
    return data + b"\0" * (-len(data) % 4)


def string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + pad(raw)


def read_string(body, at):
    (length,) = struct.unpack_from("<I", body, at)
    text = body[at + 4 : at + 4 + length - 1].decode()
    return text, at + 4 + ((length + 3) & ~3)


class Wire:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.sock.settimeout(5)
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
            try:
                chunk = self.sock.recv(4096)
            except socket.timeout:
                raise Timeout()
            if not chunk:
                raise ProtocolError(0, 0, "connection closed")
            self.buf += chunk
        obj, word = struct.unpack_from("<II", self.buf)
        size, opcode = word >> 16, word & 0xFFFF
        body, self.buf = self.buf[8:size], self.buf[size:]
        return obj, opcode, body


class Timeout(Exception):
    pass


class ProtocolError(Exception):
    """A `wl_display.error`: the object it is about, the code, the message."""

    def __init__(self, obj, code, message):
        super().__init__("error %d on object %d: %s" % (code, obj, message))
        self.obj, self.code, self.message = obj, code, message


# ---- what the protocols name ---------------------------------------------------------------------

# wl_shm formats
ARGB8888, XRGB8888 = 0, 1
XRGB2101010, XBGR2101010 = 0x30335258, 0x30334258
ARGB2101010, ABGR2101010 = 0x30335241, 0x30334241
SDR_FORMATS = [XRGB8888, ARGB8888]
HDR_FORMATS = [XRGB2101010, XBGR2101010, ARGB2101010, ABGR2101010]

# wp_color_manager_v1 enums
PRIMARIES_SRGB, PRIMARIES_BT2020 = 1, 6
TF_PQ, TF_SRGB = (
    11,
    14,
)  # 14 is compound_power_2_4, which is how a version 2+ client says sRGB
TF_NAMES = {
    2: "gamma22",
    5: "ext_linear",
    9: "srgb",
    11: "st2084_pq",
    13: "hlg",
    14: "srgb",
}
PRIMARIES_NAMES = {1: "srgb", 6: "bt2020", 8: "dci_p3", 9: "display_p3"}

# ext_image_copy_capture_frame_v1.failure_reason
REASONS = {0: "unknown", 1: "buffer_constraints", 2: "stopped"}

# xx_image_capture_color_manager_v1.error
NOT_READY, ALREADY_CAPTURED = 1, 2


class Desc:
    """A wp_image_description_v1."""

    def __init__(self, client, oid):
        self.id, self.state, self.identity, self.why = oid, None, None, None
        client.handlers[oid] = self.event

    def event(self, opcode, body):
        if opcode == 0:
            self.state, self.why = "failed", read_string(body, 4)[0]
        elif opcode == 1:
            self.state, (self.identity,) = "ready", struct.unpack_from("<I", body)
        elif opcode == 2:
            hi, lo = struct.unpack_from("<II", body)
            self.state, self.identity = "ready", (hi << 32) | lo


class Info:
    """A wp_image_description_info_v1."""

    def __init__(self, client, oid):
        self.done, self.tf, self.primaries, self.explicit = False, None, None, None
        client.handlers[oid] = self.event

    def event(self, opcode, body):
        if opcode == 0:
            self.done = True
        elif opcode == 2:
            self.explicit = struct.unpack_from("<8i", body)
        elif opcode == 3:
            (self.primaries,) = struct.unpack_from("<I", body)
        elif opcode == 5:
            (self.tf,) = struct.unpack_from("<I", body)

    def says(self):
        tf = TF_NAMES.get(self.tf, self.tf)
        primaries = PRIMARIES_NAMES.get(self.primaries, self.primaries or self.explicit)
        return "tf %s, primaries %s" % (tf, primaries)


class Session:
    """An ext_image_copy_capture_session_v1 and the constraints it announced."""

    def __init__(self, client, oid):
        self.id, self.size, self.formats, self.done = oid, None, [], False
        client.handlers[oid] = self.event

    def event(self, opcode, body):
        if opcode == 0:
            self.size = struct.unpack_from("<II", body)
        elif opcode == 1:
            self.formats.append(struct.unpack_from("<I", body)[0])
        elif opcode == 4:
            self.done = True


class Frame:
    """An ext_image_copy_capture_frame_v1: how it ended."""

    def __init__(self, client, oid):
        self.id, self.result = oid, None
        client.handlers[oid] = self.event

    def event(self, opcode, body):
        if opcode == 3:
            self.result = "ready"
        elif opcode == 4:
            (reason,) = struct.unpack_from("<I", body)
            self.result = "failed (%s)" % REASONS.get(reason, reason)


# ---- a connection --------------------------------------------------------------------------------


class Client:
    def __init__(self, output=0):
        runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
        path = os.path.join(runtime, os.environ.get("WAYLAND_DISPLAY", "wayland-0"))
        self.wire = Wire(path)
        self.next_id = 2
        self.handlers = {}
        self.globals = {}
        registry = self.new_id()
        self.handlers[registry] = self.registry_event
        self.registry = registry
        self.send(1, 1, struct.pack("<I", registry))
        self.roundtrip()
        for need in (
            "wl_output",
            "wl_shm",
            "wp_color_manager_v1",
            "xx_image_capture_color_manager_v1",
            "ext_output_image_capture_source_manager_v1",
            "ext_image_copy_capture_manager_v1",
        ):
            if need not in self.globals:
                sys.exit("%s is not advertised" % need)
        if self.globals["wp_color_manager_v1"][0][1] < 3:
            sys.exit("wp_color_manager_v1 is older than version 3")
        outputs = self.globals["wl_output"]
        if output >= len(outputs):
            sys.exit("there is no output %d (%d advertised)" % (output, len(outputs)))
        self.output = self.bind("wl_output", 1, outputs[output])
        self.shm = self.bind("wl_shm", 1)
        self.manager = self.bind("wp_color_manager_v1", 3)
        self.xx = self.bind("xx_image_capture_color_manager_v1", 1)
        self.sources = self.bind("ext_output_image_capture_source_manager_v1", 1)
        self.copy = self.bind("ext_image_copy_capture_manager_v1", 1)
        self.roundtrip()

    def new_id(self):
        self.next_id += 1
        return self.next_id - 1

    def send(self, obj, opcode, payload=b"", fds=()):
        self.wire.send(obj, opcode, payload, fds)

    def registry_event(self, opcode, body):
        if opcode == 0:
            (num,) = struct.unpack_from("<I", body)
            interface, at = read_string(body, 4)
            (version,) = struct.unpack_from("<I", body, at)
            self.globals.setdefault(interface, []).append((num, version))

    def bind(self, interface, version, which=None):
        num, offered = which or self.globals[interface][0]
        oid = self.new_id()
        self.send(
            self.registry,
            0,
            struct.pack("<I", num)
            + string(interface)
            + struct.pack("<II", min(version, offered), oid),
        )
        return oid

    def pump(self, until):
        while not until():
            obj, opcode, body = self.wire.event()
            if obj == 1 and opcode == 0:
                target, code = struct.unpack_from("<II", body)
                raise ProtocolError(target, code, read_string(body, 8)[0])
            handler = self.handlers.get(obj)
            if handler:
                handler(opcode, body)

    def roundtrip(self):
        callback, done = self.new_id(), []
        self.handlers[callback] = lambda opcode, body: done.append(True)
        self.send(1, 0, struct.pack("<I", callback))
        self.pump(lambda: bool(done))

    # -- image descriptions

    def wait(self, desc):
        self.pump(lambda: desc.state is not None)
        return desc

    def from_reference(self, reference):
        oid = self.new_id()
        self.send(self.manager, 7, struct.pack("<II", oid, reference))
        return self.wait(Desc(self, oid))

    def of_output(self):
        out = self.new_id()
        self.send(self.manager, 1, struct.pack("<II", out, self.output))
        oid = self.new_id()
        self.send(out, 1, struct.pack("<I", oid))
        return self.wait(Desc(self, oid))

    def parametric(self, tf, primaries):
        creator = self.new_id()
        self.send(self.manager, 5, struct.pack("<I", creator))
        self.send(creator, 1, struct.pack("<I", tf))
        self.send(creator, 3, struct.pack("<I", primaries))
        oid = self.new_id()
        self.send(creator, 0, struct.pack("<I", oid))
        return self.wait(Desc(self, oid))

    def broken_icc(self):
        """A description made from a file that is not an ICC profile: it fails."""
        creator = self.new_id()
        self.send(self.manager, 4, struct.pack("<I", creator))
        fd = os.memfd_create("not-a-profile")
        os.write(fd, b"\0" * 256)
        self.send(
            creator, 1, struct.pack("<II", 0, 256), fds=[fd]
        )  # set_icc_file(fd, 0, 256)
        os.close(fd)
        oid = self.new_id()
        self.send(creator, 0, struct.pack("<I", oid))
        return self.wait(Desc(self, oid))

    def info(self, desc):
        oid = self.new_id()
        self.send(desc.id, 1, struct.pack("<I", oid))
        info = Info(self, oid)
        self.pump(lambda: info.done)
        return info

    # -- the capture source and its colors

    def source(self):
        oid = self.new_id()
        self.send(self.sources, 0, struct.pack("<II", oid, self.output))
        return oid

    def source_colors(self, source, changes=None):
        colors = self.new_id()
        self.send(self.xx, 1, struct.pack("<II", colors, source))

        def event(opcode, body):
            if opcode == 0 and changes is not None:
                hi, lo = struct.unpack_from("<II", body)
                changes.append((hi << 32) | lo)

        self.handlers[colors] = event
        return colors

    def preferred(self, colors):
        reference = self.new_id()
        self.send(colors, 1, struct.pack("<I", reference))
        return self.from_reference(reference)

    # -- capturing

    def session(self):
        # The source's request goes out first, so its id is taken first: libwayland wants a
        # client's new ids in the order they are used.
        source = self.source()
        oid = self.new_id()
        self.send(self.copy, 0, struct.pack("<III", oid, source, 0))
        session = Session(self, oid)
        self.pump(lambda: session.done)
        return session

    def buffer(self, size, fmt):
        width, height = size
        total = width * 4 * height
        fd = os.memfd_create("capture-color-test")
        os.ftruncate(fd, total)
        pool = self.new_id()
        self.send(self.shm, 0, struct.pack("<II", pool, total), fds=[fd])
        buf = self.new_id()
        self.send(
            pool, 0, struct.pack("<IiiiiI", buf, 0, width, height, width * 4, fmt)
        )
        self.send(pool, 1)  # destroy: the buffer stays
        os.close(fd)
        return buf

    def frame(self, session, fmt, desc=None, capture=True):
        fid = self.new_id()
        self.send(session.id, 0, struct.pack("<I", fid))
        frame = Frame(self, fid)
        buf = self.buffer(session.size, fmt)
        self.send(fid, 1, struct.pack("<I", buf))
        self.send(fid, 2, struct.pack("<iiii", 0, 0, *session.size))
        if desc is not None:
            self.send(self.xx, 2, struct.pack("<II", fid, desc.id))
        if capture:
            self.send(fid, 3)
        return frame

    def capture(self, formats, desc=None):
        """Capture one frame in the first of `formats` the session offers. A session of its own
        every time: a later frame of a session waits for the screen to change."""
        session = self.session()
        fmt = next((f for f in formats if f in session.formats), None)
        if fmt is None:
            return None
        frame = self.frame(session, fmt, desc)
        self.pump(lambda: frame.result is not None)
        return frame.result


# ---- the checks ----------------------------------------------------------------------------------

failures = []


def report(status, name, detail=""):
    print("[%s] %s%s" % (status, name, ": " + detail if detail else ""))
    if status == "FAIL":
        failures.append(name)


def check(name, got, want):
    report(
        "PASS" if got == want else "FAIL",
        name,
        "got %s" % (got,) if got != want else str(got),
    )


def attempt(name, fn):
    """Run a check; an error from the protocol or a timeout is a failure, not a crash."""
    try:
        return fn()
    except Timeout:
        report("FAIL", name, "no answer in 5 s")
    except ProtocolError as err:
        report("FAIL", name, "protocol error: %s" % err)
    return None


def check_preferred(args):
    c = Client(args.output)
    colors = c.source_colors(c.source())
    desc = c.preferred(colors)
    if desc.state != "ready":
        report("FAIL", "preferred description", "failed: %s" % desc.why)
        return None
    info = c.info(desc)
    hdr = info.tf == TF_PQ
    report(
        "INFO",
        "preferred description",
        "%s (identity %#x)" % (info.says(), desc.identity),
    )
    if hdr:
        ok = info.primaries == PRIMARIES_BT2020
    else:
        ok = info.tf == TF_SRGB and info.primaries in (PRIMARIES_SRGB, None)
    report(
        "PASS" if ok else "FAIL",
        "preferred is what a capture holds (%s)" % ("PQ/BT.2020" if hdr else "sRGB"),
    )
    if args.expect:
        check(
            "monitor is %s as expected" % args.expect,
            "hdr" if hdr else "sdr",
            args.expect,
        )
    of_output = c.of_output()
    if of_output.identity == desc.identity:
        report("PASS", "same identity as the monitor's own description")
    else:
        report(
            "INFO",
            "identity differs from the monitor's own description",
            "fine if the monitor has an ICC profile (a capture is sRGB regardless)",
        )
    return hdr


def check_frames(hdr):
    c = Client(0)
    srgb = c.parametric(TF_SRGB, PRIMARIES_SRGB)
    pq = c.parametric(TF_PQ, PRIMARIES_BT2020)
    for name, desc in (("srgb", srgb), ("pq/bt2020", pq)):
        if desc.state != "ready":
            report("FAIL", "make the %s description" % name, "failed: %s" % desc.why)
            return
    check("8-bit frame, no description", c.capture(SDR_FORMATS), "ready")
    check("8-bit frame described as sRGB", c.capture(SDR_FORMATS, srgb), "ready")
    check(
        "8-bit frame described as PQ/BT.2020 is refused",
        c.capture(SDR_FORMATS, pq),
        "failed (unknown)",
    )
    if not hdr:
        report("INFO", "10-bit frames", "skipped, the monitor is not in HDR mode")
        return
    got = c.capture(HDR_FORMATS, pq)
    if got is None:
        report(
            "FAIL",
            "10-bit buffer on offer",
            "no 10-bit format offered on an HDR monitor",
        )
        return
    check("10-bit frame described as PQ/BT.2020", got, "ready")
    check(
        "10-bit frame described as sRGB is refused",
        c.capture(HDR_FORMATS, srgb),
        "failed (unknown)",
    )


def check_not_ready():
    c = Client(0)
    broken = c.broken_icc()
    if broken.state != "failed":
        report(
            "INFO",
            "image_description_not_ready",
            "skipped, jff accepted the bogus ICC profile",
        )
        return
    session = c.session()
    frame = c.frame(session, session.formats[0], broken, capture=False)
    try:
        c.roundtrip()
    except ProtocolError as err:
        want = (c.xx, NOT_READY)
        report(
            "PASS" if (err.obj, err.code) == want else "FAIL",
            "image_description_not_ready on a failed description",
            str(err),
        )
        return
    del frame
    report("FAIL", "image_description_not_ready on a failed description", "no error")


def check_already_captured():
    c = Client(0)
    srgb = c.parametric(TF_SRGB, PRIMARIES_SRGB)
    session = c.session()
    frame = c.frame(session, next(f for f in SDR_FORMATS if f in session.formats), srgb)
    c.pump(lambda: frame.result is not None)
    c.send(c.xx, 2, struct.pack("<II", frame.id, srgb.id))
    try:
        c.roundtrip()
    except ProtocolError as err:
        want = (c.xx, ALREADY_CAPTURED)
        report(
            "PASS" if (err.obj, err.code) == want else "FAIL",
            "already_captured when described after the capture",
            str(err),
        )
        return
    report("FAIL", "already_captured when described after the capture", "no error")


def watch(args):
    c = Client(args.output)
    changes = []
    colors = c.source_colors(c.source(), changes)

    def show(label):
        desc = c.preferred(colors)
        report(
            "INFO", label, "%s (identity %#x)" % (c.info(desc).says(), desc.identity)
        )

    show("preferred now")
    print("turn the monitor's HDR mode on and off; Ctrl+C to quit")
    seen = 0
    while True:
        try:
            c.pump(lambda: len(changes) > seen)
        except Timeout:
            continue
        seen = len(changes)
        print("preferred_changed, identity %#x" % changes[-1])
        show("preferred now")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument(
        "--output", type=int, default=0, help="which monitor (default 0)"
    )
    parser.add_argument(
        "--expect", choices=["sdr", "hdr"], help="what the monitor should be"
    )
    parser.add_argument("--watch", action="store_true", help="print preferred_changed")
    args = parser.parse_args()
    if args.watch:
        try:
            watch(args)
        except KeyboardInterrupt:
            return 0
    hdr = attempt("preferred description", lambda: check_preferred(args))
    if hdr is None:
        return 1
    attempt("frames", lambda: check_frames(hdr))
    attempt("image_description_not_ready", check_not_ready)
    attempt("already_captured", check_already_captured)
    print(
        "\n%s"
        % ("%d check(s) failed" % len(failures) if failures else "all checks passed")
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
