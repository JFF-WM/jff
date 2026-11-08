//! Tests for `jff_wm::desk`.

use jff_wm::desk::{resolve, Desk, Goto, Side, WindowMode};

fn order(d: &Desk<&'static str>) -> Vec<&'static str> {
    d.top_down().map(|e| e.window).collect()
}

#[test]
fn new_windows_go_on_top_and_cascade() {
    let mut d = Desk::default();
    let a = d.add("a");
    let b = d.add("b");
    assert_eq!(order(&d), ["b", "a"]);
    assert_ne!(a, b);
}

#[test]
fn raise_reorders() {
    let mut d = Desk::default();
    d.add("a");
    d.add("b");
    d.add("c");
    d.raise_where(|w| *w == "a");
    assert_eq!(order(&d), ["a", "c", "b"]);
}

#[test]
fn set_pos_moves_only_the_match() {
    let mut d = Desk::default();
    let a = d.add("a");
    d.add("b");
    d.set_pos_where(|w| *w == "b", (500, 300));
    assert_eq!(d.pos_of(|w| *w == "b"), Some((500, 300)));
    assert_eq!(d.pos_of(|w| *w == "a"), Some(a));
    assert_eq!(d.pos_of(|w| *w == "zzz"), None);
}

#[test]
fn maximize_then_float_round_trips() {
    let mut d = Desk::default();
    let a = d.add("a");
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (300, 200)));
    assert!(d.is_maximized(|w| *w == "a"));
    assert_eq!(d.pos_of(|w| *w == "a"), Some((0, 0)));
    assert!(d.set_mode_where(
        |w| *w == "a",
        WindowMode::Snapped(Side::Left),
        (0, 0),
        (999, 999)
    ));
    assert!(!d.is_maximized(|w| *w == "a"));
    assert_eq!(d.float_where(|w| *w == "a"), Some((300, 200)));
    assert_eq!(d.pos_of(|w| *w == "a"), Some(a));
    assert!(d.is_floating(|w| *w == "a"));
    assert_eq!(d.float_where(|w| *w == "a"), None);
}

#[test]
fn floating_is_not_a_mode_to_set() {
    let mut d = Desk::default();
    d.add("a");
    assert!(!d.set_mode_where(|w| *w == "a", WindowMode::Floating, (0, 0), (1, 1)));
    assert!(!d.set_mode_where(|w| *w == "zzz", WindowMode::Maximized, (0, 0), (1, 1)));
}

#[test]
fn fullscreen_goes_back_to_the_mode_before_it() {
    let mut d = Desk::default();
    d.add("a");
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (300, 200)));
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Fullscreen, (0, 0), (900, 900)));
    assert!(!d.is_maximized(|w| *w == "a"));
    assert_eq!(
        d.mode_after_fullscreen(|w| *w == "a"),
        WindowMode::Maximized
    );
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (900, 900)));
    assert_eq!(d.float_where(|w| *w == "a"), Some((300, 200)));
    assert_eq!(d.mode_after_fullscreen(|w| *w == "a"), WindowMode::Floating);
}

#[test]
fn floating_in_place_keeps_position_and_hands_back_the_old_geometry() {
    let mut d = Desk::default();
    let a = d.add("a");
    assert_eq!(d.float_in_place_where(|w| *w == "a"), None);
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (300, 200)));
    d.set_pos_where(|w| *w == "a", (700, 400));
    assert_eq!(d.float_in_place_where(|w| *w == "a"), Some((a, (300, 200))));
    assert!(d.is_floating(|w| *w == "a"));
    assert_eq!(d.pos_of(|w| *w == "a"), Some((700, 400)));
}

#[test]
fn restore_changes_only_while_not_floating() {
    let mut d = Desk::default();
    d.add("a");
    d.set_restore_pos_where(|w| *w == "a", (5, 5));
    d.set_restore_where(|w| *w == "a", ((1, 1), (2, 2)));
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (300, 200)));
    d.set_restore_pos_where(|w| *w == "a", (2000, 70));
    assert_eq!(d.float_where(|w| *w == "a"), Some((300, 200)));
    assert_eq!(d.pos_of(|w| *w == "a"), Some((2000, 70)));
    assert!(d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (300, 200)));
    d.set_restore_where(|w| *w == "a", ((9, 9), (50, 60)));
    assert_eq!(d.float_where(|w| *w == "a"), Some((50, 60)));
    assert_eq!(d.pos_of(|w| *w == "a"), Some((9, 9)));
}

#[test]
fn fitted_fires_once() {
    let mut d = Desk::default();
    d.add("a");
    assert!(d.mark_fitted_where(|w| *w == "a"));
    assert!(!d.mark_fitted_where(|w| *w == "a"));
    assert!(!d.mark_fitted_where(|w| *w == "nope"));
}

#[test]
fn only_the_current_workspace_is_visible() {
    let mut d = Desk::default();
    d.add("a");
    d.add("b");
    d.set_current(1);
    // New windows land on the workspace being shown, and cascade from its start.
    let c = d.add("c");
    assert_eq!(c, (40, 40));
    assert_eq!(order(&d), ["c"]);
    assert_eq!(d.all_top_down().count(), 3);
    d.set_current(0);
    assert_eq!(order(&d), ["b", "a"]);
    assert_eq!(d.workspace_of(|w| *w == "c"), Some(1));
    assert_eq!(d.workspace_of(|w| *w == "zzz"), None);
    assert_eq!(d.count_on(0), 2);
}

#[test]
fn moving_a_window_changes_what_is_visible() {
    let mut d = Desk::default();
    d.add("a");
    d.add("b");
    assert!(d.move_to_workspace(|w| *w == "b", 2));
    assert_eq!(order(&d), ["a"]);
    d.set_current(2);
    assert_eq!(order(&d), ["b"]);
    assert!(!d.move_to_workspace(|w| *w == "zzz", 0));
    // Raising keeps workspaces apart.
    d.raise_where(|w| *w == "a");
    assert_eq!(order(&d), ["b"]);
}

#[test]
fn fewer_workspaces_fold_into_the_last() {
    let mut d = Desk::default();
    d.add("a");
    d.set_current(5);
    d.add("b");
    d.clamp_workspaces(3);
    assert_eq!(d.current(), 2);
    assert_eq!(order(&d), ["b"]);
    assert_eq!(d.workspace_of(|w| *w == "a"), Some(0));
    d.clamp_workspaces(0);
    assert_eq!(d.current(), 0);
}

#[test]
fn goto_wraps_and_clamps() {
    assert_eq!(resolve(Goto::To(2), 0, 9), 2);
    assert_eq!(resolve(Goto::To(20), 0, 9), 8);
    assert_eq!(resolve(Goto::Next, 8, 9), 0);
    assert_eq!(resolve(Goto::Prev, 0, 9), 8);
    assert_eq!(resolve(Goto::Next, 0, 1), 0);
    assert_eq!(resolve(Goto::Prev, 3, 0), 0);
}

#[test]
fn remove_drops_window() {
    let mut d = Desk::default();
    d.add("a");
    d.add("b");
    d.remove_where(|w| *w == "a");
    assert_eq!(order(&d), ["b"]);
    assert_eq!(d.len(), 1);
}

fn per_output_desk() -> Desk<&'static str> {
    let mut d = Desk::default();
    d.set_per_output(true, "");
    d
}

#[test]
fn shared_workspaces_ignore_the_monitor() {
    let mut d = Desk::default();
    d.add_on("a", "left");
    d.add_on("b", "right");
    d.set_current_on("left", 1);
    // One workspace for everyone: both monitors moved, nothing is visible any more.
    assert_eq!(d.current_on("right"), 1);
    assert_eq!(order(&d), Vec::<&str>::new());
    assert!(!d.per_output());
}

#[test]
fn each_monitor_shows_its_own_workspace() {
    let mut d = per_output_desk();
    d.add_on("a", "left");
    d.add_on("b", "right");
    d.set_current_on("left", 1);
    assert_eq!(d.current_on("left"), 1);
    assert_eq!(d.current_on("right"), 0);
    // Switching the left monitor hides only its windows.
    assert_eq!(order(&d), ["b"]);
    // New windows land on the workspace their monitor shows.
    d.add_on("c", "left");
    assert_eq!(d.workspace_of(|w| *w == "c"), Some(1));
    assert_eq!(order(&d), ["c", "b"]);
    assert_eq!(d.home_of(|w| *w == "c"), Some(("left".to_owned(), 1)));
}

#[test]
fn cascade_counts_only_the_monitor() {
    let mut d = per_output_desk();
    let first = d.add_on("a", "left");
    let other = d.add_on("b", "right");
    assert_eq!(first, other);
    assert_ne!(first, d.add_on("c", "left"));
}

#[test]
fn a_shown_window_that_changes_monitor_follows_its_workspace() {
    let mut d = per_output_desk();
    d.add_on("a", "left");
    d.set_current_on("right", 3);
    d.assign_outputs(|_| Some("right".to_owned()));
    assert_eq!(d.home_of(|w| *w == "a"), Some(("right".to_owned(), 3)));
    assert_eq!(order(&d), ["a"]);
}

#[test]
fn a_hidden_window_that_changes_monitor_keeps_its_workspace() {
    let mut d = per_output_desk();
    d.add_on("a", "left");
    d.set_current_on("left", 2);
    // "a" is on workspace 0 of the left monitor, which shows 2: hidden.
    d.assign_outputs(|_| Some("right".to_owned()));
    assert_eq!(d.home_of(|w| *w == "a"), Some(("right".to_owned(), 0)));
    // The right monitor shows 0, so it turns up there.
    assert_eq!(order(&d), ["a"]);
    // A monitor that is gone is remembered: "b" was shown there, so it lands on the workspace
    // the left monitor shows.
    let mut d = per_output_desk();
    d.set_current_on("gone", 2);
    d.add_on("b", "gone");
    d.assign_outputs(|_| Some("left".to_owned()));
    assert_eq!(d.workspace_of(|w| *w == "b"), Some(0));
}

#[test]
fn assigning_outputs_does_nothing_for_shared_workspaces() {
    let mut d = Desk::default();
    d.add_on("a", "left");
    d.assign_outputs(|_| Some("right".to_owned()));
    assert_eq!(d.home_of(|w| *w == "a"), Some(("left".to_owned(), 0)));
}

#[test]
fn assigning_skips_windows_without_an_answer() {
    let mut d = per_output_desk();
    d.add_on("a", "left");
    d.assign_outputs(|_| None);
    assert_eq!(d.home_of(|w| *w == "a"), Some(("left".to_owned(), 0)));
}

#[test]
fn turning_per_output_on_keeps_the_view_and_off_follows_the_active_monitor() {
    let mut d = Desk::default();
    d.add_on("a", "left");
    d.set_current(1);
    d.add_on("b", "right");
    d.set_per_output(true, "left");
    assert_eq!(d.current_on("left"), 1);
    assert_eq!(d.current_on("right"), 1);
    assert_eq!(order(&d), ["b"]);
    d.set_current_on("left", 0);
    // "a" is on the left monitor's workspace 0 now, which it shows.
    assert_eq!(order(&d), ["b", "a"]);
    d.set_per_output(false, "left");
    assert_eq!(d.current(), 0);
    assert_eq!(d.current_on("right"), 0);
}

#[test]
fn fewer_workspaces_fold_every_monitors_workspace() {
    let mut d = per_output_desk();
    d.set_current_on("left", 7);
    d.set_current_on("right", 4);
    d.clamp_workspaces(3);
    assert_eq!(d.current_on("left"), 2);
    assert_eq!(d.current_on("right"), 2);
}

#[test]
fn set_output_where_moves_a_window_to_the_workspace_that_monitor_shows() {
    let mut d = per_output_desk();
    d.set_current_on("DP-1", 0);
    d.set_current_on("HDMI-A-2", 3);
    d.add_on("a", "DP-1");
    assert!(d.set_output_where(|w| *w == "a", "HDMI-A-2"));
    let (output, workspace) = d.home_of(|w| *w == "a").unwrap();
    assert_eq!((output.as_str(), workspace), ("HDMI-A-2", 3));
    assert!(!d.set_output_where(|w| *w == "zzz", "DP-1"));
}

#[test]
fn set_output_where_keeps_the_workspace_when_shared() {
    let mut d = Desk::default();
    d.set_current(2);
    d.add("a");
    d.set_output_where(|w| *w == "a", "DP-1");
    assert_eq!(d.workspace_of(|w| *w == "a"), Some(2));
}
