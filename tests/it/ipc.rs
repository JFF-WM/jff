//! Tests for `jff_wm::ipc`.

use std::io;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;

use jff_wm::desk::Goto;
use jff_wm::ipc::events::{Group, GroupSet};
use jff_wm::ipc::{
    parse_request, split_style, ClickButton, Maximize, Outbox, Request, Subscriber, TagOp,
    COMMANDS, MAX_PENDING,
};
use jff_wm::primitives::json::{json_opt, json_str, pretty};

#[test]
fn requests_are_parsed() {
    assert_eq!(parse_request("views"), Ok(Request::Views));
    assert_eq!(parse_request("status"), Ok(Request::Status));
    assert_eq!(parse_request("outputs"), Ok(Request::Outputs));
    assert_eq!(parse_request("  focused \n"), Ok(Request::Focused));
    assert_eq!(parse_request("view-info"), Ok(Request::Info(None)));
    assert_eq!(parse_request("view-info 7"), Ok(Request::Info(Some(7))));
    assert_eq!(parse_request("close 3"), Ok(Request::Close(Some(3))));
    assert_eq!(
        parse_request("toggle-maximize"),
        Ok(Request::Maximize(None, Maximize::Toggle))
    );
    assert_eq!(
        parse_request("unmaximize 2"),
        Ok(Request::Maximize(Some(2), Maximize::Off))
    );
    assert_eq!(
        parse_request("fullscreen"),
        Ok(Request::Fullscreen(None, true))
    );
    assert_eq!(
        parse_request("fullscreen 5"),
        Ok(Request::Fullscreen(Some(5), true))
    );
    assert_eq!(
        parse_request("unfullscreen 5"),
        Ok(Request::Fullscreen(Some(5), false))
    );
    assert_eq!(parse_request("focus 4"), Ok(Request::Focus(4)));
    assert_eq!(parse_request("workspaces"), Ok(Request::Workspaces));
    assert_eq!(parse_request("containers"), Ok(Request::Containers));
    assert_eq!(parse_request(" containers \n"), Ok(Request::Containers));
    assert!(parse_request("containers 3").is_err());
    assert!(COMMANDS.contains("containers"));
    assert_eq!(
        parse_request("screenshot-area"),
        Ok(Request::ScreenshotArea)
    );
    assert_eq!(
        parse_request(" screenshot-fullscreen \n"),
        Ok(Request::ScreenshotFullscreen)
    );
    assert_eq!(
        parse_request("screenshot-window"),
        Ok(Request::ScreenshotWindow)
    );
    assert!(parse_request("screenshot-area now").is_err());
    assert_eq!(
        parse_request("workspace 3"),
        Ok(Request::Workspace(Goto::To(2)))
    );
    assert_eq!(
        parse_request("workspace next"),
        Ok(Request::Workspace(Goto::Next))
    );
    assert_eq!(
        parse_request("workspace prev"),
        Ok(Request::Workspace(Goto::Prev))
    );
    assert_eq!(
        parse_request("move-to-workspace 2"),
        Ok(Request::MoveToWorkspace(None, 2))
    );
    assert_eq!(
        parse_request("move-to-workspace 2 9"),
        Ok(Request::MoveToWorkspace(Some(9), 2))
    );
}

#[test]
fn watch_is_parsed() {
    assert_eq!(parse_request("watch"), Ok(Request::Watch(vec![])));
    assert_eq!(parse_request("  watch \n"), Ok(Request::Watch(vec![])));
    assert_eq!(
        parse_request("watch views workspaces"),
        Ok(Request::Watch(vec![Group::Views, Group::Workspaces]))
    );
    assert_eq!(
        parse_request("watch focus outputs modes"),
        Ok(Request::Watch(vec![
            Group::Focus,
            Group::Outputs,
            Group::Modes
        ]))
    );
    let err = parse_request("watch views panels").unwrap_err();
    assert!(err.contains("\"panels\""), "{err}");
    assert!(err.contains(Group::NAMES), "{err}");
    assert!(parse_request("views focus").is_err());
    assert!(parse_request("watch close").is_err());
    assert!(parse_request("watch 3").is_err());
    assert!(COMMANDS.contains("watch"));
}

#[test]
fn spawn_commands_are_parsed() {
    assert_eq!(
        parse_request("spawn firefox"),
        Ok(Request::Spawn(vec!["firefox".into()]))
    );
    assert_eq!(
        parse_request("spawn firefox --new-window https://example.com"),
        Ok(Request::Spawn(vec![
            "firefox".into(),
            "--new-window".into(),
            "https://example.com".into(),
        ]))
    );
    assert_eq!(
        parse_request("spawn-sh grim -g \"$(slurp)\" out.png"),
        Ok(Request::SpawnShell("grim -g \"$(slurp)\" out.png".into()))
    );
    assert_eq!(
        parse_request("spawn-sh echo 'a  b'"),
        Ok(Request::SpawnShell("echo 'a  b'".into()))
    );
    assert_eq!(
        parse_request("spawn-sh   swaylock"),
        Ok(Request::SpawnShell("swaylock".into()))
    );
    assert!(parse_request("spawn").is_err());
    assert!(parse_request("spawn-sh").is_err());
    assert!(parse_request("spawn-sh   ").is_err());
}

#[test]
fn bad_workspace_requests_are_refused() {
    assert!(parse_request("workspace").is_err());
    assert!(parse_request("workspace 0").is_err());
    assert!(parse_request("workspace x").is_err());
    assert!(parse_request("workspace 1 2").is_err());
    assert!(parse_request("workspaces 1").is_err());
    assert!(parse_request("move-to-workspace").is_err());
    assert!(parse_request("move-to-workspace 1 x").is_err());
}

#[test]
fn bad_requests_are_refused() {
    assert!(parse_request("").is_err());
    assert!(parse_request("fly").is_err());
    assert!(parse_request("close abc").is_err());
    assert!(parse_request("close 1 2").is_err());
    assert!(parse_request("focus").is_err());
    assert!(parse_request("views 1").is_err());
    assert!(parse_request("status 1").is_err());
    assert!(parse_request("fullscreen 1 2").is_err());
}

#[test]
fn json_strings_are_escaped() {
    assert_eq!(json_str("plain"), "\"plain\"");
    assert_eq!(json_str("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    assert_eq!(json_str("\u{1}"), "\"\\u0001\"");
    assert_eq!(json_str("héllo ✓"), "\"héllo ✓\"");
    assert_eq!(json_opt(None), "null");
}

struct FakeSocket {
    taken: Vec<u8>,
    take: usize,
    room: usize,
    error: Option<io::ErrorKind>,
    writes: usize,
}

impl FakeSocket {
    fn new(take: usize, room: usize) -> Self {
        Self {
            taken: Vec::new(),
            take,
            room,
            error: None,
            writes: 0,
        }
    }
}

impl Write for FakeSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if let Some(kind) = self.error {
            return Err(kind.into());
        }
        if self.room == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min(self.take).min(self.room);
        self.taken.extend_from_slice(&buf[..n]);
        self.room -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_roomy_socket_takes_everything_at_once() {
    let mut out = Outbox::new(1024);
    let mut socket = FakeSocket::new(usize::MAX, usize::MAX);
    assert!(out.enqueue(b"one\n"));
    assert!(out.enqueue(b"two\n"));
    out.flush(&mut socket).unwrap();
    assert_eq!(socket.taken, b"one\ntwo\n");
    assert!(out.pending.is_empty());
    let writes = socket.writes;
    out.flush(&mut socket).unwrap();
    assert_eq!(socket.writes, writes);
}

#[test]
fn a_full_socket_keeps_the_rest_queued_in_order() {
    let mut out = Outbox::new(1024);
    let mut socket = FakeSocket::new(3, 5);
    assert!(out.enqueue(b"abcdefghij"));
    out.flush(&mut socket).unwrap();
    assert_eq!(socket.taken, b"abcde");
    assert_eq!(out.pending.len(), 5);
    assert!(out.enqueue(b"KLM"));
    socket.room = 100;
    out.flush(&mut socket).unwrap();
    assert_eq!(socket.taken, b"abcdefghijKLM");
    assert!(out.pending.is_empty());
}

#[test]
fn the_queue_has_a_limit_and_refuses_whole_messages() {
    let mut out = Outbox::new(10);
    assert!(out.enqueue(b"123456"));
    assert!(!out.enqueue(b"78901"));
    assert_eq!(out.pending.len(), 6);
    assert!(out.enqueue(b"7890"));
    assert!(!out.enqueue(b"1"));
    assert!(!Outbox::new(0).enqueue(b"x"));
    assert!(Outbox::new(0).enqueue(b""));
    let mut socket = FakeSocket::new(4, 4);
    out.flush(&mut socket).unwrap();
    assert!(out.enqueue(b"abcd"));
}

#[test]
fn socket_errors_are_reported() {
    for kind in [
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::PermissionDenied,
    ] {
        let mut out = Outbox::new(1024);
        let mut socket = FakeSocket::new(usize::MAX, usize::MAX);
        socket.error = Some(kind);
        assert!(out.enqueue(b"x\n"));
        assert_eq!(out.flush(&mut socket).unwrap_err().kind(), kind);
    }
}

#[test]
fn interrupted_writes_are_retried_and_zero_writes_are_an_error() {
    struct Flaky(u8, Vec<u8>);
    impl Write for Flaky {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.0 > 0 {
                self.0 -= 1;
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.1.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut out = Outbox::new(64);
    let mut flaky = Flaky(2, Vec::new());
    assert!(out.enqueue(b"hello\n"));
    out.flush(&mut flaky).unwrap();
    assert_eq!(flaky.1, b"hello\n");

    struct Zero;
    impl Write for Zero {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(out.enqueue(b"x"));
    assert_eq!(
        out.flush(&mut Zero).unwrap_err().kind(),
        io::ErrorKind::WriteZero
    );
}

#[test]
fn a_slow_reader_is_dropped_when_events_pile_up() {
    let mut out = Outbox::new(MAX_PENDING);
    let mut socket = FakeSocket::new(usize::MAX, 0);
    let event = vec![b'x'; 1024];
    let mut queued = 0;
    while out.enqueue(&event) {
        out.flush(&mut socket).unwrap();
        queued += 1;
    }
    assert_eq!(queued, MAX_PENDING / 1024);
    assert!(socket.taken.is_empty());
}

#[test]
fn a_watcher_over_a_real_socket_pair() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();
    let mut watcher = Subscriber {
        id: 1,
        stream: ours,
        groups: GroupSet::ALL,
        out: Outbox::new(MAX_PENDING),
    };
    assert!(watcher.send(b"{\"event\":\"snapshot\"}\n"));
    assert!(watcher.service());
    let mut got = [0u8; 64];
    let n = theirs.read(&mut got).unwrap();
    assert_eq!(&got[..n], b"{\"event\":\"snapshot\"}\n");

    theirs.write_all(b"hello?\n").unwrap();
    assert!(watcher.service());

    theirs.shutdown(Shutdown::Write).unwrap();
    assert!(!watcher.service());
}

#[test]
fn a_dropped_watcher_hangs_up() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let polled = ours.try_clone().unwrap();
    let watcher = Subscriber {
        id: 1,
        stream: ours,
        groups: GroupSet::ALL,
        out: Outbox::new(MAX_PENDING),
    };
    drop(watcher);
    let mut buf = [0u8; 8];
    assert_eq!(theirs.read(&mut buf).unwrap(), 0, "the client sees EOF");
    drop(polled);
}

#[test]
fn a_dead_peer_is_noticed_on_write() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();
    let mut watcher = Subscriber {
        id: 1,
        stream: ours,
        groups: GroupSet::ALL,
        out: Outbox::new(MAX_PENDING),
    };
    drop(theirs);
    assert!(!watcher.send(b"a\n"));
}

#[test]
fn move_cursor_parses_two_numbers() {
    assert_eq!(
        parse_request("move-cursor 100 200"),
        Ok(Request::MoveCursor(100, 200))
    );
    assert_eq!(
        parse_request("move-cursor -50 3000"),
        Ok(Request::MoveCursor(-50, 3000))
    );
    assert!(parse_request("move-cursor").is_err());
    assert!(parse_request("move-cursor 1").is_err());
    assert!(parse_request("move-cursor 1 2 3").is_err());
    assert!(parse_request("move-cursor a b").is_err());
    assert!(parse_request("move-cursor 1.5 2").is_err());
}

#[test]
fn click_parses_left_right_and_middle_only() {
    assert_eq!(
        parse_request("click left"),
        Ok(Request::Click(ClickButton::Left))
    );
    assert_eq!(
        parse_request("click right"),
        Ok(Request::Click(ClickButton::Right))
    );
    assert_eq!(
        parse_request("click middle"),
        Ok(Request::Click(ClickButton::Middle))
    );
    assert!(parse_request("click").is_err());
    assert!(parse_request("click back").is_err());
    assert!(parse_request("click left right").is_err());
    assert!(parse_request("click 1").is_err());
}

#[test]
fn toggle_overview_takes_no_arguments() {
    assert_eq!(
        parse_request("toggle-overview"),
        Ok(Request::ToggleOverview)
    );
    assert!(parse_request("toggle-overview 1").is_err());
    assert!(parse_request("toggle-overview open").is_err());
}

#[test]
fn outputs_takes_no_arguments() {
    assert_eq!(parse_request(" outputs \n"), Ok(Request::Outputs));
    assert!(parse_request("outputs DP-1").is_err());
    assert!(COMMANDS.contains("outputs"));
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn pretty_puts_one_member_per_line() {
    assert_eq!(
        pretty("{\"ok\":true,\"views\":[],\"a\":{},\"b\":[1,{\"c\":null}]}"),
        "{\n  \"ok\": true,\n  \"views\": [],\n  \"a\": {},\n  \"b\": [\n    1,\n    {\n      \"c\": null\n    }\n  ]\n}"
    );
}

#[test]
fn pretty_leaves_strings_alone() {
    // Brackets, commas, colons, an escaped quote and an escaped backslash inside a string.
    let json = "{\"title\":\"a, {b}: [c] \\\" \\\\\",\"n\":1}";
    assert_eq!(
        pretty(json),
        "{\n  \"title\": \"a, {b}: [c] \\\" \\\\\",\n  \"n\": 1\n}"
    );
    // A string that ends in an escaped backslash does end there.
    assert_eq!(
        pretty("{\"a\":\"\\\\\",\"b\":2}"),
        "{\n  \"a\": \"\\\\\",\n  \"b\": 2\n}"
    );
}

#[test]
fn pretty_does_not_change_the_json() {
    let compact =
        "{\"event\":\"snapshot\",\"views\":[{\"id\":1,\"title\":\"x y\"}],\"locked\":false}";
    let again: String = {
        // Take every whitespace outside of strings out again.
        let mut out = String::new();
        let (mut in_string, mut escaped) = (false, false);
        for c in pretty(compact).chars() {
            if in_string {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
            } else if c == '"' {
                in_string = true;
                out.push(c);
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    };
    assert_eq!(again, compact);
}

#[test]
fn the_style_flags_come_before_the_command() {
    let args = words("views");
    assert_eq!(split_style(&args), (None, &args[..]));

    let args = words("--pretty views");
    assert_eq!(split_style(&args), (Some(true), &args[1..]));

    let args = words("-c status");
    assert_eq!(split_style(&args), (Some(false), &args[1..]));

    // The last one wins, and a flag after the command is the command's.
    let args = words("-p --compact outputs");
    assert_eq!(split_style(&args), (Some(false), &args[2..]));
    let args = words("spawn-sh ls -c");
    assert_eq!(split_style(&args), (None, &args[..]));

    let args = words("--pretty");
    assert!(split_style(&args).1.is_empty());
}

#[test]
fn tag_and_untag_are_parsed() {
    assert_eq!(parse_request("tag"), Ok(Request::Tag(None, TagOp::Show)));
    assert_eq!(
        parse_request("tag obs-dock"),
        Ok(Request::Tag(None, TagOp::Set("obs-dock".to_string())))
    );
    assert_eq!(
        parse_request("tag obs-dock 12"),
        Ok(Request::Tag(Some(12), TagOp::Set("obs-dock".to_string())))
    );
    assert_eq!(parse_request("untag"), Ok(Request::Tag(None, TagOp::Clear)));
    assert_eq!(
        parse_request("untag 4"),
        Ok(Request::Tag(Some(4), TagOp::Clear))
    );
    assert!(parse_request("tag a 1 2").is_err());
    assert!(parse_request("tag a b").is_err());
    assert!(COMMANDS.contains("tag"));
}

#[test]
fn container_layout_requests_are_parsed() {
    assert_eq!(
        parse_request("layout-save Dev"),
        Ok(Request::LayoutSave("Dev".into(), None))
    );
    assert_eq!(
        parse_request("layout-save Dev 7"),
        Ok(Request::LayoutSave("Dev".into(), Some(7)))
    );
    assert_eq!(
        parse_request("layout-apply Dev"),
        Ok(Request::LayoutApply("Dev".into(), None))
    );
    assert_eq!(
        parse_request("layout-apply Dev 7"),
        Ok(Request::LayoutApply("Dev".into(), Some(7)))
    );
    assert_eq!(parse_request("layout-list"), Ok(Request::LayoutList));
    assert_eq!(
        parse_request("layout-delete Dev"),
        Ok(Request::LayoutDelete("Dev".into()))
    );
    assert!(parse_request("layout-save").is_err());
    assert!(parse_request("layout-save Dev x").is_err());
    assert!(parse_request("layout-save Dev 7 8").is_err());
    assert!(parse_request("layout-apply").is_err());
    assert!(parse_request("layout-list 3").is_err());
    assert!(parse_request("layout-delete").is_err());
    assert!(parse_request("layout-delete a b").is_err());
    for command in [
        "layout-save",
        "layout-apply",
        "layout-list",
        "layout-delete",
    ] {
        assert!(COMMANDS.contains(command));
    }
}
