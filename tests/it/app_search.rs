//! Tests for `jff_wm::primitives::app_search`.

use std::collections::HashMap;

use jff_wm::primitives::app_search::{
    entry_for_app_id, exec_argv, first_pick, launch_plan, parse_desktop_entry, search_apps,
    step_in_apps, step_pick, terminal_exec_args, valid_pick, window_matches, DesktopEntry, Env,
    Pick, Query, MAX_QUERY,
};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn parse(text: &str) -> Option<DesktopEntry> {
    parse_desktop_entry(
        "app",
        "/usr/share/applications/app.desktop",
        text,
        &Env::default(),
    )
}

fn entry(id: &str, name: &str) -> DesktopEntry {
    DesktopEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        exec: id.to_owned(),
        ..DesktopEntry::default()
    }
}

fn names(entries: &[DesktopEntry], hits: &[usize]) -> Vec<String> {
    hits.iter().map(|&i| entries[i].name.clone()).collect()
}

// ---- parsing

const FIREFOX: &str = "\
[Desktop Entry]
Version=1.0
Type=Application
Name=Firefox
Name[pt_BR]=Firefox Navegador
GenericName=Web Browser
GenericName[pt]=Navegador Web
Comment=Browse the web
Exec=firefox %u
Icon=firefox
Terminal=false
Keywords=internet;www;browser;
Categories=Network;WebBrowser;

[Desktop Action new-window]
Name=New Window
Exec=firefox --new-window
";

#[test]
fn reads_the_main_group_only() {
    let entry = parse(FIREFOX).expect("listed");
    assert_eq!(entry.id, "app");
    assert_eq!(entry.name, "Firefox");
    assert_eq!(entry.generic_name, "Web Browser");
    assert_eq!(entry.exec, "firefox %u");
    assert_eq!(entry.icon, "firefox");
    assert_eq!(entry.keywords, ["internet", "www", "browser"]);
    assert!(!entry.terminal);
    assert_eq!(entry.source, "/usr/share/applications/app.desktop");
}

#[test]
fn prefers_the_translation_of_the_best_locale() {
    let locales = strings(&["pt_BR", "pt"]);
    let env = Env {
        desktops: &[],
        locales: &locales,
    };
    let entry = parse_desktop_entry("app", "x", FIREFOX, &env).unwrap();
    assert_eq!(entry.name, "Firefox Navegador");
    // only `pt` exists for this one
    assert_eq!(entry.generic_name, "Navegador Web");
}

#[test]
fn hidden_entries_are_skipped() {
    assert!(parse("[Desktop Entry]\nName=A\nExec=a\nNoDisplay=true\n").is_none());
    assert!(parse("[Desktop Entry]\nName=A\nExec=a\nHidden=true\n").is_none());
    assert!(parse("[Desktop Entry]\nType=Link\nName=A\nExec=a\n").is_none());
    assert!(parse("[Desktop Entry]\nName=A\n").is_none());
    assert!(parse("[Desktop Entry]\nExec=a\n").is_none());
    // `false` is not hidden
    assert!(parse("[Desktop Entry]\nName=A\nExec=a\nNoDisplay=false\n").is_some());
}

#[test]
fn only_show_in_and_not_show_in_follow_the_desktop() {
    let sway = strings(&["sway"]);
    let env = Env {
        desktops: &sway,
        locales: &[],
    };
    let only_gnome = "[Desktop Entry]\nName=A\nExec=a\nOnlyShowIn=GNOME;KDE;\n";
    let only_sway = "[Desktop Entry]\nName=A\nExec=a\nOnlyShowIn=Sway;\n";
    let not_sway = "[Desktop Entry]\nName=A\nExec=a\nNotShowIn=sway;\n";
    assert!(parse_desktop_entry("a", "x", only_gnome, &env).is_none());
    assert!(parse_desktop_entry("a", "x", only_sway, &env).is_some());
    assert!(parse_desktop_entry("a", "x", not_sway, &env).is_none());
    // no desktop known: nothing is held against the entry
    assert!(parse(only_gnome).is_some());
}

#[test]
fn escapes_and_lists_are_read() {
    let entry = parse(
        "[Desktop Entry]\nName=A\\sB\nExec=a\nKeywords=one\\;two;three;\nPath=/tmp\nTerminal=true\nTryExec=a\n",
    )
    .unwrap();
    assert_eq!(entry.name, "A B");
    assert_eq!(entry.keywords, ["one;two", "three"]);
    assert_eq!(entry.path, "/tmp");
    assert!(entry.terminal);
    assert_eq!(entry.try_exec, "a");
}

// ---- Exec

fn argv(exec: &str) -> Vec<String> {
    exec_argv(&DesktopEntry {
        name: "Name".to_owned(),
        icon: "icon".to_owned(),
        source: "/src.desktop".to_owned(),
        exec: exec.to_owned(),
        ..DesktopEntry::default()
    })
}

#[test]
fn field_codes_are_stripped() {
    assert_eq!(argv("firefox %u"), ["firefox"]);
    assert_eq!(argv("gimp-2.10 %U"), ["gimp-2.10"]);
    assert_eq!(
        argv("vlc --started-from-file %U"),
        ["vlc", "--started-from-file"]
    );
    assert_eq!(argv("app %f %F %d %D %n %N %v %m"), ["app"]);
}

#[test]
fn code_text_that_is_not_a_code_stays() {
    assert_eq!(argv("app 100%%"), ["app", "100%"]);
    assert_eq!(argv("app --file=%f"), ["app", "--file="]);
}

#[test]
fn name_icon_and_file_codes_expand() {
    assert_eq!(argv("app %c"), ["app", "Name"]);
    assert_eq!(argv("app %i"), ["app", "--icon", "icon"]);
    assert_eq!(argv("app %k"), ["app", "/src.desktop"]);
}

#[test]
fn quotes_group_arguments() {
    assert_eq!(
        argv(r#"sh -c "echo hello world" %U"#),
        ["sh", "-c", "echo hello world"]
    );
    assert_eq!(argv(r#"app "a \"b\" c""#), ["app", r#"a "b" c"#]);
    assert_eq!(argv("  app    one  "), ["app", "one"]);
    // an empty argument that was written is kept
    assert_eq!(argv(r#"app "" x"#), ["app", "", "x"]);
}

#[test]
fn no_command_gives_no_plan() {
    assert_eq!(argv("%U"), Vec::<String>::new());
    assert!(launch_plan(&entry("x", "X"), "alacritty").is_some());
    let empty = DesktopEntry {
        exec: "%U".to_owned(),
        ..DesktopEntry::default()
    };
    assert!(launch_plan(&empty, "alacritty").is_none());
}

#[test]
fn terminal_apps_run_inside_a_terminal() {
    let htop = DesktopEntry {
        exec: "htop".to_owned(),
        terminal: true,
        path: "/home/me".to_owned(),
        ..DesktopEntry::default()
    };
    let plan = launch_plan(&htop, "alacritty").unwrap();
    assert_eq!(plan.argv, ["alacritty", "-e", "htop"]);
    assert_eq!(plan.cwd.as_deref(), Some("/home/me"));

    let plan = launch_plan(&htop, "/usr/bin/gnome-terminal").unwrap();
    assert_eq!(plan.argv, ["/usr/bin/gnome-terminal", "--", "htop"]);
    let plan = launch_plan(&htop, "foot").unwrap();
    assert_eq!(plan.argv, ["foot", "htop"]);
}

#[test]
fn plain_apps_have_no_terminal_and_no_directory_unless_asked() {
    let plan = launch_plan(&entry("calc", "Calculator"), "alacritty").unwrap();
    assert_eq!(plan.argv, ["calc"]);
    assert_eq!(plan.cwd, None);
    assert_eq!(terminal_exec_args("kitty"), Vec::<String>::new());
    assert_eq!(terminal_exec_args("wezterm"), ["start", "--"]);
    assert_eq!(terminal_exec_args("xterm"), ["-e"]);
}

// ---- matching

fn sample() -> Vec<DesktopEntry> {
    let mut firefox = entry("firefox", "Firefox");
    firefox.generic_name = "Web Browser".to_owned();
    firefox.keywords = strings(&["internet", "www"]);
    let mut files = entry("org.gnome.Nautilus", "Files");
    files.generic_name = "File Manager".to_owned();
    files.keywords = strings(&["folder", "explorer"]);
    let mut settings = entry("gnome-control-center", "Configurações");
    settings.generic_name = "Settings".to_owned();
    vec![
        firefox,
        files,
        settings,
        entry("firewall-config", "Firewall"),
        entry("terminal", "Terminal"),
        entry("text-editor", "Text Editor"),
    ]
}

fn search(entries: &[DesktopEntry], query: &str) -> Vec<String> {
    names(entries, &search_apps(entries, query, &HashMap::new(), 8))
}

#[test]
fn an_empty_query_finds_nothing() {
    assert!(search(&sample(), "").is_empty());
    assert!(search(&sample(), "   ").is_empty());
}

#[test]
fn prefix_of_the_name_matches() {
    // same quality, so by name
    assert_eq!(search(&sample(), "fire"), ["Firefox", "Firewall"]);
    assert_eq!(search(&sample(), "FIREF"), ["Firefox"]);
}

#[test]
fn exact_name_beats_longer_names() {
    let entries = vec![
        entry("a", "Terminal Emulator"),
        entry("b", "Term"),
        entry("c", "Terminal"),
    ];
    assert_eq!(
        search(&entries, "terminal"),
        ["Terminal", "Terminal Emulator"]
    );
    assert_eq!(search(&entries, "term")[0], "Term");
}

#[test]
fn start_of_a_word_beats_the_middle_of_one() {
    let entries = vec![entry("a", "Hexedit"), entry("b", "Text Editor")];
    assert_eq!(search(&entries, "edit"), ["Text Editor", "Hexedit"]);
}

#[test]
fn keywords_and_generic_names_match_too() {
    assert_eq!(search(&sample(), "browser"), ["Firefox"]);
    assert_eq!(search(&sample(), "internet"), ["Firefox"]);
    assert_eq!(search(&sample(), "folder"), ["Files"]);
}

#[test]
fn the_name_ranks_above_a_keyword() {
    let mut a = entry("a", "Notes");
    a.keywords = strings(&["editor"]);
    let b = entry("b", "Editor");
    assert_eq!(search(&[a, b], "editor"), ["Editor", "Notes"]);
}

#[test]
fn accents_and_case_do_not_matter() {
    assert_eq!(search(&sample(), "configuracoes"), ["Configurações"]);
    assert_eq!(search(&sample(), "CONFIGURAÇÕES"), ["Configurações"]);
    // the desktop file ID of the firewall tool has it too, further down
    assert_eq!(search(&sample(), "config"), ["Configurações", "Firewall"]);
}

#[test]
fn letters_in_order_match_a_name_with_gaps() {
    assert_eq!(search(&sample(), "ffx"), ["Firefox"]);
    // out of order: no
    assert!(search(&sample(), "xff").is_empty());
    // the first letter has to start a word: "fire" is not hiding in "Configurações"
    assert!(!search(&sample(), "fire").contains(&"Configurações".to_owned()));
}

#[test]
fn every_word_of_the_query_is_needed() {
    assert_eq!(search(&sample(), "text edit"), ["Text Editor"]);
    assert!(search(&sample(), "text firefox").is_empty());
    // different fields are fine
    assert_eq!(search(&sample(), "fire browser"), ["Firefox"]);
}

#[test]
fn launches_break_ties_only() {
    let entries = vec![entry("a", "Alpha Tool"), entry("b", "Beta Tool")];
    let mut launches = HashMap::new();
    launches.insert("b".to_owned(), 5);
    // equal match quality: the one launched more comes first
    let hits = search_apps(&entries, "tool", &launches, 8);
    assert_eq!(names(&entries, &hits), ["Beta Tool", "Alpha Tool"]);
    // a better match wins whatever the counts
    let hits = search_apps(&entries, "alpha", &launches, 8);
    assert_eq!(names(&entries, &hits), ["Alpha Tool"]);
    launches.insert("a".to_owned(), 9);
    let hits = search_apps(&entries, "tool", &launches, 8);
    assert_eq!(names(&entries, &hits), ["Alpha Tool", "Beta Tool"]);
}

#[test]
fn without_launches_equal_matches_go_by_name() {
    let entries = vec![entry("b", "Beta Tool"), entry("a", "Alpha Tool")];
    let hits = search_apps(&entries, "tool", &HashMap::new(), 8);
    assert_eq!(names(&entries, &hits), ["Alpha Tool", "Beta Tool"]);
}

#[test]
fn the_list_is_cut_at_the_limit() {
    let entries: Vec<DesktopEntry> = (0..20)
        .map(|i| entry(&format!("app{i}"), &format!("App {i:02}")))
        .collect();
    assert_eq!(search_apps(&entries, "app", &HashMap::new(), 8).len(), 8);
    assert_eq!(search_apps(&entries, "app", &HashMap::new(), 3).len(), 3);
}

#[test]
fn windows_match_on_title_or_app_id() {
    assert!(window_matches("fire", "Mozilla Firefox", "firefox"));
    assert!(window_matches(
        "mozilla",
        "Mozilla Firefox",
        "org.mozilla.firefox"
    ));
    assert!(window_matches("foot", "~/src", "foot"));
    assert!(window_matches("src", "~/src", "foot"));
    assert!(!window_matches("chrome", "Mozilla Firefox", "firefox"));
}

#[test]
fn windows_need_every_word_and_no_gaps() {
    assert!(window_matches("mozilla fox", "Mozilla Firefox", "firefox"));
    assert!(!window_matches(
        "mozilla chrome",
        "Mozilla Firefox",
        "firefox"
    ));
    // letters with gaps find apps but not windows
    assert!(!window_matches("ffx", "Mozilla Firefox", "firefox"));
    assert!(window_matches("", "anything", "x"));
}

// ---- query

#[test]
fn the_query_takes_text_and_gives_it_back() {
    let mut query = Query::default();
    assert!(query.is_empty());
    assert!(query.push('f'));
    assert!(query.push('i'));
    assert_eq!(query.as_str(), "fi");
    assert!(query.backspace());
    assert_eq!(query.as_str(), "f");
    assert!(query.backspace());
    assert!(!query.backspace());
    assert!(query.is_empty());
}

#[test]
fn the_query_ignores_what_is_not_text() {
    let mut query = Query::default();
    assert!(!query.push(' '), "no space at the start");
    assert!(!query.push('\n'));
    assert!(!query.push('\u{1b}'));
    assert!(query.push('a'));
    assert!(query.push(' '));
    assert!(query.push('\u{e7}'));
    assert_eq!(query.as_str(), "a ç");
    query.clear();
    assert!(query.is_empty());
}

#[test]
fn the_query_has_a_length_limit() {
    let mut query = Query::default();
    for _ in 0..MAX_QUERY {
        assert!(query.push('a'));
    }
    assert!(!query.push('a'));
    assert_eq!(query.as_str().chars().count(), MAX_QUERY);
}

// ---- selection

#[test]
fn the_first_result_is_a_window_else_an_app() {
    assert_eq!(first_pick(2, 3), Some(Pick::Window(0)));
    assert_eq!(first_pick(0, 3), Some(Pick::App(0)));
    assert_eq!(first_pick(0, 0), None);
}

#[test]
fn a_pick_that_points_at_nothing_falls_back() {
    assert_eq!(
        valid_pick(Some(Pick::Window(1)), 2, 3),
        Some(Pick::Window(1))
    );
    assert_eq!(
        valid_pick(Some(Pick::Window(2)), 2, 3),
        Some(Pick::Window(0))
    );
    assert_eq!(valid_pick(Some(Pick::App(3)), 0, 3), Some(Pick::App(0)));
    assert_eq!(valid_pick(Some(Pick::App(0)), 2, 0), Some(Pick::Window(0)));
    assert_eq!(valid_pick(None, 0, 0), None);
}

#[test]
fn tab_goes_over_the_windows_then_the_apps_and_wraps() {
    let mut pick = None;
    let mut seen = Vec::new();
    for _ in 0..6 {
        pick = step_pick(pick, 2, 2, true);
        seen.push(pick.unwrap());
    }
    assert_eq!(
        seen,
        [
            Pick::Window(0),
            Pick::Window(1),
            Pick::App(0),
            Pick::App(1),
            Pick::Window(0),
            Pick::Window(1)
        ]
    );
}

#[test]
fn shift_tab_goes_back_and_starts_at_the_end() {
    assert_eq!(step_pick(None, 2, 2, false), Some(Pick::App(1)));
    assert_eq!(
        step_pick(Some(Pick::App(0)), 2, 2, false),
        Some(Pick::Window(1))
    );
    assert_eq!(
        step_pick(Some(Pick::Window(0)), 2, 2, false),
        Some(Pick::App(1))
    );
    assert_eq!(step_pick(None, 0, 0, true), None);
}

#[test]
fn up_and_down_walk_the_app_rows() {
    assert_eq!(step_in_apps(0, 2, 3, false), Some(Pick::App(1)));
    assert_eq!(step_in_apps(2, 2, 3, false), None);
    assert_eq!(step_in_apps(2, 2, 3, true), Some(Pick::App(1)));
    // up from the first row goes back to the windows
    assert_eq!(step_in_apps(0, 2, 3, true), Some(Pick::Window(1)));
    assert_eq!(step_in_apps(0, 0, 3, true), None);
}

// ---- icons

#[test]
fn icons_are_scaled_to_a_square_and_keep_their_color() {
    use jff_wm::feature::app_icons::scale;
    // a 4x2 opaque red picture into 8x8: red in the middle band, empty above and below
    let red: Vec<u8> = (0..8).flat_map(|_| [255, 0, 0, 255]).collect();
    let out = scale(&red, 4, 2, 8);
    assert_eq!(out.len(), 8 * 8 * 4);
    let at = |x: usize, y: usize| &out[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
    assert_eq!(at(4, 4), [255, 0, 0, 255]);
    assert_eq!(at(4, 0)[3], 0);
    assert_eq!(at(4, 7)[3], 0);
}

#[test]
fn transparent_pixels_do_not_darken_the_edges() {
    use jff_wm::feature::app_icons::scale;
    // one white opaque pixel and one clear black one, squeezed into a single pixel
    let out = scale(&[255, 255, 255, 255, 0, 0, 0, 0], 2, 1, 1);
    assert_eq!(&out[..3], [255, 255, 255]);
}

#[test]
fn no_icon_name_gives_no_icon() {
    assert!(jff_wm::feature::app_icons::icon_for("").is_none());
    assert!(jff_wm::feature::app_icons::icon_for("../etc/passwd").is_none());
}

// ---- app grid

#[test]
fn grid_arrows_move_by_tile_and_row() {
    use jff_wm::primitives::app_search::step_in_app_grid as step;
    use jff_wm::primitives::overview_grid::Dir;
    // 7 apps, 4 per row:  0 1 2 3 / 4 5 6
    assert_eq!(step(1, 2, 7, 4, Dir::Right), Some(Pick::App(2)));
    assert_eq!(step(1, 2, 7, 4, Dir::Left), Some(Pick::App(0)));
    assert_eq!(step(0, 2, 7, 4, Dir::Left), None);
    assert_eq!(step(6, 2, 7, 4, Dir::Right), None);
    assert_eq!(step(1, 2, 7, 4, Dir::Down), Some(Pick::App(5)));
    // below tile 3 there is no tile: go to the last one
    assert_eq!(step(3, 2, 7, 4, Dir::Down), Some(Pick::App(6)));
    assert_eq!(step(5, 2, 7, 4, Dir::Down), None);
    assert_eq!(step(5, 2, 7, 4, Dir::Up), Some(Pick::App(1)));
    // up from the first row goes back to the windows
    assert_eq!(step(2, 2, 7, 4, Dir::Up), Some(Pick::Window(1)));
    assert_eq!(step(2, 0, 7, 4, Dir::Up), None);
}

#[test]
fn grid_size_and_hit_test_agree() {
    use jff_wm::feature::overview_search::{
        columns_for_width, grid_size, tile_at, LIST_PAD, TILE_H, TILE_W,
    };
    assert_eq!(grid_size(0, 1000), (0, 0));
    let (w, h) = grid_size(6, 1000);
    // 6 apps: 4 in the first row, 2 in the second
    assert_eq!(columns_for_width(w), 4);
    assert_eq!(h, 2 * TILE_H + 2 * LIST_PAD);
    assert_eq!(tile_at(LIST_PAD + 1, LIST_PAD + 1, w, 6), Some(0));
    assert_eq!(
        tile_at(LIST_PAD + TILE_W + 1, LIST_PAD + TILE_H + 1, w, 6),
        Some(5)
    );
    // the empty corner of the short last row, and the panel's padding
    assert_eq!(
        tile_at(LIST_PAD + 3 * TILE_W + 1, LIST_PAD + TILE_H + 1, w, 6),
        None
    );
    assert_eq!(tile_at(2, 2, w, 6), None);
    // a narrow screen gets fewer columns
    let (narrow, _) = grid_size(6, TILE_W + 2 * LIST_PAD);
    assert_eq!(columns_for_width(narrow), 1);
}

// ---- windows to apps

#[test]
fn window_app_id_finds_its_entry() {
    let with = |id: &str, wm: &str, icon: &str| DesktopEntry {
        id: id.to_owned(),
        name: id.to_owned(),
        wm_class: wm.to_owned(),
        icon: icon.to_owned(),
        ..DesktopEntry::default()
    };
    let entries = [
        with("org.gnome.Nautilus", "", "org.gnome.Nautilus"),
        with("firefox", "", "firefox"),
        with("code", "Code", "vscode"),
        with("steam", "", "steam"),
    ];
    let find = |app_id: &str| entry_for_app_id(&entries, app_id);
    assert_eq!(find("firefox"), Some(1));
    assert_eq!(find("Firefox"), Some(1));
    assert_eq!(find("org.mozilla.firefox"), Some(1));
    assert_eq!(find("nautilus"), Some(0));
    assert_eq!(find("org.gnome.Nautilus"), Some(0));
    assert_eq!(find("Code"), Some(2));
    assert_eq!(find("vscode"), Some(2));
    assert_eq!(find("steam.desktop"), Some(3));
    assert_eq!(find("unknown"), None);
    assert_eq!(find(""), None);
}

#[test]
fn browser_web_apps_are_found_by_their_long_id() {
    let youtube = parse_desktop_entry(
        "msedge-agimnkijcaahngcdmfeangaknmldooml-Default",
        "/home/me/.local/share/applications/msedge-agimnkijcaahngcdmfeangaknmldooml-Default.desktop",
        "[Desktop Entry]\nType=Application\nName=YouTube\n\
         Exec=/opt/microsoft/msedge/microsoft-edge --profile-directory=Default --app-id=agimnkijcaahngcdmfeangaknmldooml\n\
         Icon=msedge-agimnkijcaahngcdmfeangaknmldooml-Default\n\
         StartupWMClass=crx__agimnkijcaahngcdmfeangaknmldooml\n",
        &Env::default(),
    )
    .unwrap();
    assert_eq!(youtube.wm_class, "crx__agimnkijcaahngcdmfeangaknmldooml");
    let entries = [entry("microsoft-edge", "Microsoft Edge"), youtube];
    for app_id in [
        "msedge-agimnkijcaahngcdmfeangaknmldooml-Default",
        "crx__agimnkijcaahngcdmfeangaknmldooml",
        "crx_agimnkijcaahngcdmfeangaknmldooml",
        "chrome-agimnkijcaahngcdmfeangaknmldooml-Default",
    ] {
        assert_eq!(entry_for_app_id(&entries, app_id), Some(1), "{app_id}");
    }
    assert_eq!(entry_for_app_id(&entries, "microsoft-edge"), Some(0));
    assert_eq!(entry_for_app_id(&entries, "msedge"), None);
}
