//! A 16-bit PNG of an HDR picture: PQ in BT.2020, tagged with a `cICP` chunk so viewers know it.

use super::hdr_output::PQ_PEAK;

/// `cICP` of PQ in BT.2020: primaries 9, transfer 16, RGB matrix 0, full range.
pub const CICP_PQ_BT2020: [u8; 4] = [9, 16, 0, 1];

/// PQ code values (0 to 1, R G B per pixel, top row first) as a 16-bit RGB PNG. `max_cll` (cd/m²) goes into a `cLLI` chunk when given, with the frame average left unknown.
pub fn encode(
    pq: &[f32],
    width: u32,
    height: u32,
    max_cll: Option<f32>,
) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * 3;
    if width == 0 || height == 0 || pq.len() != expected {
        return Err(format!(
            "a {width}x{height} picture needs {expected} values, got {}",
            pq.len()
        ));
    }
    let mut data = Vec::with_capacity(expected * 2);
    for value in pq {
        let code = (value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16;
        data.extend_from_slice(&code.to_be_bytes());
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder
            .write_header()
            .map_err(|err| format!("PNG header: {err}"))?;
        writer
            .write_chunk(png::chunk::cICP, &CICP_PQ_BT2020)
            .map_err(|err| format!("PNG cICP: {err}"))?;
        if let Some(level) = max_cll {
            // units of 0.0001 cd/m², max frame average light level unknown (0)
            let units = (level.clamp(0.0, PQ_PEAK) * 10_000.0).round() as u32;
            let mut chunk = units.to_be_bytes().to_vec();
            chunk.extend_from_slice(&0u32.to_be_bytes());
            writer
                .write_chunk(png::chunk::cLLI, &chunk)
                .map_err(|err| format!("PNG cLLI: {err}"))?;
        }
        writer
            .write_image_data(&data)
            .map_err(|err| format!("PNG data: {err}"))?;
        writer.finish().map_err(|err| format!("PNG end: {err}"))?;
    }
    Ok(out)
}
