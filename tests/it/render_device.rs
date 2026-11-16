//! `render-device`: the GPU jff opens.

use jff_wm::config::Config;

#[test]
fn render_device_is_off_by_default() {
    assert_eq!(Config::parse("").unwrap().render_device(), None);
}

#[test]
fn render_device_names_a_gpu() {
    let config = Config::parse(r#"render-device "/dev/dri/card1""#).unwrap();
    assert_eq!(config.render_device(), Some("/dev/dri/card1"));
}

#[test]
fn render_device_needs_a_path() {
    assert!(Config::parse("render-device").is_err());
    assert!(Config::parse(r#"render-device "card1""#).is_err());
}
