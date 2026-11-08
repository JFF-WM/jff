//! wp_drm_lease_device_v1: hands non-desktop connectors (VR headsets) to a client as a DRM
//! lease, so it can drive them directly. The device side (which connectors are leasable, which
//! CRTC and planes go with them) is in `app/tty`.

use smithay::backend::drm::DrmNode;
use smithay::wayland::drm_lease::{
    DrmLease, DrmLeaseBuilder, DrmLeaseHandler, DrmLeaseRequest, DrmLeaseState, LeaseRejected,
};

use crate::app::Backend;
use crate::state::JffState;

impl DrmLeaseHandler for JffState {
    fn drm_lease_state(&mut self, _node: DrmNode) -> &mut DrmLeaseState {
        // The global only exists once the tty backend created its lease state, and clients
        // can't reach this without that global.
        match &mut self.backend {
            Backend::Tty(tty) => tty
                .lease_state_mut()
                .expect("a lease request without a lease global"),
            Backend::Winit(_) => unreachable!("no DRM lease global on winit"),
        }
    }

    fn lease_request(
        &mut self,
        _node: DrmNode,
        request: DrmLeaseRequest,
    ) -> Result<DrmLeaseBuilder, LeaseRejected> {
        match &mut self.backend {
            Backend::Tty(tty) => tty.build_lease(&request.connectors),
            Backend::Winit(_) => Err(LeaseRejected::default()),
        }
    }

    fn new_active_lease(&mut self, _node: DrmNode, lease: DrmLease) {
        if let Backend::Tty(tty) = &mut self.backend {
            tty.add_active_lease(lease);
        }
    }

    fn lease_destroyed(&mut self, _node: DrmNode, lease: u32) {
        if let Backend::Tty(tty) = &mut self.backend {
            tty.remove_active_lease(lease);
        }
    }
}
