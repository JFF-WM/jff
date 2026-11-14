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
    }
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
    assert_eq!(a, "firefox\t0\tDP-2\nfoot\t1\tDP-1\n");
}

#[test]
fn a_line_with_only_an_app_id_has_no_place() {
    assert_eq!(parse_apps("foot\n"), vec![place("foot", None, "")]);
    assert_eq!(parse_apps("foot\t3\n"), vec![place("foot", Some(3), "")]);
}

#[test]
fn bad_lines_are_skipped() {
    let list =
        parse_apps("foot\tx\tDP-1\n\n\tnot an id? \nfirefox\t2\tDP-1\ntoo\t1\tmany\tfields\n");
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
