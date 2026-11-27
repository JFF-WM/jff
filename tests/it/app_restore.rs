//! Tests for the list kept by `restore-apps`: `jff_wm::primitives::app_restore`.

use jff_wm::primitives::app_restore::{
    app_ids, format_apps, is_autostarted, parse_apps, shell_line_command, sorted, Placement,
};

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn place(app_id: &str, workspace: Option<usize>, output: &str) -> Placement {
    Placement {
        app_id: app_id.to_owned(),
        workspace,
        output: output.to_owned(),
        pid: None,
        proc_id: None,
        title: String::new(),
        container: String::new(),
        slot: None,
        layout: String::new(),
        maximized: false,
    }
}

#[test]
fn process_identity_round_trips() {
    let mut p = place("foot", Some(2), "DP-1");
    p.pid = Some(4242);
    p.proc_id = Some(9_000_000_001);
    assert_eq!(parse_apps(&format_apps(&[p.clone()])), vec![p]);
}

#[test]
fn partial_identity_round_trips() {
    let mut a = place("foot", Some(0), "DP-1");
    a.pid = Some(7);
    let mut b = place("kitty", Some(1), "DP-1");
    b.proc_id = Some(9);
    let list = sorted([a, b]);
    assert_eq!(parse_apps(&format_apps(&list)), list);
}

#[test]
fn maximized_lines_survive_a_round_trip() {
    let mut plain = place("firefox", Some(1), "DP-1");
    plain.maximized = true;
    let mut full = place("firefox", Some(2), "DP-1");
    full.pid = Some(10);
    full.proc_id = Some(20);
    full.title = "Mozilla Firefox".to_owned();
    full.container = "web".to_owned();
    full.slot = Some(1);
    full.layout = "h(a,b)".to_owned();
    full.maximized = true;
    let list = sorted([plain, full]);
    assert_eq!(parse_apps(&format_apps(&list)), list);
}

#[test]
fn identity_from_another_boot_is_dropped() {
    use jff_wm::primitives::app_restore::without_stale_identity;
    let mut p = place("foot", Some(0), "DP-1");
    p.pid = Some(5);
    p.proc_id = Some(6);
    assert_eq!(
        without_stale_identity(vec![p.clone()], true),
        vec![p.clone()]
    );
    let cleared = without_stale_identity(vec![p], false);
    assert_eq!((cleared[0].pid, cleared[0].proc_id), (None, None));
}

#[test]
fn old_three_field_lines_still_parse() {
    let list = parse_apps("foot\t1\tDP-1\n");
    assert_eq!(list, vec![place("foot", Some(1), "DP-1")]);
}

#[test]
fn one_living_process_keeps_the_app_from_starting_again() {
    use jff_wm::primitives::app_restore::{app_survived, ProcessState::*};
    assert!(app_survived([Dead, Alive, Unknown]));
    assert!(!app_survived([Dead, Unknown]));
    assert!(!app_survived([]));
}

#[test]
fn places_are_sorted_trimmed_and_blank_ids_dropped() {
    let list = sorted([
        place("foot", Some(1), "DP-1"),
        place(" firefox ", Some(0), "DP-2"),
        place("", Some(0), "DP-1"),
        place("   ", None, ""),
        place("foot", Some(0), "DP-1"),
    ]);
    assert_eq!(
        list,
        vec![
            place("firefox", Some(0), "DP-2"),
            place("foot", Some(0), "DP-1"),
            place("foot", Some(1), "DP-1"),
        ]
    );
}

#[test]
fn windows_of_one_app_in_one_place_are_all_kept() {
    let list = sorted([
        place("foot", Some(2), "DP-1"),
        place("foot", Some(2), "DP-1"),
    ]);
    assert_eq!(list.len(), 2);
    assert_eq!(app_ids(&list), argv(&["foot"]));
}

#[test]
fn the_list_round_trips() {
    let list = sorted([
        place("firefox", Some(0), "DP-2"),
        place("org.gnome.Nautilus", Some(4), "HDMI-A-1"),
        place("odd\\id", Some(1), "weird\tname"),
        place("two\nlines", None, ""),
    ]);
    let text = format_apps(&list);
    // One line per window, whatever the ids and names hold.
    assert_eq!(text.lines().count(), list.len());
    assert_eq!(parse_apps(&text), list);
}

#[test]
fn the_text_is_the_same_for_the_same_places() {
    let a = format_apps(&sorted([
        place("foot", Some(1), "DP-1"),
        place("firefox", Some(0), "DP-2"),
    ]));
    let b = format_apps(&sorted([
        place("firefox", Some(0), "DP-2"),
        place("foot", Some(1), "DP-1"),
    ]));
    assert_eq!(a, b);
    // The pid and process id columns are always there, empty when unknown.
    assert_eq!(a, "firefox\t0\tDP-2\t\t\nfoot\t1\tDP-1\t\t\n");
}

#[test]
fn a_line_with_only_an_app_id_has_no_place() {
    assert_eq!(parse_apps("foot\n"), vec![place("foot", None, "")]);
    assert_eq!(parse_apps("foot\t3\n"), vec![place("foot", Some(3), "")]);
}

#[test]
fn bad_lines_are_skipped() {
    // The last line has eleven fields, one more than the format has.
    let list = parse_apps(
        "foot\tx\tDP-1\n\n\tnot an id? \nfirefox\t2\tDP-1\ntoo\t1\tmany\t1\t2\ta\tb\t3\tc\ttrue\textra\n",
    );
    assert_eq!(list, vec![place("firefox", Some(2), "DP-1")]);
    assert!(parse_apps("").is_empty());
}

#[test]
fn an_autostarted_program_is_not_restored() {
    let autostarted = vec![argv(&["waybar"]), argv(&["/usr/bin/foot", "--server"])];
    assert!(is_autostarted(&argv(&["foot"]), &autostarted));
    assert!(is_autostarted(
        &argv(&["/opt/waybar/bin/waybar"]),
        &autostarted
    ));
    assert!(!is_autostarted(&argv(&["firefox"]), &autostarted));
    assert!(!is_autostarted(&[], &autostarted));
    assert!(!is_autostarted(&argv(&["foot"]), &[]));
}

#[test]
fn wrappers_must_match_whole() {
    let autostarted = vec![argv(&["flatpak", "run", "org.mozilla.firefox"])];
    assert!(is_autostarted(
        &argv(&["flatpak", "run", "org.mozilla.firefox"]),
        &autostarted
    ));
    assert!(!is_autostarted(
        &argv(&["flatpak", "run", "org.gnome.Nautilus"]),
        &autostarted
    ));
}

#[test]
fn a_shell_line_gives_its_first_command() {
    assert_eq!(
        shell_line_command("foot --server && sleep 1"),
        argv(&["foot", "--server"])
    );
    assert_eq!(shell_line_command("sleep 2 ; foot"), argv(&["sleep", "2"]));
    assert!(shell_line_command("   ").is_empty());
}
