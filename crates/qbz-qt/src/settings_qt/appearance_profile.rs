//! Visual presets do not change audio, renderer selection or waveform experiments.
use serde_json::{Map, Value, json};

fn values(low: bool) -> Map<String, Value> {
    json!({
        "album_header_gradient": !low,
        "compact_album_header": low,
        "app_background": "off",
        "large_visualizer_on": !low,
        "sidebar_playlist_collage": !low,
        "queue_track_artwork": false,
        "library_track_artwork": false,
        "local_library_track_artwork": false,
        "play_indicator_animation": false,
        "npb_mode": if low { "small" } else { "new" }
    }).as_object().unwrap().clone()
}

pub(super) fn managed(key: &str) -> bool {
    matches!(key, "album_header_gradient" | "compact_album_header" | "app_background"
        | "large_visualizer_on" | "sidebar_playlist_collage" | "queue_track_artwork"
        | "library_track_artwork" | "local_library_track_artwork" | "play_indicator_animation" | "npb_mode")
}

fn classify(doc: &Map<String, Value>) -> &'static str {
    let defaults = values(false);
    let matches = |low| values(low).iter().all(|(key, expected)| {
        doc.get(key).unwrap_or(&defaults[key]) == expected
    });
    match doc.get("appearance_profile").and_then(Value::as_str) {
        Some("custom") => "custom",
        Some("low") if matches(true) => "low",
        None | Some("default") if matches(false) => "default",
        _ => "custom",
    }
}

pub(super) fn selected() -> String {
    super::prefs_path().and_then(|path| super::read_json_object(&path))
        .map(|doc| classify(&doc).to_string()).unwrap_or_else(|| "default".into())
}

pub(super) fn apply(profile: &str) {
    if !matches!(profile, "low" | "default" | "custom") { return; }
    super::update_prefs(|doc| {
        if profile != "custom" { doc.extend(values(profile == "low")); }
        doc.insert("appearance_profile".into(), json!(profile));
        true
    });
    // Read back persisted values so a failed preferences write cannot leave
    // the live shell using a preset that was never saved.
    let mode = super::npb_mode_index();
    let visualizer = super::large_visualizer_on();
    let height = crate::shell_bridge::large_dock_height(visualizer);
    let collage = super::sidebar_playlist_collage();
    let background = super::app_background_mode();
    crate::shell_bridge::ui(move |mut shell| {
        shell.as_mut().set_npb_mode(mode);
        shell.as_mut().set_large_visualizer_on(visualizer);
        shell.as_mut().set_large_dock_height(height);
        shell.as_mut().set_sidebar_playlist_collage(collage);
        shell.as_mut().set_ambient_mode(background);
    });
    if !visualizer { crate::viz_qt::set_enabled(false); }
    crate::local_album_actions::publish_track_artwork();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_custom_values_and_ignores_unrelated_preferences() {
        let mut doc = Map::new();
        assert_eq!(classify(&doc), "default");
        doc.insert("language".into(), json!("es"));
        doc.insert("seekbar_waveform".into(), json!(true));
        assert_eq!(classify(&doc), "default");
        doc.insert("queue_track_artwork".into(), json!(true));
        assert_eq!(classify(&doc), "custom");
        doc.extend(values(true));
        doc.insert("appearance_profile".into(), json!("low"));
        assert_eq!(classify(&doc), "low");
        assert_eq!(doc["seekbar_waveform"], true);
        doc.insert("npb_mode".into(), json!("large"));
        assert_eq!(classify(&doc), "custom");
    }
}
