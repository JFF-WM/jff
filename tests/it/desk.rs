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
    assert_eq!(d.mode_after_fullscreen(|w| *w == "a"), WindowMode::Maximized);
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
