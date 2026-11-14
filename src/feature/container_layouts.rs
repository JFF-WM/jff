//! Saving a tiling container's layout and putting it back, tagged windows going to their slot.

use smithay::desktop::Window;

use crate::desk::layout_store::{load_layouts, save_layouts, Layouts};
use crate::desk::{ContainerError, ContainerId, LayoutApplied};
use crate::primitives::layout_template::Template;
use crate::protocol::toplevel_tag;
use crate::state::JffState;

/// The tag rules and layouts see: the user's, else the app's.
pub(crate) fn window_tag(window: &Window) -> Option<String> {
    window
        .toplevel()
        .and_then(|toplevel| toplevel_tag::tag_of(toplevel.wl_surface()))
}

fn container_error(err: ContainerError) -> String {
    match err {
        ContainerError::NoSuchContainer => "the container is gone".to_owned(),
        other => format!("could not lay the container out: {other:?}"),
    }
}

/// A layout name: not empty, one line.
fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a layout needs a name".to_owned());
    }
    if name.contains(['\n', '\r']) {
        return Err("a layout name is one line".to_owned());
    }
    Ok(name)
}

impl JffState {
    /// Saves the layout of `window`'s container under `name`, replacing one with that name.
    pub fn save_container_layout(
        &mut self,
        name: &str,
        window: &Window,
    ) -> Result<Template, String> {
        let name = check_name(name)?;
        let id = self
            .desk
            .container_of(|w| w == window)
            .ok_or("the window is not in a tiling container")?;
        let container = self.desk.container(id).ok_or("the container is gone")?;
        let template = Template::from_tree(container.tree(), |leaf| {
            self.desk
                .find_by_id(leaf)
                .and_then(|entry| window_tag(&entry.window))
        })
        .ok_or("the container has no windows")?;

        let mut layouts = load_layouts();
        layouts.insert(name.to_owned(), template.clone());
        save_layouts(&layouts)?;
        tracing::info!("container {} saved as layout {name:?}", id.get());
        Ok(template)
    }

    /// A container that a `container-rule` `layout` laid out is saved under that name again
    /// after the user changed it: a divider moved, a window joined or was put somewhere else.
    /// Nothing is written for a container that was not laid out by a rule, whose layout is gone,
    /// or whose shape did not change.
    ///
    /// A window that closes does not save, and neither does a container that is not all there:
    /// a layout with a tagged slot that no window of the container fills is left as it is, so
    /// the layout is not cut down to the docks that happened to be open (while an app is still
    /// opening them, or after some were closed). A layout with no tagged slot is kept while the
    /// container has fewer windows than it has slots. `save-container-layout` saves it as it is.
    pub(crate) fn autosave_container_layout(&mut self, id: ContainerId) {
        let Some(name) = self.layout_autosave.get(&id).cloned() else {
            return;
        };
        let Some(container) = self.desk.container(id) else {
            self.layout_autosave.remove(&id);
            return;
        };
        let mut layouts = load_layouts();
        let Some(saved) = layouts.get(&name) else {
            // Deleted by the user: it is not brought back.
            self.layout_autosave.remove(&id);
            return;
        };
        let tags: Vec<Option<String>> = self
            .desk
            .members(id)
            .iter()
            .map(|entry| window_tag(&entry.window))
            .collect();
        let slots = saved.slots();
        let tagged: Vec<&str> = slots.iter().filter_map(|slot| *slot).collect();
        let complete = if tagged.is_empty() {
            tags.len() >= slots.len()
        } else {
            tagged
                .iter()
                .all(|wanted| tags.iter().any(|tag| tag.as_deref() == Some(*wanted)))
        };
        if !complete {
            return;
        }
        let Some(template) = Template::from_tree(container.tree(), |leaf| {
            self.desk
                .find_by_id(leaf)
                .and_then(|entry| window_tag(&entry.window))
        }) else {
            return;
        };
        if template == *saved {
            return;
        }
        layouts.insert(name.clone(), template);
        match save_layouts(&layouts) {
            Ok(()) => tracing::info!("container {} saved again as layout {name:?}", id.get()),
            Err(why) => tracing::warn!("layout {name:?} not saved again: {why}"),
        }
    }

    /// Lays out `window`'s container as the layout `name` says, then retiles it.
    pub fn apply_container_layout(
        &mut self,
        name: &str,
        window: &Window,
    ) -> Result<LayoutApplied, String> {
        let id = self
            .desk
            .container_of(|w| w == window)
            .ok_or("the window is not in a tiling container")?;
        let applied = self.apply_layout_to(id, name)?;
        self.retile_container(id);
        tracing::info!(
            "container {} laid out as {name:?}: {} slots, {} empty, {} extra windows",
            id.get(),
            applied.slots,
            applied.empty,
            applied.extra
        );
        Ok(applied)
    }

    /// Rebuilds the tree of container `id` from the saved layout `name`, the caller retiles.
    pub(crate) fn apply_layout_to(
        &mut self,
        id: ContainerId,
        name: &str,
    ) -> Result<LayoutApplied, String> {
        let name = check_name(name)?;
        let template = load_layouts()
            .remove(name)
            .ok_or_else(|| format!("there is no saved layout named \"{name}\""))?;
        let gap = self.config.snap_gap();
        self.desk
            .apply_container_layout(id, &template, window_tag, gap)
            .map_err(container_error)
    }

    /// Forgets the saved layout `name`.
    pub fn delete_container_layout(&mut self, name: &str) -> Result<(), String> {
        let name = check_name(name)?;
        let mut layouts: Layouts = load_layouts();
        if layouts.remove(name).is_none() {
            return Err(format!("there is no saved layout named \"{name}\""));
        }
        save_layouts(&layouts)
    }

    /// The `save-container-layout` and `apply-container-layout` binds, errors as a toast.
    pub fn container_layout_bind(&mut self, name: &str, save: bool) {
        let Some(window) = self.focused_window() else {
            self.show_error("Container layout", "No window has the keyboard.");
            return;
        };
        let result = if save {
            self.save_container_layout(name, &window).map(|_| ())
        } else {
            self.apply_container_layout(name, &window).map(|_| ())
        };
        if let Err(why) = result {
            self.show_error("Container layout", &why);
        }
    }
}
