//! What the colors of a capture are, for `xx-image-capture-color-management-v1`.
//!
//! A capture (`protocol/image_copy.rs`) is a render like a screenshot's: the 8-bit formats hold
//! plain sRGB, and the 10-bit formats of a monitor in HDR mode hold what the monitor is sent,
//! PQ in BT.2020 (see `primitives/hdr_capture.rs`). So that is the preferred description of a
//! source, and the only descriptions a frame can be tagged with: nothing converts a picture to
//! another one, a frame tagged with something else fails instead of being mislabeled.

use super::color::{ImageDescription, NamedPrimaries, NamedTf, TransferFunction};
use super::gamut::is_plain_srgb;

/// What a capture source's picture holds when drawn into a buffer that `hdr_picture` says is
/// an HDR one (a 10-bit format of a monitor in HDR mode): `hdr` is the description of that
/// monitor, `None` for any other source (and for a monitor not in HDR mode).
pub fn preferred(hdr: Option<ImageDescription>) -> ImageDescription {
    hdr.filter(is_pq_bt2020).unwrap_or_else(ImageDescription::srgb)
}

/// Whether the picture of a frame can be said to be `description`. `hdr_picture`: the frame is
/// drawn as PQ in BT.2020 (10-bit buffer, monitor in HDR mode), else it is plain sRGB. Luminance
/// metadata is not compared: PQ is absolute light whatever it says, and sRGB's is relative.
pub fn frame_description_fits(description: &ImageDescription, hdr_picture: bool) -> bool {
    if hdr_picture {
        is_pq_bt2020(description)
    } else {
        is_plain_srgb(description)
    }
}

fn is_pq_bt2020(description: &ImageDescription) -> bool {
    description.primaries == NamedPrimaries::Bt2020.chromaticities()
        && description.tf == TransferFunction::Named(NamedTf::St2084Pq)
}
