//! Builds the libadwaita rows from the data in pages.rs and hooks them up to the config.

use crate::pages::{Field, Group, Kind};
use crate::{lists, Ctx};
use adw::prelude::*;
use gtk4 as gtk;
use jff_settings_core::{modes, Arg, JffConfig};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Fired when the row that gates a group changes.
pub type Gate = Rc<dyn Fn(bool)>;

pub fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

// config access

fn read_arg(cfg: &JffConfig, path: &[String], prop: Option<&str>) -> Option<Arg> {
    let p = refs(path);
    match prop {
        Some(key) => cfg.get_prop(&p, key),
        None => cfg.args(&p).into_iter().next(),
    }
}

fn write_arg(cfg: &mut JffConfig, path: &[String], prop: Option<&str>, value: Arg) {
    let p = refs(path);
    match prop {
        Some(key) => cfg.set_prop(&p, key, value),
        None => cfg.set_args(&p, &[value]),
    }
}

fn clear(cfg: &mut JffConfig, path: &[String], prop: Option<&str>) {
    let p = refs(path);
    match prop {
        Some(key) => cfg.remove_prop(&p, key),
        None => cfg.remove(&p),
    }
}

fn read_int(
    cfg: &JffConfig,
    path: &[String],
    prop: Option<&str>,
    positional: bool,
    default: i64,
    block_default: Option<i64>,
) -> i64 {
    let p = refs(path);
    let value = match prop {
        Some(key) => match cfg.get_prop(&p, key) {
            Some(Arg::Int(i)) => Some(i),
            _ => None,
        },
        None if positional => cfg.get_int_or_positional(&p),
        None => cfg.get_int(&p),
    };
    value.unwrap_or_else(|| {
        let parent = &p[..p.len().saturating_sub(1)];
        match block_default {
            Some(b) if !parent.is_empty() && cfg.has(parent) => b,
            _ => default,
        }
    })
}

fn write_int(
    cfg: &mut JffConfig,
    path: &[String],
    prop: Option<&str>,
    positional: bool,
    value: i64,
) {
    let p = refs(path);
    match prop {
        Some(key) => cfg.set_prop(&p, key, Arg::Int(value)),
        None if positional => cfg.set_int_or_positional(&p, value),
        None => cfg.set_int(&p, value),
    }
}

fn off_path<'a>(block: &[&'a str]) -> Vec<&'a str> {
    let mut path = block.to_vec();
    path.push("off");
    path
}

/// A block is on if it exists (or doesn't need to) and has no `off` inside.
fn enable_get(cfg: &JffConfig, block: &[&str], needs_block: bool) -> bool {
    let base = if cfg.has(block) {
        cfg.flag(block, true)
    } else {
        !needs_block
    };
    base && !cfg.flag(&off_path(block), false)
}

fn enable_set(cfg: &mut JffConfig, block: &[&str], needs_block: bool, on: bool) {
    let off = off_path(block);
    if on {
        if needs_block || cfg.has(block) {
            cfg.set_flag(block, true);
        }
        cfg.remove(&off);
    } else if cfg.has(block) || !needs_block {
        cfg.set_flag(&off, true);
    }
}

fn to_hex(c: &gtk::gdk::RGBA) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(c.red()),
        byte(c.green()),
        byte(c.blue()),
        byte(c.alpha())
    )
}

// builders

/// A free text row. Empty text removes the key.
fn text_row(title: &str, path: Vec<String>, prop: Option<&'static str>, ctx: &Ctx) -> gtk::Widget {
    let row = adw::EntryRow::new();
    row.set_title(title);
    row.set_show_apply_button(true);
    let current = read_arg(&ctx.cfg.borrow(), &path, prop);
    row.set_text(current.as_ref().and_then(Arg::as_str).unwrap_or(""));
    let ctx = ctx.clone();
    row.connect_apply(move |r| {
        let text = r.text();
        let mut cfg = ctx.cfg.borrow_mut();
        if text.is_empty() {
            clear(&mut cfg, &path, prop);
        } else {
            write_arg(&mut cfg, &path, prop, Arg::Str(text.to_string()));
        }
        drop(cfg);
        (ctx.changed)();
    });
    row.upcast()
}

/// The mode of the display `root` points at, picked from what `wlr-randr` lists for it. The
/// first entry is Automatic (no `mode` line). A mode in the file that the display doesn't
/// offer stays in the list, so opening the window never changes the file. Without a list
/// (no wlr-randr, display not connected) it is a text entry.
fn mode_row(
    field: &Field,
    root: &[String],
    path: Vec<String>,
    prop: Option<&'static str>,
    ctx: &Ctx,
) -> gtk::Widget {
    let name = match ctx.cfg.borrow().args(&refs(root)).first() {
        Some(Arg::Str(name)) => name.clone(),
        _ => String::new(),
    };
    let modes = modes::for_output(&name);
    if modes.is_empty() {
        return text_row("Mode, for example 1920x1080@60", path, prop, ctx);
    }
    let current = read_arg(&ctx.cfg.borrow(), &path, prop)
        .and_then(|arg| arg.as_str().map(str::to_owned))
        .filter(|value| !value.is_empty());

    // (value in the file, label); `None` is Automatic.
    let mut items: Vec<(Option<String>, String)> = vec![(None, "Automatic".to_owned())];
    items.extend(modes.iter().map(|m| (Some(m.value()), m.label())));
    let selected = match &current {
        None => 0,
        Some(value) => match items.iter().position(|(v, _)| v.as_deref() == Some(value)) {
            Some(i) => i,
            None => {
                items.push((Some(value.clone()), format!("{value} (not offered)")));
                items.len() - 1
            }
        },
    };

    let row = adw::ComboRow::new();
    titled(&row, field);
    let labels: Vec<&str> = items.iter().map(|(_, label)| label.as_str()).collect();
    row.set_model(Some(&gtk::StringList::new(&labels)));
    row.set_expression(Some(&gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    row.set_enable_search(true);
    row.set_selected(selected as u32);
    let ctx = ctx.clone();
    row.connect_selected_notify(move |r| {
        let Some((value, _)) = items.get(r.selected() as usize) else {
            return;
        };
        let mut cfg = ctx.cfg.borrow_mut();
        match value {
            None => clear(&mut cfg, &path, prop),
            Some(value) => write_arg(&mut cfg, &path, prop, Arg::Str(value.clone())),
        }
        drop(cfg);
        (ctx.changed)();
    });
    row.upcast()
}

/// Adds one PreferencesGroup per group to the page.
pub fn build_groups(page: &adw::PreferencesPage, groups: &[Group], root: &[String], ctx: &Ctx) {
    for group in groups {
        let g = adw::PreferencesGroup::new();
        g.set_title(&gtk::glib::markup_escape_text(group.title));
        if !group.description.is_empty() {
            g.set_description(Some(
                gtk::glib::markup_escape_text(group.description).as_str(),
            ));
        }
        let gated: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
        let gate: Option<Gate> = group.gated.then(|| {
            let gated = gated.clone();
            Rc::new(move |on: bool| {
                for widget in gated.borrow().iter() {
                    widget.set_sensitive(on);
                }
            }) as Gate
        });
        let mut initial = true;
        for (i, field) in group.fields.iter().enumerate() {
            let (row, on) = build_row(field, root, ctx, if i == 0 { gate.clone() } else { None });
            g.add(&row);
            if group.gated {
                if i == 0 {
                    initial = on;
                } else {
                    gated.borrow_mut().push(row);
                }
            }
        }
        if !group.fields.is_empty() {
            page.add(&g);
        }
        for spec in &group.lists {
            let widget = lists::text_list(spec, ctx);
            page.add(&widget);
            if group.gated {
                gated.borrow_mut().push(widget.upcast());
            }
        }
        for widget in gated.borrow().iter() {
            widget.set_sensitive(initial);
        }
    }
}

/// Titles and subtitles are plain text, so no markup parsing.
fn titled(row: &(impl IsA<adw::PreferencesRow> + IsA<adw::ActionRow>), field: &Field) {
    row.set_title(field.title);
    row.set_subtitle(field.subtitle);
    row.set_use_markup(false);
}

fn clear_button(tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name("edit-undo-symbolic");
    button.set_valign(gtk::Align::Center);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    button
}

/// One row per setting. The bool tells a gated group if the row is on (switch) or above 0 (number).
pub fn build_row(
    field: &Field,
    root: &[String],
    ctx: &Ctx,
    gate: Option<Gate>,
) -> (gtk::Widget, bool) {
    let path: Vec<String> = root
        .iter()
        .cloned()
        .chain(field.path.iter().map(|s| (*s).to_owned()))
        .collect();
    let prop = field.prop;

    match &field.kind {
        Kind::Int {
            min,
            max,
            default,
            block_default,
            optional,
            positional,
        } => {
            let (default, block_default, positional) = (*default, *block_default, *positional);
            let row = adw::SpinRow::with_range(*min as f64, *max as f64, 1.0);
            row.set_numeric(true);
            titled(&row, field);
            let initial = read_int(
                &ctx.cfg.borrow(),
                &path,
                prop,
                positional,
                default,
                block_default,
            );
            row.set_value(initial as f64);
            let silent = Rc::new(Cell::new(false));
            if *optional {
                let button = clear_button("Reset to Default");
                row.add_suffix(&button);
                let (ctx, path, row, silent) =
                    (ctx.clone(), path.clone(), row.clone(), silent.clone());
                button.connect_clicked(move |_| {
                    clear(&mut ctx.cfg.borrow_mut(), &path, prop);
                    silent.set(true);
                    row.set_value(default as f64);
                    silent.set(false);
                    (ctx.changed)();
                });
            }
            let ctx = ctx.clone();
            row.connect_value_notify(move |r| {
                if silent.get() {
                    return;
                }
                let value = r.value().round() as i64;
                write_int(&mut ctx.cfg.borrow_mut(), &path, prop, positional, value);
                (ctx.changed)();
                if let Some(gate) = &gate {
                    gate(value > 0);
                }
            });
            (row.upcast(), initial > 0)
        }
        Kind::Float {
            min,
            max,
            step,
            default,
        } => {
            let row = adw::SpinRow::with_range(*min, *max, *step);
            row.set_numeric(true);
            titled(&row, field);
            row.set_digits(1);
            let current = ctx.cfg.borrow().get_float(&refs(&path)).unwrap_or(*default);
            row.set_value(current);
            let ctx = ctx.clone();
            row.connect_value_notify(move |r| {
                ctx.cfg.borrow_mut().set_float(&refs(&path), r.value());
                (ctx.changed)();
            });
            (row.upcast(), true)
        }
        Kind::Flag { default } => {
            let row = adw::SwitchRow::new();
            titled(&row, field);
            let active = ctx.cfg.borrow().flag(&refs(&path), *default);
            row.set_active(active);
            let ctx = ctx.clone();
            row.connect_active_notify(move |r| {
                ctx.cfg.borrow_mut().set_flag(&refs(&path), r.is_active());
                (ctx.changed)();
                if let Some(gate) = &gate {
                    gate(r.is_active());
                }
            });
            (row.upcast(), active)
        }
        Kind::Presence => {
            let row = adw::SwitchRow::new();
            titled(&row, field);
            row.set_active(ctx.cfg.borrow().has(&refs(&path)));
            let ctx = ctx.clone();
            row.connect_active_notify(move |r| {
                ctx.cfg
                    .borrow_mut()
                    .set_presence(&refs(&path), r.is_active());
                (ctx.changed)();
            });
            (row.upcast(), true)
        }
        Kind::Enable { needs_block } => {
            let needs_block = *needs_block;
            let row = adw::SwitchRow::new();
            titled(&row, field);
            let active = enable_get(&ctx.cfg.borrow(), &refs(&path), needs_block);
            row.set_active(active);
            let ctx = ctx.clone();
            row.connect_active_notify(move |r| {
                enable_set(
                    &mut ctx.cfg.borrow_mut(),
                    &refs(&path),
                    needs_block,
                    r.is_active(),
                );
                (ctx.changed)();
                if let Some(gate) = &gate {
                    gate(r.is_active());
                }
            });
            (row.upcast(), active)
        }
        Kind::Tri { unset } => {
            let row = adw::ComboRow::new();
            titled(&row, field);
            row.set_model(Some(&gtk::StringList::new(&[*unset, "Yes", "No"])));
            let selected = {
                let cfg = ctx.cfg.borrow();
                let p = refs(&path);
                let value = match prop {
                    Some(key) => match cfg.get_prop(&p, key) {
                        Some(Arg::Bool(b)) => Some(b),
                        Some(Arg::Str(s)) => Some(matches!(s.as_str(), "true" | "yes")),
                        _ => None,
                    },
                    None => cfg.has(&p).then(|| cfg.flag(&p, true)),
                };
                match value {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                }
            };
            row.set_selected(selected);
            let ctx = ctx.clone();
            row.connect_selected_notify(move |r| {
                let mut cfg = ctx.cfg.borrow_mut();
                match (r.selected(), prop) {
                    (0, _) => clear(&mut cfg, &path, prop),
                    (sel, Some(_)) => write_arg(&mut cfg, &path, prop, Arg::Bool(sel == 1)),
                    (sel, None) => cfg.set_flag(&refs(&path), sel == 1),
                }
                drop(cfg);
                (ctx.changed)();
            });
            (row.upcast(), true)
        }
        Kind::Choice { options, default } => {
            let options: &'static [(&'static str, &'static str)] = *options;
            let row = adw::ComboRow::new();
            titled(&row, field);
            let labels: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
            row.set_model(Some(&gtk::StringList::new(&labels)));
            let current = ctx
                .cfg
                .borrow()
                .get_str(&refs(&path))
                .unwrap_or_else(|| (*default).to_owned());
            if let Some(i) = options
                .iter()
                .position(|(value, _)| value.eq_ignore_ascii_case(&current))
            {
                row.set_selected(i as u32);
            }
            let ctx = ctx.clone();
            row.connect_selected_notify(move |r| {
                if let Some((value, _)) = options.get(r.selected() as usize) {
                    ctx.cfg.borrow_mut().set_str(&refs(&path), value);
                    (ctx.changed)();
                }
            });
            (row.upcast(), true)
        }
        Kind::Text => (text_row(field.title, path, prop, ctx), true),
        Kind::Mode => (mode_row(field, root, path, prop, ctx), true),
        Kind::Color { default, optional } => {
            let default: &'static str = *default;
            let fallback = || gtk::gdk::RGBA::parse(default).unwrap_or(gtk::gdk::RGBA::BLACK);
            let row = adw::ActionRow::new();
            titled(&row, field);
            let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
            button.set_valign(gtk::Align::Center);
            let current = ctx.cfg.borrow().get_str(&refs(&path));
            button.set_rgba(
                &current
                    .and_then(|c| gtk::gdk::RGBA::parse(c).ok())
                    .unwrap_or_else(fallback),
            );
            let silent = Rc::new(Cell::new(false));
            if *optional {
                let clear_it = clear_button("Reset to Default");
                let (ctx, path, button, silent) =
                    (ctx.clone(), path.clone(), button.clone(), silent.clone());
                clear_it.connect_clicked(move |_| {
                    clear(&mut ctx.cfg.borrow_mut(), &path, prop);
                    silent.set(true);
                    button
                        .set_rgba(&gtk::gdk::RGBA::parse(default).unwrap_or(gtk::gdk::RGBA::BLACK));
                    silent.set(false);
                    (ctx.changed)();
                });
                row.add_suffix(&clear_it);
            }
            row.add_suffix(&button);
            let ctx = ctx.clone();
            button.connect_rgba_notify(move |b| {
                if silent.get() {
                    return;
                }
                ctx.cfg
                    .borrow_mut()
                    .set_str(&refs(&path), &to_hex(&b.rgba()));
                (ctx.changed)();
            });
            (row.upcast(), true)
        }
        Kind::Pair { min, max, default } => {
            let default = *default;
            let row = adw::ActionRow::new();
            titled(&row, field);
            let spin = |value: i64| {
                let s = gtk::SpinButton::with_range(*min as f64, *max as f64, 1.0);
                s.set_valign(gtk::Align::Center);
                s.set_value(value as f64);
                s
            };
            let (x, y) = match ctx.cfg.borrow().args(&refs(&path)).as_slice() {
                [Arg::Int(x), Arg::Int(y)] => (*x, *y),
                _ => default,
            };
            let (first, second) = (spin(x), spin(y));
            let silent = Rc::new(Cell::new(false));
            let clear_it = clear_button("Reset to Default");
            {
                let (ctx, path, first, second, silent) = (
                    ctx.clone(),
                    path.clone(),
                    first.clone(),
                    second.clone(),
                    silent.clone(),
                );
                clear_it.connect_clicked(move |_| {
                    ctx.cfg.borrow_mut().remove(&refs(&path));
                    silent.set(true);
                    first.set_value(default.0 as f64);
                    second.set_value(default.1 as f64);
                    silent.set(false);
                    (ctx.changed)();
                });
            }
            row.add_suffix(&clear_it);
            row.add_suffix(&first);
            row.add_suffix(&second);
            let write = {
                let (ctx, first, second) = (ctx.clone(), first.clone(), second.clone());
                Rc::new(move || {
                    if silent.get() {
                        return;
                    }
                    let pair = [
                        Arg::Int(first.value().round() as i64),
                        Arg::Int(second.value().round() as i64),
                    ];
                    ctx.cfg.borrow_mut().set_args(&refs(&path), &pair);
                    (ctx.changed)();
                })
            };
            let w = write.clone();
            first.connect_value_changed(move |_| w());
            second.connect_value_changed(move |_| write());
            (row.upcast(), true)
        }
    }
}
