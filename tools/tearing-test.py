"""Manual test client for wp_tearing_control_v1. Plain Python 3, speaks the wire protocol itself.

Opens a fullscreen window that asks for tearing (wp_tearing_control_v1.set_presentation_hint
async, plus wp_content_type game) and animates a white bar that jumps across the screen every
frame. A torn frame shows as the bar broken into two offset pieces at a horizontal line.

Run it inside the jff session (WAYLAND_DISPLAY set), with `allow-tearing` on in config.kdl:

    python3 tools/tearing-test.py                 # dmabuf buffers (can be scanned out)
    python3 tools/tearing-test.py --uncapped      # commit as fast as buffers are released
    python3 tools/tearing-test.py --shm           # shm buffers (expect NO direct scanout)
    python3 tools/tearing-test.py --vsync         # same, but hint vsync (A/B comparison)

While it runs, in another terminal / TTY:

    jff msg stats

Expect, for a working setup:

    client_asked.tearing       true
    client_asked.buffer        "dmabuf"
    compositor.path            a scanout path, not "composited"
    hardware.async_flip        true
    hardware.tearing_observed  non-null

With --shm the compositor can't hand the buffer to a plane, so `path` stays "composited" and
nothing tears, which is the case in the original bug report (Doom: buffer "shm").

dmabuf buffers are allocated with libgbm through ctypes (no pip packages needed) on a DRM
render node (--render-node, default /dev/dri/renderD128), using the XRGB8888 modifiers the
compositor advertises through zwp_linux_dmabuf_v1 (LINEAR preferred, so the CPU can fill them).
The window closes itself after --seconds (default 30; 0 = never) since it is fullscreen and
takes no input.
"""

import argparse
import array
import ctypes
import ctypes.util
import mmap
import os
import select
import socket
import struct
import sys
import time

DISPLAY, REGISTRY = 1, 2
DRM_FORMAT_XRGB8888 = 0x34325258
WL_SHM_XRGB8888 = 1
MOD_LINEAR = 0
MOD_INVALID = 0x00FFFFFFFFFFFFFF
GBM_BO_USE_SCANOUT = 1 << 0
GBM_BO_USE_RENDERING = 1 << 2
GBM_BO_TRANSFER_WRITE = 1 << 1

BAR_W = 160
BUFFERS = 8
BACKGROUND = b"\x30\x20\x20\x00"
BAR = b"\xff\xff\xff\x00"


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
    sys.exit("tearing-test: " + message)


class Gbm:
    """The little bit of libgbm this needs."""

    def __init__(self, node):
        name = ctypes.util.find_library("gbm") or "libgbm.so.1"
        try:
            self.lib = ctypes.CDLL(name)
        except OSError as err:
            die("can't load libgbm (%s); use --shm to skip dmabuf" % err)
        lib = self.lib
        u, vp = ctypes.c_uint32, ctypes.c_void_p
        lib.gbm_create_device.restype = vp
        lib.gbm_create_device.argtypes = [ctypes.c_int]
        lib.gbm_bo_create.restype = vp
        lib.gbm_bo_create.argtypes = [vp, u, u, u, u]
        lib.gbm_bo_create_with_modifiers2.restype = vp
        lib.gbm_bo_create_with_modifiers2.argtypes = [
            vp,
            u,
            u,
            u,
            ctypes.POINTER(ctypes.c_uint64),
            u,
            u,
        ]
        lib.gbm_bo_get_stride.restype = u
        lib.gbm_bo_get_stride.argtypes = [vp]
        lib.gbm_bo_get_offset.restype = u
        lib.gbm_bo_get_offset.argtypes = [vp, ctypes.c_int]
        lib.gbm_bo_get_modifier.restype = ctypes.c_uint64
        lib.gbm_bo_get_modifier.argtypes = [vp]
        lib.gbm_bo_get_fd.restype = ctypes.c_int
        lib.gbm_bo_get_fd.argtypes = [vp]
        lib.gbm_bo_map.restype = vp
        lib.gbm_bo_map.argtypes = [
            vp,
            u,
            u,
            u,
            u,
            u,
            ctypes.POINTER(u),
            ctypes.POINTER(vp),
        ]
        lib.gbm_bo_unmap.restype = None
        lib.gbm_bo_unmap.argtypes = [vp, vp]
        try:
            self.fd = os.open(node, os.O_RDWR | os.O_CLOEXEC)
        except OSError as err:
            die("can't open %s: %s (try --render-node)" % (node, err))
        self.dev = lib.gbm_create_device(self.fd)
        if not self.dev:
            die("gbm_create_device failed on %s" % node)

    def create(self, width, height, modifiers):
        flags = GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING
        bo = None
        if modifiers:
            arr = (ctypes.c_uint64 * len(modifiers))(*modifiers)
            bo = self.lib.gbm_bo_create_with_modifiers2(
                self.dev, width, height, DRM_FORMAT_XRGB8888, arr, len(modifiers), flags
            )
        if not bo:
            bo = self.lib.gbm_bo_create(
                self.dev, width, height, DRM_FORMAT_XRGB8888, flags
            )
        if not bo:
            die("gbm_bo_create failed for %dx%d XRGB8888" % (width, height))
        return bo

    def fill(self, bo, width, height, make_frame):
        stride = ctypes.c_uint32()
        data = ctypes.c_void_p()
        ptr = self.lib.gbm_bo_map(
            bo,
            0,
            0,
            width,
            height,
            GBM_BO_TRANSFER_WRITE,
            ctypes.byref(stride),
            ctypes.byref(data),
        )
        if not ptr:
            die("gbm_bo_map failed (tiled modifier? the compositor must offer LINEAR)")
        frame = make_frame(stride.value)
        ctypes.memmove(ptr, frame, len(frame))
        self.lib.gbm_bo_unmap(bo, data)


class Wire:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
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
        self.cb_done = set()
        self.frame_cb = None
        self.frame_ready = True
        self.modifiers = {}
        self.size = None
        self.configured = False
        self.buffers = []
        self.by_id = {}
        self.order = []
        self.cursor = 0
        self.commits = 0

    def alloc(self):
        i = self.next_id
        self.next_id += 1
        return i

    def send(self, *a, **kw):
        self.wire.send(*a, **kw)

    def handle(self, obj, opcode, body):
        if obj == DISPLAY and opcode == 0:
            (target, code) = struct.unpack_from("<II", body)
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
            self.cb_done.add(obj)
            if obj == self.frame_cb:
                self.frame_ready = True
        elif obj == getattr(self, "wm_base", None) and opcode == 0:
            self.send(self.wm_base, 3, body[:4])
        elif obj == getattr(self, "xdg_surface", None) and opcode == 0:
            self.send(self.xdg_surface, 4, body[:4])
            self.configured = True
        elif obj == getattr(self, "toplevel", None):
            if opcode == 0:
                width, height = struct.unpack_from("<ii", body)
                if width > 0 and height > 0:
                    if self.size and self.size != (width, height):
                        print(
                            "size changed to %dx%d, ignoring (buffers keep %dx%d)"
                            % (width, height, *self.size)
                        )
                    else:
                        self.size = (width, height)
            elif opcode == 1:
                print("closed by the compositor")
                sys.exit(0)
        elif obj == getattr(self, "dmabuf", None) and opcode == 1:
            fmt, hi, lo = struct.unpack_from("<III", body)
            self.modifiers.setdefault(fmt, []).append((hi << 32) | lo)
        elif obj in self.by_id and opcode == 0:
            self.by_id[obj]["busy"] = False

    callbacks = ()

    def pump(self, timeout):
        ready, _, _ = select.select([self.wire], [], [], timeout)
        if not ready:
            return
        self.wire.read()
        for obj, opcode, body in self.wire.events():
            self.handle(obj, opcode, body)

    def roundtrip(self):
        cb = self.alloc()
        self.callbacks = set(self.callbacks) | {cb}
        self.send(DISPLAY, 0, u32(cb))
        while cb not in self.cb_done:
            self.pump(None)

    def bind(self, interface, version):
        if interface not in self.globals:
            return None
        name, advertised = self.globals[interface]
        version = min(version, advertised)
        obj = self.alloc()
        self.send(REGISTRY, 0, u32(name) + string(interface) + u32(version) + u32(obj))
        return obj

    def setup(self):
        args = self.args
        self.send(DISPLAY, 1, u32(REGISTRY))
        self.roundtrip()

        compositor = self.bind("wl_compositor", 4)
        self.wm_base = self.bind("xdg_wm_base", 1)
        tearing_mgr = self.bind("wp_tearing_control_manager_v1", 1)
        content_mgr = self.bind("wp_content_type_manager_v1", 1)
        shm = self.bind("wl_shm", 1)
        self.dmabuf = None if args.shm else self.bind("zwp_linux_dmabuf_v1", 3)
        if not compositor or not self.wm_base:
            die("wl_compositor / xdg_wm_base missing")
        if not tearing_mgr and not args.no_hint:
            print(
                "WARNING: wp_tearing_control_manager_v1 is not advertised by the compositor"
            )
        if not args.shm and not self.dmabuf:
            die("zwp_linux_dmabuf_v1 is not advertised; use --shm")
        if args.shm and not shm:
            die("wl_shm is not advertised")
        self.roundtrip()

        self.surface = self.alloc()
        self.send(compositor, 0, u32(self.surface))
        self.xdg_surface = self.alloc()
        self.send(self.wm_base, 2, u32(self.xdg_surface, self.surface))
        self.toplevel = self.alloc()
        self.send(self.xdg_surface, 1, u32(self.toplevel))
        self.send(self.toplevel, 2, string("tearing-test"))
        self.send(self.toplevel, 3, string("tearing-test"))
        self.send(self.toplevel, 11, u32(0))

        if tearing_mgr and not args.no_hint:
            hint = self.alloc()
            self.send(tearing_mgr, 1, u32(hint, self.surface))
            self.send(hint, 0, u32(0 if args.vsync else 1))
            print("hint: %s" % ("vsync" if args.vsync else "async (tearing)"))
        if content_mgr:
            content = self.alloc()
            self.send(content_mgr, 1, u32(content, self.surface))
            self.send(content, 1, u32(3))

        self.send(self.surface, 6)
        while not self.configured:
            self.pump(None)
        self.size = self.size or (args.width, args.height)
        print("window: %dx%d fullscreen" % self.size)

        if args.shm:
            self.make_shm_buffers(shm)
        else:
            self.make_dmabuf_buffers()
        n = len(self.buffers)
        self.order = list(range(n)) + list(range(n - 2, 0, -1))

    def frame_bytes(self, stride, index):
        width, height = self.size
        travel = max(width - BAR_W, 1)
        x = index * travel // max(BUFFERS - 1, 1)
        row = bytearray(BACKGROUND * width)
        row[x * 4 : (x + BAR_W) * 4] = BAR * BAR_W
        row += bytes(stride - len(row))
        return bytes(row) * height

    def make_shm_buffers(self, shm):
        width, height = self.size
        stride = width * 4
        size = stride * height
        fd = os.memfd_create("tearing-test", os.MFD_CLOEXEC)
        os.ftruncate(fd, size * BUFFERS)
        mem = mmap.mmap(fd, size * BUFFERS)
        pool = self.alloc()
        self.send(shm, 0, u32(pool) + u32(size * BUFFERS), fds=[fd])
        for i in range(BUFFERS):
            mem[i * size : (i + 1) * size] = self.frame_bytes(stride, i)
            buf = self.alloc()
            self.send(
                pool,
                0,
                u32(buf, i * size) + i32(width, height, stride) + u32(WL_SHM_XRGB8888),
            )
            self.add_buffer(buf)
        self.send(pool, 1)
        os.close(fd)
        self.mem = mem
        print("buffers: %d x shm" % BUFFERS)

    def make_dmabuf_buffers(self):
        width, height = self.size
        gbm = Gbm(self.args.render_node)
        offered = self.modifiers.get(DRM_FORMAT_XRGB8888, [])
        if offered and not self.args.any_modifier and MOD_LINEAR in offered:
            mods = [MOD_LINEAR]
        else:
            mods = [m for m in offered if m != MOD_INVALID]
        for i in range(BUFFERS):
            bo = gbm.create(width, height, mods)
            gbm.fill(bo, width, height, lambda stride, i=i: self.frame_bytes(stride, i))
            fd = gbm.lib.gbm_bo_get_fd(bo)
            stride = gbm.lib.gbm_bo_get_stride(bo)
            offset = gbm.lib.gbm_bo_get_offset(bo, 0)
            modifier = gbm.lib.gbm_bo_get_modifier(bo)
            params = self.alloc()
            self.send(self.dmabuf, 1, u32(params))
            self.send(
                params,
                1,
                u32(0, offset, stride, modifier >> 32, modifier & 0xFFFFFFFF),
                fds=[fd],
            )
            buf = self.alloc()
            self.send(params, 3, u32(buf, width, height, DRM_FORMAT_XRGB8888, 0))
            self.send(params, 0)
            os.close(fd)
            self.add_buffer(buf)
            if i == 0:
                print(
                    "buffers: %d x dmabuf, modifier 0x%x, stride %d"
                    % (BUFFERS, modifier, stride)
                )

    def add_buffer(self, buf):
        entry = {"id": buf, "busy": False}
        self.buffers.append(entry)
        self.by_id[buf] = entry

    def can_draw(self):
        if not self.args.uncapped and not self.frame_ready:
            return False
        return not self.buffers[self.order[self.cursor]]["busy"]

    def draw(self):
        entry = self.buffers[self.order[self.cursor]]
        self.cursor = (self.cursor + 1) % len(self.order)
        width, height = self.size
        self.send(self.surface, 1, u32(entry["id"]) + i32(0, 0))
        self.send(self.surface, 9, i32(0, 0, width, height))
        if not self.args.uncapped:
            self.frame_cb = self.alloc()
            self.callbacks = set(self.callbacks) | {self.frame_cb}
            self.send(self.surface, 3, u32(self.frame_cb))
            self.frame_ready = False
        self.send(self.surface, 6)
        entry["busy"] = True
        self.commits += 1

    def run(self):
        print("running; check `jff msg stats` now (Ctrl-C or --seconds to stop)")
        start = last = time.monotonic()
        last_commits = 0
        while True:
            now = time.monotonic()
            if self.args.seconds and now - start >= self.args.seconds:
                print("done")
                return
            if now - last >= 1.0:
                print(
                    "client commits: %.1f/s"
                    % ((self.commits - last_commits) / (now - last))
                )
                last, last_commits = now, self.commits
            if self.can_draw():
                self.draw()
                self.pump(0)
            else:
                self.pump(0.25)


def main():
    parser = argparse.ArgumentParser(
        description="Fullscreen window that asks for tearing."
    )
    parser.add_argument(
        "--shm", action="store_true", help="use wl_shm buffers (can't be scanned out)"
    )
    parser.add_argument(
        "--vsync", action="store_true", help="send the vsync hint instead of async"
    )
    parser.add_argument(
        "--no-hint", action="store_true", help="don't bind wp_tearing_control at all"
    )
    parser.add_argument(
        "--uncapped",
        action="store_true",
        help="don't wait for frame callbacks; commit whenever a buffer is free",
    )
    parser.add_argument(
        "--render-node",
        default="/dev/dri/renderD128",
        help="DRM render node used to allocate dmabufs",
    )
    parser.add_argument(
        "--any-modifier",
        action="store_true",
        help="allow any advertised modifier, not just LINEAR",
    )
    parser.add_argument(
        "--seconds", type=float, default=30, help="exit after this long (0 = never)"
    )
    parser.add_argument(
        "--width",
        type=int,
        default=1920,
        help="fallback if the compositor sends no size",
    )
    parser.add_argument("--height", type=int, default=1080)
    args = parser.parse_args()

    client = Client(args)
    client.setup()
    try:
        client.run()
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
