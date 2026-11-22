//! `container-template` and the `spawn-container` bind: a named group of apps that start
//! together in one tiling container.
//!
//! ```kdl
//! container-template "dev" {
//!     run "kitty"
//!     run "firefox" app-id="firefox"
//! }
//! binds { Super+D { spawn-container "dev", } }
//! ```
//!
//! The template's name is the container's name, and the name is what makes it persistent:
//!
//! * A container with that name exists, on any workspace or monitor: the bind shows its
//!   workspace and focuses it. Nothing is started. It works like a scratchpad for a whole
//!   group, and a second press never makes a second copy.
//! * Otherwise every `run` is started and noted as a *claim*: "the next window with this app
//!   id joins the container NAME, on the workspace and monitor the bind was pressed on". The
//!   first window to arrive starts the container, the others join it (`open_in_named_container`,
//!   the same code `open-in-container` uses). A claim that nobody takes up is dropped after
//!   [`CLAIM_TTL`]. While a launch is still on its way, pressing the bind again does nothing.
//!
//! The app id of a `run` is its `app-id=` or else the file name of its program. It is how a
//! window is told to be the one a `run` started, because a program often hands the work to a
//! process that already runs (a browser), so the process that was started is no use.
//!
//! `restore-apps` and a template must not both start an app. For every `run`:
//!
//! * an app that `restore-apps` already put back (its window is open, in no container) is
//!   taken into the container instead of being started again, and
//! * an app that `restore-apps` is starting right now (or `autostart` is, the restore knows
//!   both) is not started: the claim waits for its window.
//!
//! The arrangement of the container is remembered. It is saved as the container layout named
//! after the template (`jff msg layout-list`, `save-container-layout`) and kept up to date by
//! the same autosave a `container-rule` `layout` has: when you move a divider or put a window
//! somewhere else, the layout is saved again. The next `spawn-container` lays the windows out
//! that way as they join. The first time, when no layout of that name exists, the arrangement
//! the container has once every `run` has a window is saved as the first. A `container-rule`
//! `layout` that fits one of the windows wins over it.
//!
//! Needs `tiling-containers`.

use std::time::{Duration, Instant};

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::config::{RuleOutput, WindowRule};
use crate::desk::layout_store::load_layouts;
use crate::desk::ContainerId;
use crate::feature::osd::toplevel_names;
use crate::input::spawn_command;
use crate::state::JffState;

/// How long a started app has to show its window before the container stops waiting for it.
/// Generous: a cold browser on a slow disk takes its time.
const CLAIM_TTL: Duration = Duration::from_secs(45);

/// One window a template launch is still waiting for.
struct Claim {
    template: String,
    /// Lower case: app ids are compared without regard to case.
    app_id: String,
    since: Instant,
    /// Where the bind was pressed, and where the window goes, so a group never splits over
    /// the workspaces the user switched through while it started.
    workspace: usize,
    output: String,
}

/// The windows the templates that were started wait for, oldest first.
#[derive(Default)]
pub struct TemplateClaims {
    claims: Vec<Claim>,
}

impl TemplateClaims {
    fn expire(&mut self) {
        self.claims
            .retain(|claim| claim.since.elapsed() < CLAIM_TTL);
    }

    fn add(&mut self, template: &str, app_id: &str, workspace: usize, output: &str) {
        self.claims.push(Claim {
            template: template.to_owned(),
            app_id: app_id.to_ascii_lowercase(),
            since: Instant::now(),
            workspace,
            output: output.to_owned(),
        });
    }

    /// Whether a launch of `template` still waits for a window.
    fn waiting_for(&mut self, template: &str) -> bool {
        self.expire();
        self.claims.iter().any(|claim| claim.template == template)
    }

    /// The oldest claim for a window with this (lower case) app id, taken off the list.
    fn take(&mut self, app_id: &str) -> Option<Claim> {
        self.expire();
        let index = self
            .claims
            .iter()
            .position(|claim| claim.app_id == app_id)?;
        Some(self.claims.remove(index))
    }
}

/// Where a template's windows go: the workspace and monitor the bind was pressed on.
fn target_rule(workspace: usize, output: &str) -> WindowRule {
    WindowRule {
        output: (!output.is_empty()).then(|| RuleOutput::Named(output.to_owned())),
        workspace: Some(workspace),
        ..WindowRule::default()
    }
}

impl JffState {
    /// The `spawn-container "NAME"` bind. See the module documentation.
    pub fn spawn_container(&mut self, name: &str) {
        if !self.config.tiling_containers() {
            self.show_error(
                "Containers are off",
                "`spawn-container` needs `tiling-containers` in the config.",
            );
            return;
        }
        let Some(template) = self.config.container_template(name).cloned() else {
            self.show_error(
                "No such template",
                &format!("There is no container-template named \"{name}\"."),
            );
            return;
        };

        // The name is the container's identity: there is one already, so show that one.
        if self.focus_template_container(name) {
            tracing::info!("spawn-container {name:?}: the container exists, focused it");
            return;
        }
        // The apps are on their way and the container is not there yet.
        if self.template_claims.waiting_for(name) {
            tracing::info!("spawn-container {name:?}: still starting, nothing to do");
            return;
        }

        let workspace = self.current_workspace();
        let output = self.active_output_name();

        // Windows `restore-apps` put back and that are in no container yet, as (window,
        // lower case app id). A `run` that finds its app here takes the window in.
        let mut free: Vec<(Window, String)> = Vec::new();
        for root in self.app_restore_placed() {
            let Some(entry) = self.desk.all_top_down().find(|entry| {
                entry
                    .window
                    .toplevel()
                    .is_some_and(|toplevel| *toplevel.wl_surface() == root)
            }) else {
                continue;
            };
            let dialog = self.dialog_toplevels.contains(&root)
                || entry
                    .window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.parent().is_some());
            if entry.container.is_some() || dialog {
                continue;
            }
            let app_id = toplevel_names(&root).0.trim().to_ascii_lowercase();
            free.push((entry.window.clone(), app_id));
        }

        let mut adopt: Vec<Window> = Vec::new();
        let mut start: Vec<usize> = Vec::new();
        for (index, run) in template.runs.iter().enumerate() {
            let app_id = run.app_id.to_ascii_lowercase();
            if let Some(position) = free.iter().position(|(_, id)| *id == app_id) {
                adopt.push(free.remove(position).0);
            } else {
                start.push(index);
            }
        }

        // Windows that are open already go first: they make the container, and what is
        // started below finds it when it shows.
        let target = target_rule(workspace, &output);
        for window in &adopt {
            if !self.open_in_named_container(window, &target, name) {
                tracing::warn!("spawn-container {name:?}: could not take a restored window in");
            }
        }

        for index in start {
            let run = &template.runs[index];
            self.template_claims
                .add(name, &run.app_id, workspace, &output);
            if self.app_restore_expects(&run.app_id) {
                tracing::info!(
                    "spawn-container {name:?}: restore-apps is starting {:?}, not started twice",
                    run.app_id
                );
                continue;
            }
            if let Some((program, args)) = run.argv.split_first() {
                tracing::info!("spawn-container {name:?}: starting {program}");
                spawn_command(program, args);
            }
        }
    }

    /// A window shows for the first time: when a template launch waits for a window with this
    /// app id, the rule that sends it to the container (the workspace and monitor the bind
    /// was pressed on) and the container's name. For `open_in_named_container`.
    pub(crate) fn template_claim(&mut self, root: &WlSurface) -> Option<(WindowRule, String)> {
        if !self.config.tiling_containers() {
            return None;
        }
        let app_id = toplevel_names(root).0.trim().to_ascii_lowercase();
        if app_id.is_empty() {
            return None;
        }
        let claim = self.template_claims.take(&app_id)?;
        // A reload may have taken the template away while its apps started.
        self.config.container_template(&claim.template)?;
        tracing::info!(
            "container-template {:?}: a window with app id {app_id:?} came",
            claim.template
        );
        Some((target_rule(claim.workspace, &claim.output), claim.template))
    }

    /// The saved layout a window that joined container `id` is laid out as: the one named like
    /// the container, when that is a `container-template` and the layout exists.
    pub(crate) fn template_layout_name(&self, id: ContainerId) -> Option<String> {
        let name = self.desk.container(id)?.name()?.to_owned();
        self.config.container_template(&name)?;
        load_layouts().contains_key(&name).then_some(name)
    }

    /// A window has joined (or started) container `id`, called `name`. When that is a
    /// `container-template` and every one of its `run`s has a window now, the layout named
    /// like the template starts being kept up to date (`layout_autosave`): it is applied when
    /// it exists, and saved from the arrangement the container has when it does not.
    pub(crate) fn template_layout_sync(&mut self, id: ContainerId, name: &str) {
        let Some(wanted) = self
            .config
            .container_template(name)
            .map(|template| template.runs.len())
        else {
            return;
        };
        // Autosave is on already: a layout was applied as the windows joined.
        if self.layout_autosave.contains_key(&id) {
            return;
        }
        let members: Vec<Window> = self
            .desk
            .members(id)
            .iter()
            .map(|entry| entry.window.clone())
            .collect();
        let Some(first) = members.first() else {
            return;
        };
        if members.len() < wanted {
            return;
        }
        if load_layouts().contains_key(name) {
            match self.apply_layout_to(id, name) {
                Ok(_) => {
                    self.retile_container(id);
                    self.layout_autosave.insert(id, name.to_owned());
                }
                Err(why) => {
                    tracing::warn!("container-template {name:?}: layout not applied: {why}")
                }
            }
            return;
        }
        match self.save_container_layout(name, first) {
            Ok(_) => {
                self.layout_autosave.insert(id, name.to_owned());
            }
            Err(why) => tracing::warn!("container-template {name:?}: layout not saved: {why}"),
        }
    }

    /// Show the workspace of the container called `name`, on whatever monitor it is on, and
    /// give it the keyboard. False when there is no such container.
    fn focus_template_container(&mut self, name: &str) -> bool {
        let found = self
            .desk
            .containers()
            .find(|container| container.name() == Some(name))
            .map(|container| {
                (
                    container.id(),
                    container.workspace(),
                    container.output().to_owned(),
                )
            });
        let Some((id, workspace, output)) = found else {
            return false;
        };
        let output = if output.is_empty() {
            self.active_output_name()
        } else {
            output
        };
        // Does nothing when the workspace is shown already.
        self.switch_workspace_on(&output, workspace);
        self.select_container(id)
    }
}
