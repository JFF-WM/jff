mod lists;
mod pages;
mod widgets;

use adw::prelude::*;
use gtk4 as gtk;
use jff_settings_core::JffConfig;
use libadwaita as adw;
use pages::Body;
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "dev.jff.Settings";
const TITLE: &str = "jff Settings";

/// Everything a row needs to edit the config. Cheap to clone, it's all Rc inside.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) cfg: Rc<RefCell<JffConfig>>,
    /// Call after every edit, it queues a save.
    pub(crate) changed: Rc<dyn Fn()>,
    pub(crate) toast: Rc<dyn Fn(String)>,
}

fn icon(page_id: &str) -> &'static str {
    match page_id {
        "appearance" => "preferences-desktop-appearance-symbolic",
        "effects" => "applications-graphics-symbolic",
        "multitasking" => "view-grid-symbolic",
        "displays" => "video-display-symbolic",
        "keyboard" => "input-keyboard-symbolic",
        "shortcuts" => "preferences-desktop-keyboard-shortcuts-symbolic",
        "rules" => "window-new-symbolic",
        "startup" => "system-run-symbolic",
        _ => "preferences-system-symbolic",
    }
}

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(TITLE)
        .default_width(980)
        .default_height(640)
        .build();

    let cfg = match JffConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            // better to stop here than edit a file we couldn't read
            let status = adw::StatusPage::builder()
                .icon_name("dialog-warning-symbolic")
                .title("Can't Read the jff Config")
                .description(gtk::glib::markup_escape_text(&e.to_string()).as_str())
                .build();
            let view = adw::ToolbarView::new();
            view.add_top_bar(&adw::HeaderBar::new());
            view.set_content(Some(&status));
            window.set_content(Some(&view));
            window.present();
            return;
        }
    };

    let banner = adw::Banner::new("");
    if cfg.is_new() {
        banner.set_title(&format!(
            "No config yet. The first change creates {}",
            cfg.path().display()
        ));
        banner.set_revealed(true);
    }
    let config_path = cfg.path().to_path_buf();
    let cfg = Rc::new(RefCell::new(cfg));
    let toasts = adw::ToastOverlay::new();

    // saves go out half a second after the last edit, so typing in a field doesn't write
    // the file on every keystroke. jff watches it, a good save applies right away
    let pending: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::new(RefCell::new(None));
    let save_now: Rc<dyn Fn()> = {
        let cfg = cfg.clone();
        let toasts = toasts.clone();
        let banner = banner.clone();
        Rc::new(move || {
            let result = cfg.borrow_mut().save();
            match result {
                Ok(()) => banner.set_revealed(false),
                Err(e) => {
                    let toast = adw::Toast::new(&format!("Not applied: {e}"));
                    toast.set_timeout(8);
                    toasts.add_toast(toast);
                }
            }
        })
    };
    let ctx = Ctx {
        cfg: cfg.clone(),
        toast: {
            let toasts = toasts.clone();
            Rc::new(move |message: String| {
                let toast = adw::Toast::new(&message);
                toast.set_timeout(6);
                toasts.add_toast(toast);
            })
        },
        changed: {
            let pending = pending.clone();
            let save_now = save_now.clone();
            Rc::new(move || {
                if let Some(id) = pending.borrow_mut().take() {
                    id.remove();
                }
                let pending_inner = pending.clone();
                let save_now = save_now.clone();
                let id = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(500),
                    move || {
                        // already fired, forget the id or it gets removed a second time
                        pending_inner.borrow_mut().take();
                        save_now();
                    },
                );
                *pending.borrow_mut() = Some(id);
            })
        },
    };

    // pages

    let pages = pages::build();
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    for page in &pages {
        let widget: gtk::Widget = match &page.body {
            Body::Groups(groups) => {
                let prefs = adw::PreferencesPage::new();
                widgets::build_groups(&prefs, groups, &[], &ctx);
                prefs.upcast()
            }
            Body::Outputs(extra) => lists::outputs_page(extra, &ctx),
            Body::Rules => lists::rules_page(&ctx),
            Body::Binds => lists::binds_page(&ctx),
        };
        stack.add_named(&widget, Some(page.id));
    }

    // sidebar

    let sidebar_list = gtk::ListBox::new();
    sidebar_list.set_selection_mode(gtk::SelectionMode::Single);
    sidebar_list.add_css_class("navigation-sidebar");
    for page in &pages {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(6);
        row.set_margin_end(6);
        row.append(&gtk::Image::from_icon_name(icon(page.id)));
        row.append(
            &gtk::Label::builder()
                .label(page.title)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
        );
        sidebar_list.append(&row);
    }
    let no_results = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("No Results Found")
        .description("Try a different search")
        .build();
    no_results.add_css_class("compact");
    sidebar_list.set_placeholder(Some(&no_results));

    let search_text: Rc<Vec<String>> = Rc::new(pages.iter().map(|p| p.search_text()).collect());
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search Settings"));
    let matches = {
        let (search_text, search_entry) = (search_text.clone(), search_entry.clone());
        move |index: usize| {
            let query = search_entry.text().to_lowercase();
            query
                .split_whitespace()
                .all(|word| search_text.get(index).is_some_and(|t| t.contains(word)))
        }
    };
    {
        let matches = matches.clone();
        sidebar_list.set_filter_func(move |row| matches(row.index() as usize));
    }

    let split = adw::NavigationSplitView::new();

    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&search_entry));
    search_bar.connect_entry(&search_entry);
    search_bar.set_key_capture_widget(Some(&window));

    let search_toggle = gtk::ToggleButton::new();
    search_toggle.set_icon_name("system-search-symbolic");
    search_toggle.set_tooltip_text(Some("Search (Ctrl+F)"));
    search_bar
        .bind_property("search-mode-enabled", &search_toggle, "active")
        .bidirectional()
        .sync_create()
        .build();

    {
        // keep a page selected that is actually visible, and let Enter jump to the first hit
        let (list, matches, count) = (sidebar_list.clone(), matches.clone(), pages.len());
        search_entry.connect_search_changed(move |_| {
            list.invalidate_filter();
            let current = list.selected_row().map(|r| r.index() as usize);
            if current.map_or(true, |i| !matches(i)) {
                if let Some(first) = (0..count).find(|i| matches(*i)) {
                    list.select_row(list.row_at_index(first as i32).as_ref());
                }
            }
        });
    }
    {
        let (list, split, count) = (sidebar_list.clone(), split.clone(), pages.len());
        search_entry.connect_activate(move |_| {
            if let Some(first) = (0..count).find(|i| matches(*i)) {
                list.select_row(list.row_at_index(first as i32).as_ref());
                split.set_show_content(true);
            }
        });
    }

    let menu = gtk::gio::Menu::new();
    menu.append(Some("Open Config File"), Some("win.open-config"));
    menu.append(Some("About jff Settings"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .primary(true)
        .menu_model(&menu)
        .build();

    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.pack_start(&search_toggle);
    sidebar_header.pack_end(&menu_button);
    let sidebar_scroll = gtk::ScrolledWindow::new();
    sidebar_scroll.set_vexpand(true);
    sidebar_scroll.set_child(Some(&sidebar_list));
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.add_top_bar(&search_bar);
    sidebar_view.set_content(Some(&sidebar_scroll));
    let sidebar_page = adw::NavigationPage::new(&sidebar_view, "Settings");

    // content

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(&banner);
    content_box.append(&stack);
    toasts.set_child(Some(&content_box));
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&adw::HeaderBar::new());
    content_view.set_content(Some(&toasts));
    let first_title = pages.first().map_or("", |p| p.title);
    let content_page = adw::NavigationPage::new(&content_view, first_title);

    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(240.0);
    split.set_max_sidebar_width(300.0);

    {
        let entries: Vec<(&'static str, &'static str)> =
            pages.iter().map(|p| (p.id, p.title)).collect();
        let stack = stack.clone();
        let content_page = content_page.clone();
        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            if let Some((id, title)) = usize::try_from(row.index())
                .ok()
                .and_then(|i| entries.get(i))
            {
                stack.set_visible_child_name(id);
                content_page.set_title(title);
            }
        });
    }
    {
        // only a real click should slide to the content when the window is narrow,
        // selecting the first page on startup must not
        let split = split.clone();
        sidebar_list.connect_row_activated(move |_, _| split.set_show_content(true));
    }
    if let Some(first) = sidebar_list.row_at_index(0) {
        sidebar_list.select_row(Some(&first));
    }

    // narrow windows: sidebar and content take turns, like the real Settings
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        560.0,
        adw::LengthUnit::Sp,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    // actions

    let search_action = gtk::gio::SimpleAction::new("search", None);
    {
        let toggle = search_toggle.clone();
        search_action.connect_activate(move |_, _| toggle.set_active(!toggle.is_active()));
    }
    window.add_action(&search_action);
    app.set_accels_for_action("win.search", &["<Control>f"]);

    let about_action = gtk::gio::SimpleAction::new("about", None);
    {
        let window = window.downgrade();
        about_action.connect_activate(move |_, _| {
            let Some(window) = window.upgrade() else {
                return;
            };
            adw::AboutWindow::builder()
                .transient_for(&window)
                .application_name(TITLE)
                .application_icon("preferences-system-symbolic")
                .developer_name("The jff developers")
                .version(env!("CARGO_PKG_VERSION"))
                .comments("Edit the settings of the jff compositor")
                .license_type(gtk::License::Gpl30)
                .build()
                .present();
        });
    }
    window.add_action(&about_action);

    let open_action = gtk::gio::SimpleAction::new("open-config", None);
    {
        let window = window.downgrade();
        let toast = ctx.toast.clone();
        open_action.connect_activate(move |_, _| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let file = gtk::gio::File::for_path(&config_path);
            let toast = toast.clone();
            gtk::FileLauncher::new(Some(&file)).launch(
                Some(&window),
                gtk::gio::Cancellable::NONE,
                move |result| {
                    if let Err(e) = result {
                        if !e.matches(gtk::DialogError::Dismissed) {
                            toast(format!("Couldn't open the config file: {e}"));
                        }
                    }
                },
            );
        });
    }
    window.add_action(&open_action);

    window.set_content(Some(&split));
    // don't lose an edit that is still waiting for its save
    window.connect_close_request(move |_| {
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
            save_now();
        }
        gtk::glib::Propagation::Proceed
    });
    window.present();
}
