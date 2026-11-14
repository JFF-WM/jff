//! Saved container layouts and their file: one `name<TAB>layout` line each, bad lines skipped.

use std::collections::BTreeMap;

use super::session_store::{escape, state_file, unescape};
use crate::primitives::layout_template::Template;

/// Every saved layout by name.
pub type Layouts = BTreeMap<String, Template>;

/// The layouts as the text of the file, sorted by name.
pub fn format_layouts(layouts: &Layouts) -> String {
    let mut out = String::new();
    for (name, template) in layouts {
        out.push_str(&escape(name));
        out.push('\t');
        out.push_str(&template.to_text());
        out.push('\n');
    }
    out
}

pub fn parse_layouts(text: &str) -> Layouts {
    let mut layouts = Layouts::new();
    for line in text.lines() {
        let Some((name, body)) = line.split_once('\t') else {
            continue;
        };
        let name = unescape(name);
        if name.is_empty() {
            continue;
        }
        if let Ok(template) = Template::parse(body) {
            layouts.insert(name, template);
        }
    }
    layouts
}

pub fn load_layouts() -> Layouts {
    state_file("container-layouts")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_layouts(&text))
        .unwrap_or_default()
}

/// Write the layouts, saying why when it fails.
pub fn save_layouts(layouts: &Layouts) -> Result<(), String> {
    let path = state_file("container-layouts")
        .ok_or("there is no state directory to save the layout in (HOME is not set)")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, format_layouts(layouts))
        .map_err(|err| format!("could not write {}: {err}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .map_err(|err| format!("could not replace {}: {err}", path.display()))
}
