//! Tests for `jff_wm::primitives::hdr_png`.

use jff_wm::primitives::hdr_png::{encode, encode_rgba, CICP_PQ_BT2020};

fn has_chunk(bytes: &[u8], name: &[u8; 4], data: &[u8]) -> bool {
    bytes
        .windows(4 + data.len())
        .any(|w| &w[..4] == name && &w[4..] == data)
}

#[test]
fn a_picture_is_16_bit_rgb_and_keeps_its_codes() {
    let pq = [0.0, 0.5, 1.0, 0.25, 0.75, 2.0];
    let bytes = encode(&pq, 2, 1, None).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .unwrap();
    let info = reader.info();
    assert_eq!((info.width, info.height), (2, 1));
    assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
    assert_eq!(info.color_type, png::ColorType::Rgb);
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    let codes: Vec<u16> = buf
        .chunks(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect();
    // a value above 1 is clamped
    assert_eq!(codes, [0, 32768, 65535, 16384, 49151, 65535]);
}

#[test]
fn the_picture_says_it_is_pq_in_bt2020() {
    let bytes = encode(&[0.1; 3], 1, 1, None).unwrap();
    assert!(has_chunk(&bytes, b"cICP", &CICP_PQ_BT2020));
    assert!(!bytes.windows(4).any(|w| w == b"cLLI"));
}

#[test]
fn the_content_light_level_goes_in_units_of_a_ten_thousandth() {
    let bytes = encode(&[0.1; 3], 1, 1, Some(1000.0)).unwrap();
    let mut level = 10_000_000u32.to_be_bytes().to_vec();
    level.extend_from_slice(&[0, 0, 0, 0]);
    assert!(has_chunk(&bytes, b"cLLI", &level));
}

#[test]
fn the_wrong_amount_of_pixels_is_refused() {
    assert!(encode(&[0.0; 5], 1, 1, None).is_err());
    assert!(encode(&[], 0, 0, None).is_err());
}

#[test]
fn a_picture_with_alpha_is_16_bit_rgba() {
    let bytes = encode_rgba(&[0.5, 0.25, 1.0, 0.5], 1, 1, None).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .unwrap();
    assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
    assert_eq!(reader.info().color_type, png::ColorType::Rgba);
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    let codes: Vec<u16> = buf
        .chunks(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(codes, [32768, 16384, 65535, 32768]);
    assert!(has_chunk(&bytes, b"cICP", &CICP_PQ_BT2020));
}

#[test]
fn a_picture_with_alpha_needs_four_values_a_pixel() {
    assert!(encode_rgba(&[0.0; 3], 1, 1, None).is_err());
    assert!(encode_rgba(&[], 0, 0, None).is_err());
}
