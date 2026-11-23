//! Tests for the clipboard history (`primitives::clipboard`) and the `clipboard` config block.

use jff_wm::config::Config;
use jff_wm::primitives::clipboard::{
    fit_to_budget, human_size, wanted_mimes, History, Offer, DEFAULT_HISTORY, DEFAULT_MAX_SIZE_MIB,
    MAX_HISTORY, MAX_MAX_SIZE_MIB, SECRET_HINT_MIME,
};

fn text(body: &str) -> Vec<Offer> {
    vec![
        Offer::new("text/plain;charset=utf-8", body.as_bytes().to_vec()),
        Offer::new("text/plain", body.as_bytes().to_vec()),
    ]
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn the_newest_copy_comes_first() {
    let mut history = History::default();
    history.set_limit(5);
    let a = history.push(10, text("a")).unwrap();
    let b = history.push(20, text("b")).unwrap();
    assert_ne!(a, b);
    assert_eq!(history.len(), 2);
    assert_eq!(history.latest().unwrap().id, b);
    let ids: Vec<u64> = history.iter().map(|entry| entry.id).collect();
    assert_eq!(ids, vec![b, a]);
}

#[test]
fn a_copy_with_nothing_in_it_is_not_kept() {
    let mut history = History::default();
    assert_eq!(history.push(1, Vec::new()), None);
    assert!(history.is_empty());
}

#[test]
fn the_same_contents_move_to_the_front_instead_of_repeating() {
    let mut history = History::default();
    history.set_limit(5);
    let a = history.push(10, text("a")).unwrap();
    history.push(20, text("b")).unwrap();
    let again = history.push(30, text("a")).unwrap();
    assert_eq!(again, a, "the entry keeps its id");
    assert_eq!(history.len(), 2);
    let latest = history.latest().unwrap();
    assert_eq!(latest.id, a);
    assert_eq!(latest.stamp, 30, "and its time is renewed");
}

#[test]
fn the_order_of_the_offers_does_not_make_a_copy_new() {
    let mut history = History::default();
    history.set_limit(5);
    let mut offers = text("a");
    let id = history.push(1, offers.clone()).unwrap();
    offers.reverse();
    assert_eq!(history.push(2, offers), Some(id));
    assert_eq!(history.len(), 1);
}

#[test]
fn the_same_text_in_another_mime_type_is_another_copy() {
    let mut history = History::default();
    history.set_limit(5);
    let a = history
        .push(1, vec![Offer::new("text/plain", b"x".to_vec())])
        .unwrap();
    let b = history
        .push(2, vec![Offer::new("text/html", b"x".to_vec())])
        .unwrap();
    assert_ne!(a, b);
    assert_eq!(history.len(), 2);
}

#[test]
fn the_limit_drops_the_oldest_and_never_reuses_an_id() {
    let mut history = History::default();
    assert_eq!(DEFAULT_HISTORY, 1);
    let a = history.push(1, text("a")).unwrap();
    let b = history.push(2, text("b")).unwrap();
    assert_eq!(history.len(), 1, "one copy by default");
    assert!(history.get(a).is_none());
    assert!(history.get(b).is_some());

    history.set_limit(2);
    let c = history.push(3, text("c")).unwrap();
    let d = history.push(4, text("d")).unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.get(b).is_none());
    assert!(c > b && d > c);

    // A smaller limit cuts right away, the newest copies stay.
    history.set_limit(1);
    assert_eq!(history.len(), 1);
    assert_eq!(history.latest().unwrap().id, d);
}

#[test]
fn the_limit_is_kept_between_one_and_the_largest() {
    let mut history = History::default();
    history.set_limit(0);
    history.push(1, text("a"));
    history.push(2, text("b"));
    assert_eq!(history.len(), 1, "0 still keeps the latest copy");

    history.set_limit(MAX_HISTORY + 100);
    for n in 0..MAX_HISTORY + 10 {
        history.push(n as u64, text(&format!("copy {n}")));
    }
    assert_eq!(history.len(), MAX_HISTORY);
}

#[test]
fn picking_a_copy_moves_it_to_the_front() {
    let mut history = History::default();
    history.set_limit(5);
    let a = history.push(1, text("a")).unwrap();
    let b = history.push(2, text("b")).unwrap();
    assert!(history.promote(a, 9));
    assert_eq!(history.latest().unwrap().id, a);
    assert_eq!(history.latest().unwrap().stamp, 9);
    assert_eq!(history.iter().nth(1).unwrap().id, b);
    assert!(!history.promote(999, 10));
}

#[test]
fn clearing_forgets_everything() {
    let mut history = History::default();
    history.set_limit(5);
    history.push(1, text("a"));
    history.push(2, text("b"));
    history.clear();
    assert!(history.is_empty());
    assert!(history.latest().is_none());
}

#[test]
fn an_entry_knows_its_offers() {
    let mut history = History::default();
    let id = history.push(1, text("hello")).unwrap();
    let entry = history.get(id).unwrap();
    assert_eq!(entry.size(), 10);
    assert_eq!(
        entry.mime_types(),
        strings(&["text/plain;charset=utf-8", "text/plain"])
    );
    assert_eq!(&**entry.offer("text/plain").unwrap().data, b"hello");
    assert!(entry.offer("image/png").is_none());
}

#[test]
fn the_preview_is_one_short_line_of_the_text() {
    let mut history = History::default();
    history.set_limit(5);
    let id = history.push(1, text("  one\n\ttwo   three \n")).unwrap();
    assert_eq!(history.get(id).unwrap().preview(80), "one two three");
    assert_eq!(history.get(id).unwrap().preview(5), "one t…");
    assert_eq!(history.get(id).unwrap().preview(13), "one two three");
}

#[test]
fn the_preview_prefers_utf8_text_and_describes_anything_else() {
    let mut history = History::default();
    history.set_limit(5);
    let id = history
        .push(
            1,
            vec![
                Offer::new("text/html", b"<b>bold</b>".to_vec()),
                Offer::new("STRING", b"plain".to_vec()),
                Offer::new("text/plain;charset=UTF-8", "pão".as_bytes().to_vec()),
            ],
        )
        .unwrap();
    assert_eq!(history.get(id).unwrap().preview(80), "pão");

    let image = history
        .push(2, vec![Offer::new("image/png", vec![0u8; 2048])])
        .unwrap();
    assert_eq!(
        history.get(image).unwrap().preview(80),
        "[image/png 2.0 KiB]"
    );
}

#[test]
fn sizes_are_written_for_people() {
    assert_eq!(human_size(0), "0 B");
    assert_eq!(human_size(1023), "1023 B");
    assert_eq!(human_size(1024), "1.0 KiB");
    assert_eq!(human_size(1536), "1.5 KiB");
    assert_eq!(human_size(3 * 1024 * 1024), "3.0 MiB");
}

#[test]
fn a_secret_is_never_wanted() {
    let offered = strings(&["text/plain", SECRET_HINT_MIME]);
    assert_eq!(wanted_mimes(&offered), None);
    let shouted = strings(&["text/plain", "X-KDE-PASSWORDMANAGERHINT"]);
    assert_eq!(wanted_mimes(&shouted), None);
}

#[test]
fn x11_targets_and_repeats_are_not_data() {
    let offered = strings(&[
        "TARGETS",
        "text/plain;charset=utf-8",
        "TIMESTAMP",
        "text/plain",
        "text/plain",
        "",
        "SAVE_TARGETS",
        "image/png",
    ]);
    assert_eq!(
        wanted_mimes(&offered),
        Some(strings(&[
            "text/plain;charset=utf-8",
            "text/plain",
            "image/png"
        ]))
    );
    assert_eq!(wanted_mimes(&strings(&["TARGETS"])), Some(Vec::new()));
    assert_eq!(wanted_mimes(&[]), Some(Vec::new()));
}

#[test]
fn an_offer_that_does_not_fit_leaves_the_rest_a_chance() {
    let offers = vec![
        Offer::new("image/bmp", vec![0u8; 100]),
        Offer::new("text/plain", vec![b'a'; 10]),
        Offer::new("image/png", vec![0u8; 5]),
    ];
    let kept = fit_to_budget(offers.clone(), 20);
    let mimes: Vec<&str> = kept.iter().map(|o| o.mime.as_str()).collect();
    assert_eq!(mimes, vec!["text/plain", "image/png"]);

    assert_eq!(fit_to_budget(offers.clone(), 1000).len(), 3);
    assert!(fit_to_budget(offers, 0).is_empty());
    assert!(fit_to_budget(Vec::new(), 10).is_empty());
}

#[test]
fn the_budget_counts_all_offers_together() {
    let offers = vec![
        Offer::new("text/plain", vec![b'a'; 10]),
        Offer::new("text/html", vec![b'a'; 10]),
        Offer::new("UTF8_STRING", vec![b'a'; 10]),
    ];
    assert_eq!(fit_to_budget(offers, 25).len(), 2);
}

// ---- the `clipboard` config block ------------------------------------------------------------

#[test]
fn the_clipboard_is_kept_by_default() {
    for text in ["", "clipboard { }"] {
        let config = Config::parse(text).unwrap();
        let clipboard = config.clipboard();
        assert!(!clipboard.off, "{text:?}");
        assert_eq!(clipboard.history, DEFAULT_HISTORY);
        assert_eq!(clipboard.max_size, DEFAULT_MAX_SIZE_MIB * 1024 * 1024);
    }
}

#[test]
fn the_clipboard_block_is_read() {
    let config = Config::parse("clipboard { history 25; max-size 64; }").unwrap();
    assert!(!config.clipboard().off);
    assert_eq!(config.clipboard().history, 25);
    assert_eq!(config.clipboard().max_size, 64 * 1024 * 1024);

    let config = Config::parse("clipboard { off; }").unwrap();
    assert!(config.clipboard().off);
    let config = Config::parse("clipboard { off #false; history 3; }").unwrap();
    assert!(!config.clipboard().off);
    assert_eq!(config.clipboard().history, 3);
}

#[test]
fn the_clipboard_block_checks_its_numbers() {
    let ok = |line: &str| Config::parse(&format!("clipboard {{ {line}; }}")).is_ok();
    assert!(ok("history 1"));
    assert!(ok(&format!("history {MAX_HISTORY}")));
    assert!(!ok("history 0"));
    assert!(!ok(&format!("history {}", MAX_HISTORY + 1)));
    assert!(!ok("history -1"));
    assert!(!ok("history lots"));
    assert!(!ok("history"));
    assert!(ok("max-size 1"));
    assert!(ok(&format!("max-size {MAX_MAX_SIZE_MIB}")));
    assert!(!ok("max-size 0"));
    assert!(!ok(&format!("max-size {}", MAX_MAX_SIZE_MIB + 1)));
    assert!(!ok("max-size big"));
    assert!(!ok("persist"));
    assert!(Config::parse("clipboard { size 5; }").is_err());
}
