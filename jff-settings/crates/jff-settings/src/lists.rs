//! The parts of the config that are lists: text lines, displays, window rules, shortcuts.
//! A list gets rebuilt from the config whenever an entry is added, removed or moved, since
//! entries are addressed by position (`window-rule#2`, `*#5`).

use crate::pages::{self, Group, TextList};
use crate::widgets::{build_groups, build_row, refs};
use crate::Ctx;
use adw::prelude::*;
use gtk4 as gtk;
use jff_settings_core::actions::{self, ArgKind, ACTIONS};
use jff_settings_core::Arg;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_valign(gtk::Align::Center);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    button
}

fn trash_button(tooltip: &str) -> gtk::Button {
    flat_button("user-trash-symbolic", tooltip)
}

fn entry(title: &str) -> adw::EntryRow {
    let row = adw::EntryRow::new();
    row.set_title(title);
    row.set_show_apply_button(true);
    row
}

fn empty_state(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let status = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .build();
    status.add_css_class("compact");
    status
}

/// A group whose contents get thrown away and rebuilt. It remembers what it added so
/// `clear` can take it all out again.
#[derive(Clone)]
struct Dynamic {
    group: adw::PreferencesGroup,
    items: Rc<RefCell<Vec<gtk::Widget>>>,
}

impl Dynamic {
    fn new(title: &str, description: &str) -> Self {
        let group = adw::PreferencesGroup::new();
        group.set_title(&gtk::glib::markup_escape_text(title));
        if !description.is_empty() {
            group.set_description(Some(gtk::glib::markup_escape_text(description).as_str()));
        }
        Self {
            group,
            items: Rc::default(),
        }
    }

    fn push(&self, widget: &impl IsA<gtk::Widget>) {
        self.group.add(widget);
        self.items.borrow_mut().push(widget.clone().upcast());
    }

    fn clear(&self) {
        for item in self.items.borrow_mut().drain(..) {
            self.group.remove(&item);
        }
    }
}

/// Rebuild once the current signal handler is done. Removing the row that is still
/// running its own handler isn't safe.
fn later(f: impl FnOnce() + 'static) {
    gtk::glib::idle_add_local_once(f);
}

// text lists

/// A group of lines, each one editable and removable, with a row at the end to add one.
pub fn text_list(spec: &TextList, ctx: &Ctx) -> adw::PreferencesGroup {
    let list = Dynamic::new(spec.title, spec.description);
    fill_text_list(&list, spec, ctx);
    list.group
}

fn fill_text_list(list: &Dynamic, spec: &TextList, ctx: &Ctx) {
    list.clear();
    let rebuild = {
        let (list, spec, ctx) = (list.clone(), spec.clone(), ctx.clone());
        move || {
            let (list, spec, ctx) = (list.clone(), spec.clone(), ctx.clone());
            later(move || fill_text_list(&list, &spec, &ctx));
        }
    };
    let to_args = {
        let words = spec.words;
        move |text: &str| -> Vec<Arg> {
            if words {
                actions::split_words(text)
                    .into_iter()
                    .map(Arg::Str)
                    .collect()
            } else {
                vec![Arg::Str(text.trim().to_owned())]
            }
        }
    };
    let count = ctx.cfg.borrow().count(spec.parent, spec.name);
    for i in 0..count {
        let path: Vec<String> = spec
            .parent
            .iter()
            .map(|s| (*s).to_owned())
            .chain([format!("{}#{i}", spec.name)])
            .collect();
        let current = ctx.cfg.borrow().args(&refs(&path));
        let text = if spec.words {
            let strings: Vec<String> = current
                .iter()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect();
            actions::join_words(&strings)
        } else {
            current
                .first()
                .and_then(Arg::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let row = entry(spec.item);
        row.set_text(&text);
        {
            let (ctx, path, rebuild, to_args) =
                (ctx.clone(), path.clone(), rebuild.clone(), to_args.clone());
            row.connect_apply(move |r| {
                let text = r.text();
                if text.trim().is_empty() {
                    ctx.cfg.borrow_mut().remove(&refs(&path));
                    rebuild();
                } else {
                    ctx.cfg.borrow_mut().set_args(&refs(&path), &to_args(&text));
                }
                (ctx.changed)();
            });
        }
        let remove = trash_button("Remove");
        {
            let (ctx, path, rebuild) = (ctx.clone(), path.clone(), rebuild.clone());
            remove.connect_clicked(move |_| {
                ctx.cfg.borrow_mut().remove(&refs(&path));
                (ctx.changed)();
                rebuild();
            });
        }
        row.add_suffix(&remove);
        list.push(&row);
    }
    let add = entry(spec.add);
    {
        let (ctx, spec, rebuild, to_args) =
            (ctx.clone(), spec.clone(), rebuild.clone(), to_args.clone());
        add.connect_apply(move |r| {
            let text = r.text();
            if text.trim().is_empty() {
                return;
            }
            ctx.cfg
                .borrow_mut()
                .add(spec.parent, spec.name, &to_args(&text));
            r.set_text("");
            (ctx.changed)();
            rebuild();
        });
    }
    list.push(&add);
}

// displays

pub fn outputs_page(extra: &[Group], ctx: &Ctx) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let list = Dynamic::new(
        "Displays",
        "A display that isn't listed here uses its own defaults",
    );
    page.add(&list.group);

    let add_group = adw::PreferencesGroup::new();
    let add = entry("Connector Name, for example DP-1");
    {
        let (ctx, list) = (ctx.clone(), list.clone());
        add.connect_apply(move |r| {
            let name = r.text().trim().to_owned();
            if name.is_empty() {
                return;
            }
            ctx.cfg
                .borrow_mut()
                .add(&["outputs"], "output", &[Arg::Str(name)]);
            r.set_text("");
            (ctx.changed)();
            fill_outputs(&list, &ctx);
        });
    }
    add_group.set_title("Add a Display");
    add_group.add(&add);
    page.add(&add_group);

    fill_outputs(&list, ctx);
    build_groups(&page, extra, &[], ctx);
    page.upcast()
}

fn fill_outputs(list: &Dynamic, ctx: &Ctx) {
    list.clear();
    let count = ctx.cfg.borrow().count(&["outputs"], "output");
    if count == 0 {
        list.push(&empty_state(
            "video-display-symbolic",
            "No Displays Configured",
            "Add a display by its connector name to set its mode, position and more",
        ));
        return;
    }
    for i in 0..count {
        let root = vec!["outputs".to_owned(), format!("output#{i}")];
        let name = match ctx.cfg.borrow().args(&refs(&root)).first() {
            Some(Arg::Str(name)) => name.clone(),
            _ => String::new(),
        };
        let expander = adw::ExpanderRow::new();
        expander.set_title(&gtk::glib::markup_escape_text(&name));

        let name_row = entry("Connector Name");
        name_row.set_text(&name);
        {
            let (ctx, root, expander) = (ctx.clone(), root.clone(), expander.clone());
            name_row.connect_apply(move |r| {
                let text = r.text().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                ctx.cfg
                    .borrow_mut()
                    .set_args(&refs(&root), &[Arg::Str(text.clone())]);
                expander.set_title(&gtk::glib::markup_escape_text(&text));
                (ctx.changed)();
            });
        }
        expander.add_row(&name_row);
        for field in pages::output_fields() {
            expander.add_row(&build_row(&field, &root, ctx, None).0);
        }
        let remove_row = adw::ActionRow::new();
        remove_row.set_title("Remove This Display");
        remove_row.set_subtitle("It goes back to its defaults");
        let remove = trash_button("Remove");
        {
            let (ctx, root, list) = (ctx.clone(), root.clone(), list.clone());
            remove.connect_clicked(move |_| {
                ctx.cfg.borrow_mut().remove(&refs(&root));
                (ctx.changed)();
                let (ctx, list) = (ctx.clone(), list.clone());
                later(move || fill_outputs(&list, &ctx));
            });
        }
        remove_row.add_suffix(&remove);
        expander.add_row(&remove_row);
        list.push(&expander);
    }
}

// window rules

pub fn rules_page(ctx: &Ctx) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let list = Dynamic::new(
        "Window Rules",
        "The first rule that fits a window applies. Only the first match line of a rule is shown, any others are left alone",
    );
    let add_button = flat_button("list-add-symbolic", "Add Rule");
    {
        let (ctx, list) = (ctx.clone(), list.clone());
        add_button.connect_clicked(move |_| add_rule(&list, &ctx));
    }
    list.group.set_header_suffix(Some(&add_button));
    page.add(&list.group);
    fill_rules(&list, ctx);
    page.upcast()
}

fn add_rule(list: &Dynamic, ctx: &Ctx) {
    ctx.cfg.borrow_mut().add(&[], "window-rule", &[]);
    (ctx.changed)();
    let (list, ctx) = (list.clone(), ctx.clone());
    later(move || fill_rules(&list, &ctx));
}

fn fill_rules(list: &Dynamic, ctx: &Ctx) {
    list.clear();
    let count = ctx.cfg.borrow().count(&[], "window-rule");
    if count == 0 {
        let status = empty_state(
            "window-new-symbolic",
            "No Window Rules",
            "Rules open matching windows on a display or workspace, at a size, or floating",
        );
        let button = gtk::Button::with_label("Add Rule");
        button.set_halign(gtk::Align::Center);
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        {
            let (list, ctx) = (list.clone(), ctx.clone());
            button.connect_clicked(move |_| add_rule(&list, &ctx));
        }
        status.set_child(Some(&button));
        list.push(&status);
        return;
    }
    for i in 0..count {
        let node = format!("window-rule#{i}");
        let summary = {
            let cfg = ctx.cfg.borrow();
            [("app-id", "App ID"), ("title", "Title"), ("tag", "Tag")]
                .iter()
                .find_map(
                    |(key, label)| match cfg.get_prop(&[node.as_str(), "match"], key) {
                        Some(Arg::Str(s)) => Some(format!("{label}: {s}")),
                        _ => None,
                    },
                )
                .unwrap_or_else(|| "Any Window".to_owned())
        };
        let expander = adw::ExpanderRow::new();
        expander.set_title(&gtk::glib::markup_escape_text(&summary));
        expander.set_subtitle(&format!("Rule {}", i + 1));
        let root = vec![node];
        for field in pages::rule_fields() {
            expander.add_row(&build_row(&field, &root, ctx, None).0);
        }
        let order_row = adw::ActionRow::new();
        order_row.set_title("Order and Removal");
        let up = flat_button("go-up-symbolic", "Move Up");
        let down = flat_button("go-down-symbolic", "Move Down");
        let remove = trash_button("Remove This Rule");
        up.set_sensitive(i > 0);
        down.set_sensitive(i + 1 < count);
        let refill = {
            let (ctx, list) = (ctx.clone(), list.clone());
            move || {
                (ctx.changed)();
                let (ctx, list) = (ctx.clone(), list.clone());
                later(move || fill_rules(&list, &ctx));
            }
        };
        {
            let (ctx, refill) = (ctx.clone(), refill.clone());
            up.connect_clicked(move |_| {
                ctx.cfg.borrow_mut().swap(&[], "window-rule", i, i - 1);
                refill();
            });
        }
        {
            let (ctx, refill) = (ctx.clone(), refill.clone());
            down.connect_clicked(move |_| {
                ctx.cfg.borrow_mut().swap(&[], "window-rule", i, i + 1);
                refill();
            });
        }
        {
            let ctx = ctx.clone();
            remove.connect_clicked(move |_| {
                ctx.cfg
                    .borrow_mut()
                    .remove(&[format!("window-rule#{i}").as_str()]);
                refill();
            });
        }
        order_row.add_suffix(&up);
        order_row.add_suffix(&down);
        order_row.add_suffix(&remove);
        expander.add_row(&order_row);
        list.push(&expander);
    }
}

// keyboard shortcuts

pub fn binds_page(ctx: &Ctx) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let list = Dynamic::new(
        "Shortcuts",
        "Join modifiers and the key with +, like Alt+Return. Symbols go by name, like comma or minus",
    );
    page.add(&list.group);

    let add_group = adw::PreferencesGroup::new();
    add_group.set_title("Add a Shortcut");
    let add = entry("Key Combination, for example Super+Return");
    {
        let (ctx, list) = (ctx.clone(), list.clone());
        add.connect_apply(move |r| {
            let combo = r.text().trim().to_owned();
            if let Err(why) = actions::valid_combo(&combo) {
                (ctx.toast)(why);
                return;
            }
            {
                let mut cfg = ctx.cfg.borrow_mut();
                cfg.add(&["binds"], &combo, &[]);
                let last = format!("*#{}", cfg.child_names(&["binds"]).len() - 1);
                cfg.set_first_child(&["binds", last.as_str()], "terminal", &[]);
            }
            r.set_text("");
            (ctx.changed)();
            fill_binds(&list, &ctx);
        });
    }
    add_group.add(&add);
    page.add(&add_group);

    fill_binds(&list, ctx);
    page.upcast()
}

fn fill_binds(list: &Dynamic, ctx: &Ctx) {
    list.clear();
    let count = ctx.cfg.borrow().child_names(&["binds"]).len();
    if count == 0 {
        list.push(&empty_state(
            "input-keyboard-symbolic",
            "No Shortcuts",
            "Add a key combination below to bind it to an action",
        ));
        return;
    }
    for i in 0..count {
        list.push(&bind_row(list, ctx, i));
    }
}

fn bind_row(list: &Dynamic, ctx: &Ctx, index: usize) -> adw::ExpanderRow {
    let path = vec!["binds".to_owned(), format!("*#{index}")];
    let (combo, action, args) = {
        let cfg = ctx.cfg.borrow();
        let combo = cfg
            .child_names(&["binds"])
            .get(index)
            .cloned()
            .unwrap_or_default();
        let (action, args) = cfg.first_child(&refs(&path)).unwrap_or_default();
        (combo, action, args)
    };
    let args_text = actions::action_text(&action, &args);
    let expander = adw::ExpanderRow::new();
    expander.set_title(&gtk::glib::markup_escape_text(&combo));
    expander.set_subtitle(&gtk::glib::markup_escape_text(&format!(
        "{action} {args_text}"
    )));

    let combo_row = entry("Key Combination");
    combo_row.set_text(&combo);
    {
        let (ctx, path, expander) = (ctx.clone(), path.clone(), expander.clone());
        combo_row.connect_apply(move |r| {
            let new = r.text().trim().to_owned();
            match actions::valid_combo(&new) {
                Ok(()) => {
                    ctx.cfg.borrow_mut().rename(&refs(&path), &new);
                    expander.set_title(&gtk::glib::markup_escape_text(&new));
                    (ctx.changed)();
                }
                Err(why) => (ctx.toast)(why),
            }
        });
    }

    let names: Vec<&str> = ACTIONS.iter().map(|(name, _)| *name).collect();
    let action_row = adw::ComboRow::new();
    action_row.set_title("Action");
    action_row.set_model(Some(&gtk::StringList::new(&names)));
    // search in the popup needs to know which property of a StringObject to match on
    action_row.set_expression(Some(&gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        gtk::Expression::NONE,
        "string",
    )));
    action_row.set_enable_search(true);
    action_row.set_selected(
        names
            .iter()
            .position(|n| *n == action)
            .map_or(gtk::INVALID_LIST_POSITION, |p| p as u32),
    );

    let args_row = entry("Arguments");
    args_row.set_text(&args_text);
    args_row.set_visible(actions::kind_of(&action) != ArgKind::None);

    // action and arguments get written together, and only if they make sense together
    let write = {
        let (ctx, path, expander, args_row) = (
            ctx.clone(),
            path.clone(),
            expander.clone(),
            args_row.clone(),
        );
        Rc::new(
            move |name: &str| match actions::action_args(name, &args_row.text()) {
                Ok(parsed) => {
                    args_row.remove_css_class("error");
                    ctx.cfg
                        .borrow_mut()
                        .set_first_child(&refs(&path), name, &parsed);
                    expander.set_subtitle(&gtk::glib::markup_escape_text(&format!(
                        "{name} {}",
                        actions::action_text(name, &parsed)
                    )));
                    (ctx.changed)();
                }
                Err(why) => {
                    args_row.add_css_class("error");
                    (ctx.toast)(why);
                }
            },
        )
    };
    {
        let (write, args_row) = (write.clone(), args_row.clone());
        action_row.connect_selected_notify(move |r| {
            let Some((name, kind)) = ACTIONS.get(r.selected() as usize) else {
                return;
            };
            args_row.set_visible(*kind != ArgKind::None);
            if *kind == ArgKind::None || !args_row.text().is_empty() {
                write(*name);
            } else {
                args_row.add_css_class("error");
            }
        });
    }
    {
        let action_row = action_row.clone();
        args_row.connect_apply(move |_| {
            if let Some((name, _)) = ACTIONS.get(action_row.selected() as usize) {
                write(*name);
            }
        });
    }

    let remove_row = adw::ActionRow::new();
    remove_row.set_title("Remove This Shortcut");
    let remove = trash_button("Remove");
    {
        let (ctx, path, list) = (ctx.clone(), path.clone(), list.clone());
        remove.connect_clicked(move |_| {
            ctx.cfg.borrow_mut().remove(&refs(&path));
            (ctx.changed)();
            let (ctx, list) = (ctx.clone(), list.clone());
            later(move || fill_binds(&list, &ctx));
        });
    }
    remove_row.add_suffix(&remove);

    expander.add_row(&combo_row);
    expander.add_row(&action_row);
    expander.add_row(&args_row);
    expander.add_row(&remove_row);
    expander
}
