#!/usr/bin/env python3
"""Test client for xdg-dbus-annotation-v1, plain Python, speaks the wire protocol itself.

Every case runs on its own connection (a protocol error ends the connection) and prints
PASS, FAIL or SKIP. Exit status is 1 when a case failed. Run it inside the jff session
(WAYLAND_DISPLAY set). With `RUST_LOG=jff_wm=debug` jff logs every address it accepts, which
is the only way to see the stored state from outside.

    python3 tools/xdg-dbus-annotation-test.py [CASE ...]
"""

import os
import socket
import struct
import sys
import traceback

MANAGER = "xdg_dbus_annotation_manager_v1"
ALREADY_ANNOTATED, BAD_TARGET = 0, 1

# Request opcodes.
DISPLAY_SYNC, DISPLAY_GET_REGISTRY = 0, 1
REGISTRY_BIND = 0
MANAGER_DESTROY, MANAGER_CREATE_CLIENT, MANAGER_CREATE_SURFACE = 0, 1, 2
ANNOTATION_DESTROY, ANNOTATION_SET_ADDRESS = 0, 1
COMPOSITOR_CREATE_SURFACE = 0
SUBCOMPOSITOR_GET_SUBSURFACE = 1
WM_BASE_GET_XDG_SURFACE = 2
XDG_SURFACE_GET_TOPLEVEL = 1


class Skip(Exception):
    """The compositor lacks a global the case needs."""


def u32(value):
    return struct.pack("<I", value)


def string(text):
    raw = text.encode() + b"\0"
    return u32(len(raw)) + raw + b"\0" * (-len(raw) % 4)


def read_string(body, at):
    (length,) = struct.unpack_from("<I", body, at)
    text = body[at + 4 : at + 4 + length - 1].decode()
    return text, at + 4 + ((length + 3) & ~3)


class Conn:
    """One Wayland connection with object id allocation, a registry and error capture."""

    def __init__(self):
        runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
        path = os.path.join(runtime, os.environ.get("WAYLAND_DISPLAY", "wayland-0"))
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(5)
        self.sock.connect(path)
        self.bound = {}
        self.buf = b""
        self.next_id = 2
        self.errors = []
        self.globals = {}
        self.registry = self.new_id()
        self.send(1, DISPLAY_GET_REGISTRY, u32(self.registry))
        self.roundtrip()

    def new_id(self):
        self.next_id += 1
        return self.next_id - 1

    def send(self, obj, opcode, *parts):
        payload = b"".join(parts)
        try:
            self.sock.sendall(
                struct.pack("<II", obj, ((8 + len(payload)) << 16) | opcode) + payload
            )
        except (BrokenPipeError, ConnectionResetError):
            # The compositor already ended the connection, the error event says why.
            pass

    def event(self):
        while len(self.buf) < 8 or len(self.buf) < (
            struct.unpack_from("<I", self.buf, 4)[0] >> 16
        ):
            try:
                chunk = self.sock.recv(4096)
            except ConnectionResetError:
                return None
            if not chunk:
                return None
            self.buf += chunk
        obj, word = struct.unpack_from("<II", self.buf)
        size, opcode = word >> 16, word & 0xFFFF
        body, self.buf = self.buf[8:size], self.buf[size:]
        return obj, opcode, body

    def handle(self, obj, opcode, body):
        """Record `wl_display.error` and the registry's globals."""
        if obj == 1 and opcode == 0:
            target, code = struct.unpack_from("<II", body)
            self.errors.append((target, code, read_string(body, 8)[0]))
        elif obj == self.registry and opcode == 0:
            name = struct.unpack_from("<I", body)[0]
            interface, _ = read_string(body, 4)
            self.globals[interface] = name

    def roundtrip(self):
        """Wait until the compositor has handled everything sent so far.

        Returns False when the connection was closed first, which is what a protocol error
        looks like after the `wl_display.error` event.
        """
        callback = self.new_id()
        self.send(1, DISPLAY_SYNC, u32(callback))
        while True:
            ev = self.event()
            if ev is None:
                return False
            self.handle(*ev)
            if ev[0] == callback and ev[1] == 0:
                return True

    def drain(self):
        """Read until the compositor closes the connection.

        No `sync` is sent: a client that has unread data in flight when jff kills it can get
        a reset that throws away the error event it was about to read.
        """
        while (ev := self.event()) is not None:
            self.handle(*ev)

    def bind(self, interface):
        if interface in self.bound:
            return self.bound[interface]
        if interface not in self.globals:
            raise Skip(interface)
        # The compositor only accepts ids in the order they are created, with no gaps, so an
        # id is taken right before the request that creates it is sent.
        new = self.new_id()
        self.bound[interface] = new
        self.send(
            self.registry,
            REGISTRY_BIND,
            u32(self.globals[interface]),
            string(interface),
            u32(1),
            u32(new),
        )
        return new

    def ok(self):
        assert self.roundtrip() and not self.errors, "unexpected error: %r" % (
            self.errors,
        )

    def fails(self, obj, code):
        self.drain()
        assert self.errors, "no protocol error, expected code %d" % code
        target, got, message = self.errors[0]
        assert (target, got) == (obj, code), "got %r, expected object %d code %d" % (
            self.errors[0],
            obj,
            code,
        )


def annotate_client(c, manager, interface):
    new = c.new_id()
    c.send(manager, MANAGER_CREATE_CLIENT, string(interface), u32(new))
    return new


def annotate_surface(c, manager, interface, surface):
    new = c.new_id()
    c.send(manager, MANAGER_CREATE_SURFACE, string(interface), u32(new), u32(surface))
    return new


def set_address(c, annotation, service, path):
    c.send(annotation, ANNOTATION_SET_ADDRESS, string(service), string(path))


def plain_surface(c):
    compositor = c.bind("wl_compositor")
    surface = c.new_id()
    c.send(compositor, COMPOSITOR_CREATE_SURFACE, u32(surface))
    return surface


def toplevel_surface(c):
    surface = plain_surface(c)
    wm_base = c.bind("xdg_wm_base")
    xdg_surface = c.new_id()
    c.send(wm_base, WM_BASE_GET_XDG_SURFACE, u32(xdg_surface), u32(surface))
    c.send(xdg_surface, XDG_SURFACE_GET_TOPLEVEL, u32(c.new_id()))
    return surface


def case_client_annotation(c):
    m = c.bind(MANAGER)
    set_address(c, annotate_client(c, m, "org.test.Menu"), "org.test.App", "/Menu")
    c.ok()


def case_surface_annotation_without_role(c):
    m = c.bind(MANAGER)
    set_address(
        c,
        annotate_surface(c, m, "org.test.Menu", plain_surface(c)),
        ":1.42",
        "/MenuBar",
    )
    c.ok()


def case_surface_annotation_on_toplevel(c):
    m = c.bind(MANAGER)
    set_address(
        c,
        annotate_surface(c, m, "org.test.Menu", toplevel_surface(c)),
        ":1.42",
        "/MenuBar",
    )
    c.ok()


def case_address_can_be_updated(c):
    m = c.bind(MANAGER)
    a = annotate_client(c, m, "org.test.Menu")
    set_address(c, a, "org.test.One", "/One")
    set_address(c, a, "org.test.Two", "/Two")
    c.ok()


def case_invalid_address_is_ignored(c):
    m = c.bind(MANAGER)
    a = annotate_client(c, m, "org.test.Menu")
    set_address(c, a, "org.test.App", "not/a/path")
    set_address(c, a, "single", "/Menu")
    set_address(c, a, "", "/")
    c.ok()


def case_invalid_interface_is_inert(c):
    m = c.bind(MANAGER)
    a = annotate_client(c, m, "")
    set_address(c, a, "org.test.App", "/Menu")
    annotate_client(c, m, "")
    c.send(a, ANNOTATION_DESTROY)
    c.ok()


def case_duplicate_client(c):
    m = c.bind(MANAGER)
    annotate_client(c, m, "org.test.Menu")
    annotate_client(c, m, "org.test.Menu")
    c.fails(m, ALREADY_ANNOTATED)


def case_duplicate_surface(c):
    m = c.bind(MANAGER)
    s = plain_surface(c)
    annotate_surface(c, m, "org.test.Menu", s)
    annotate_surface(c, m, "org.test.Menu", s)
    c.fails(m, ALREADY_ANNOTATED)


def case_other_interface_or_surface_is_fine(c):
    m = c.bind(MANAGER)
    s1, s2 = plain_surface(c), plain_surface(c)
    annotate_surface(c, m, "org.test.Menu", s1)
    annotate_surface(c, m, "org.test.Other", s1)
    annotate_surface(c, m, "org.test.Menu", s2)
    c.ok()


def case_client_and_surface_do_not_clash(c):
    m = c.bind(MANAGER)
    annotate_client(c, m, "org.test.Menu")
    annotate_surface(c, m, "org.test.Menu", plain_surface(c))
    c.ok()


def case_recreate_after_destroy(c):
    m = c.bind(MANAGER)
    s = plain_surface(c)
    first = annotate_surface(c, m, "org.test.Menu", s)
    c.send(first, ANNOTATION_DESTROY)
    annotate_surface(c, m, "org.test.Menu", s)
    c.ok()


def case_manager_destroy_keeps_annotations(c):
    m = c.bind(MANAGER)
    a = annotate_client(c, m, "org.test.Menu")
    c.send(m, MANAGER_DESTROY)
    set_address(c, a, "org.test.App", "/Menu")
    c.ok()


def case_subsurface_is_bad_target(c):
    m = c.bind(MANAGER)
    sub, parent = plain_surface(c), plain_surface(c)
    subcompositor = c.bind("wl_subcompositor")
    c.send(
        subcompositor,
        SUBCOMPOSITOR_GET_SUBSURFACE,
        u32(c.new_id()),
        u32(sub),
        u32(parent),
    )
    annotate_surface(c, m, "org.test.Menu", sub)
    c.fails(m, BAD_TARGET)


CASES = [
    (name[5:], fn) for name, fn in sorted(globals().items()) if name.startswith("case_")
]


def main():
    wanted = set(sys.argv[1:])
    unknown = wanted - {name for name, _ in CASES}
    if unknown:
        sys.exit(
            "unknown case(s): %s\nknown: %s"
            % (", ".join(sorted(unknown)), ", ".join(n for n, _ in CASES))
        )
    failed = 0
    for name, fn in CASES:
        if wanted and name not in wanted:
            continue
        try:
            c = Conn()
            if MANAGER not in c.globals:
                sys.exit("jff does not offer %s" % MANAGER)
            fn(c)
            print("PASS  %s" % name)
        except Skip as missing:
            print("SKIP  %s (no %s)" % (name, missing))
        except AssertionError as err:
            failed += 1
            print("FAIL  %s: %s" % (name, err))
        except Exception:
            failed += 1
            print("FAIL  %s" % name)
            traceback.print_exc()
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
