//! Tests for `remember-container-layouts`: what is remembered and where the windows go back.

use jff_wm::config::Config;
use jff_wm::primitives::layout_memory::{LayoutMemory, MAX_REMEMBERED};
use jff_wm::primitives::layout_template::Template;

/// One big window beside two stacked ones.
fn three_way() -> Template {
    Template::parse("h0.7(_,v0.3(_,_))").unwrap()
}

fn windows(list: &[(u32, &str)]) -> Vec<(u32, String)> {
    list.iter()
        .map(|(id, app)| (*id, (*app).to_owned()))
        .collect()
}

fn ids(recalled: &jff_wm::primitives::layout_memory::Recalled<'_, u32>) -> Vec<u32> {
    recalled.order.clone()
}

#[test]
fn the_same_windows_go_back_to_their_own_slots() {
    let mut memory = LayoutMemory::default();
    let arranged = windows(&[(1, "kitty"), (2, "kitty"), (3, "kitty")]);
    assert!(memory.remember(three_way(), arranged));

    // Offered in another order: each still ends up in the slot it had.
    let again = windows(&[(3, "kitty"), (1, "kitty"), (2, "kitty")]);
    let recalled = memory.recall(&again).unwrap();
    assert_eq!(recalled.template, &three_way());
    assert_eq!(ids(&recalled), vec![1, 2, 3]);
}

#[test]
fn new_windows_with_the_same_apps_fill_the_slots_in_order() {
    let mut memory = LayoutMemory::default();
    memory.remember(
        three_way(),
        windows(&[(1, "kitty"), (2, "kitty"), (3, "kitty")]),
    );
    let reopened = windows(&[(7, "kitty"), (8, "kitty"), (9, "kitty")]);
    assert_eq!(ids(&memory.recall(&reopened).unwrap()), vec![7, 8, 9]);
}

#[test]
fn a_window_goes_to_a_slot_of_its_own_app_when_its_own_slot_is_gone() {
    let mut memory = LayoutMemory::default();
    memory.remember(
        three_way(),
        windows(&[(1, "firefox"), (2, "kitty"), (3, "kitty")]),
    );
    // Window 3 is still there and takes its slot, the new firefox takes the firefox slot, and
    // the other new kitty the remaining kitty slot.
    let now = windows(&[(10, "kitty"), (11, "firefox"), (3, "kitty")]);
    assert_eq!(ids(&memory.recall(&now).unwrap()), vec![11, 10, 3]);
}

#[test]
fn other_apps_or_another_count_recall_nothing() {
    let mut memory = LayoutMemory::default();
    memory.remember(
        three_way(),
        windows(&[(1, "kitty"), (2, "kitty"), (3, "kitty")]),
    );
    assert!(memory
        .recall(&windows(&[(1, "kitty"), (2, "kitty"), (3, "foot")]))
        .is_none());
    assert!(memory
        .recall(&windows(&[(1, "kitty"), (2, "kitty")]))
        .is_none());
    assert!(memory
        .recall(&windows(&[
            (1, "kitty"),
            (2, "kitty"),
            (3, "kitty"),
            (4, "kitty")
        ]))
        .is_none());
}

#[test]
fn nothing_is_remembered_for_one_window_or_a_window_without_an_app_id() {
    let mut memory = LayoutMemory::default();
    assert!(!memory.remember(Template::Slot(None), windows(&[(1, "kitty")])));
    let pair = Template::parse("h0.5(_,_)").unwrap();
    assert!(!memory.remember(pair.clone(), windows(&[(1, "kitty"), (2, "")])));
    assert!(memory.is_empty());
    assert!(memory.recall(&windows(&[(1, "kitty"), (2, "")])).is_none());
}

#[test]
fn a_template_that_does_not_fit_the_windows_is_refused() {
    let mut memory = LayoutMemory::default();
    let pair = Template::parse("h0.5(_,_)").unwrap();
    assert!(!memory.remember(pair, windows(&[(1, "kitty"), (2, "kitty"), (3, "kitty")])));
    assert!(memory.is_empty());
}

#[test]
fn the_latest_arrangement_of_the_same_apps_wins() {
    let mut memory = LayoutMemory::default();
    let apps = windows(&[(1, "kitty"), (2, "kitty"), (3, "kitty")]);
    memory.remember(three_way(), apps.clone());
    let other = Template::parse("v0.5(_,h0.5(_,_))").unwrap();
    memory.remember(other.clone(), apps.clone());
    assert_eq!(memory.len(), 1);
    assert_eq!(memory.recall(&apps).unwrap().template, &other);
}

#[test]
fn the_order_of_the_app_ids_does_not_matter() {
    let mut memory = LayoutMemory::default();
    memory.remember(
        three_way(),
        windows(&[(1, "kitty"), (2, "firefox"), (3, "kitty")]),
    );
    let now = windows(&[(5, "firefox"), (6, "kitty"), (7, "kitty")]);
    assert!(memory.recall(&now).is_some());
}

#[test]
fn only_the_newest_arrangements_are_kept() {
    let mut memory = LayoutMemory::default();
    let pair = Template::parse("h0.5(_,_)").unwrap();
    for n in 0..=MAX_REMEMBERED {
        let app = format!("app{n}");
        memory.remember(
            pair.clone(),
            windows(&[(1, app.as_str()), (2, app.as_str())]),
        );
    }
    assert_eq!(memory.len(), MAX_REMEMBERED);
    assert!(memory
        .recall(&windows(&[(1, "app0"), (2, "app0")]))
        .is_none());
    let newest = format!("app{MAX_REMEMBERED}");
    assert!(memory
        .recall(&windows(&[(1, newest.as_str()), (2, newest.as_str())]))
        .is_some());
}

#[test]
fn remember_container_layouts_is_off_unless_asked_for() {
    assert!(!Config::parse("").unwrap().remember_container_layouts());
    assert!(Config::parse("remember-container-layouts")
        .unwrap()
        .remember_container_layouts());
    assert!(Config::parse("remember-container-layouts #true")
        .unwrap()
        .remember_container_layouts());
    assert!(!Config::parse("remember-container-layouts #false")
        .unwrap()
        .remember_container_layouts());
}

#[test]
fn check_config_warns_about_remember_container_layouts_without_tiling_containers() {
    let warnings = Config::check_text("remember-container-layouts").unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("remember-container-layouts")),
        "{warnings:?}"
    );
    let warnings = Config::check_text("tiling-containers\nremember-container-layouts").unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w.contains("remember-container-layouts")),
        "{warnings:?}"
    );
}
