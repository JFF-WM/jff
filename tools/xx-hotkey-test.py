#!/usr/bin/env python3
"""Manual test client for xx-hotkey-v1, plain Python, speaks the wire protocol itself.

Binds Super+Shift+B and prints `triggered` / `released`, and checks that a plain `a`
is denied with not_permitted. Run it inside the jff session (WAYLAND_DISPLAY set).
"""

import os
import socket
import struct
import sys

SHIFT, CTRL, ALT, SUPER = 1, 2, 4, 8
KEY_B, KEY_A = 0x62, 0x61
DENY = {0: "already_bound", 1: "not_permitted"}


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

    def send(self, obj, opcode, payload=b""):
        size = 8 + len(payload)
        self.sock.sendall(struct.pack("<II", obj, (size << 16) | opcode) + payload)

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


def main():
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
    wire = Wire(os.path.join(runtime, os.environ.get("WAYLAND_DISPLAY", "wayland-0")))
    DISPLAY, REGISTRY, SYNC, MANAGER, HOTKEY_B, HOTKEY_A = 1, 2, 3, 4, 5, 6

    wire.send(DISPLAY, 1, struct.pack("<I", REGISTRY))
    wire.send(DISPLAY, 0, struct.pack("<I", SYNC))
    name = None
    while True:
        obj, opcode, body = wire.event()
        if obj == REGISTRY and opcode == 0:  # wl_registry.global
            (num,) = struct.unpack_from("<I", body)
            interface, at = read_string(body, 4)
            (version,) = struct.unpack_from("<I", body, at)
            if interface == "xx_hotkey_manager_v1":
                name, mgr_version = num, version
        elif obj == DISPLAY and opcode == 0:
            sys.exit("protocol error: %r" % (read_string(body, 8)[0],))
        elif obj == SYNC:
            break
    if name is None:
        sys.exit(
            "xx_hotkey_manager_v1 is not advertised (is `global-hotkeys` on in config.kdl?)"
        )

    wire.send(
        REGISTRY,
        0,
        struct.pack("<I", name)
        + string("xx_hotkey_manager_v1")
        + struct.pack("<II", 1, MANAGER),
    )
    wire.send(MANAGER, 1, string("org.example.hotkey-test"))  # set_app_id
    wire.send(MANAGER, 2, struct.pack("<I", HOTKEY_B))  # create_hotkey
    wire.send(MANAGER, 2, struct.pack("<I", HOTKEY_A))

    wire.send(HOTKEY_B, 1, string("Test hotkey"))  # set_description
    wire.send(HOTKEY_B, 3, struct.pack("<II", KEY_B, SUPER | SHIFT))  # set_key_trigger
    wire.send(HOTKEY_B, 5)  # commit
    wire.send(HOTKEY_A, 3, struct.pack("<II", KEY_A, 0))
    wire.send(HOTKEY_A, 5)

    print("bind Super+Shift+B and a plain `a`; press Super+Shift+B, Ctrl+C to quit")
    while True:
        obj, opcode, body = wire.event()
        who = {HOTKEY_B: "Super+Shift+B", HOTKEY_A: "plain a"}.get(obj)
        if obj == DISPLAY and opcode == 0:
            sys.exit("protocol error: %r" % (read_string(body, 8)[0],))
        if who is None:
            continue
        if opcode == 0:
            print("%s: bound" % who)
            if obj == HOTKEY_A:
                sys.exit("FAIL: a plain `a` must not be bound")
        elif opcode == 1:
            (reason,) = struct.unpack_from("<I", body)
            message, _ = read_string(body, 4)
            print("%s: denied(%s, %r)" % (who, DENY.get(reason, reason), message))
            if obj == HOTKEY_A:
                print("OK" if reason == 1 else "FAIL: expected not_permitted")
        elif opcode == 2:
            print("%s: revoked" % who)
        elif opcode in (3, 4):
            serial, time = struct.unpack_from("<II", body)
            print(
                "%s: %s serial=%d time=%d"
                % (who, "triggered" if opcode == 3 else "released", serial, time)
            )


if __name__ == "__main__":
    main()
