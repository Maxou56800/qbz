// crates/qbz-app/src/settings/bundle/tests.rs — the normative test suite for
// the settings portability engine. Per 04-settings-portability.md, "the
// classification table IS the test suite": one test per §3/§5 rule. The engine
// takes a `LiveSystem` by injection, so nothing here touches audio hardware.

use super::*;
use serde_json::json;

// ---------------------------------- fixtures ----------------------------------

fn scratch(name: &str) -> ProfilePaths {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "qbz-bundle-{name}-{}-{nonce}",
        std::process::id()
    ));
    ProfilePaths {
        config_root: base.join("config"),
        data_root: base.join("data"),
    }
}

fn cleanup(p: &ProfilePaths) {
    let _ = std::fs::remove_dir_all(p.data_root.parent().unwrap_or(&p.data_root));
}

fn live() -> LiveSystem {
    LiveSystem {
        backends: vec!["SystemDefault".into(), "Alsa".into(), "PipeWire".into()],
        devices: vec![
            ("hw:1,0".into(), "Topping D90".into()),
            ("hw:0,0".into(), "Onboard".into()),
        ],
    }
}

fn bundle_with(domains: serde_json::Value) -> Bundle {
    let obj = domains.as_object().cloned().unwrap_or_default();
    Bundle {
        schema_version: SCHEMA_VERSION,
        created_at: "2026-07-14T09:30:00Z".into(),
        source: BundleSource {
            app_version: "2.0.2".into(),
            profile: "desktop".into(),
            hostname: "workstation".into(),
        },
        domains: obj,
    }
}

fn find(lines: &[PlanLine], key: &str) -> Option<PlanLine> {
    lines.iter().find(|l| l.key == key).cloned()
}

fn find_contains(lines: &[PlanLine], needle: &str) -> Option<PlanLine> {
    lines.iter().find(|l| l.key.contains(needle)).cloned()
}

fn write_of<'a>(plan: &'a ImportPlan, key: &str) -> Option<&'a Value> {
    plan.writes.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

// ------------------------------- the rules -------------------------------

#[test]
fn portable_fields_apply_verbatim() {
    // §3 PORTABLE: playback.*, the audio portable subset, prefs.streaming_quality.
    let p = scratch("portable");
    let bundle = bundle_with(json!({
        "playback": {
            "autoplay_mode": "infinite",
            "show_context_icon": false,
            "persist_session": true,
            "resume_playback_position": false
        },
        "audio": {
            "gapless_enabled": true,
            "stream_buffer_seconds": 4,
            "normalization_target_lufs": -18.0,
            "normalization_prevent_clipping": false,
            "sync_audio_on_startup": true
        },
        "prefs": { "streaming_quality": "hires_plus" }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    assert!(plan.adapted.is_empty(), "portable fields must not adapt: {:?}", plan.adapted);
    assert_eq!(find(&plan.applied, "playback.autoplay_mode").unwrap().new, "infinite");
    assert_eq!(find(&plan.applied, "playback.show_context_icon").unwrap().new, "false");
    assert_eq!(find(&plan.applied, "audio.gapless_enabled").unwrap().new, "true");
    assert_eq!(find(&plan.applied, "audio.stream_buffer_seconds").unwrap().new, "4");
    assert_eq!(find(&plan.applied, "prefs.streaming_quality").unwrap().new, "hires_plus");
    cleanup(&p);
}

#[test]
fn volume_is_never_class_even_hand_added() {
    // §1 corollary: a hand-edited bundle with `volume` anywhere -> skipped, always.
    let p = scratch("volume");
    let bundle = bundle_with(json!({
        "audio": { "volume": 0.8, "gapless_enabled": true },
        "prefs": { "streaming_quality": "cd", "volume": 0.9 }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    let a = find(&plan.skipped, "audio.volume").expect("audio.volume skipped");
    assert!(a.why.contains("never imported"), "{}", a.why);
    let pr = find(&plan.skipped, "prefs.volume").expect("prefs.volume skipped");
    assert!(pr.why.contains("never imported"), "{}", pr.why);
    // No write ever carries a volume.
    assert!(plan.writes.iter().all(|(k, _)| !k.contains("volume")));
    cleanup(&p);
}

#[test]
fn device_uuid_never_imported() {
    // §2.4: cloning the QConnect identity makes two nodes fight → NEVER.
    let p = scratch("uuid");
    let bundle = bundle_with(json!({
        "qconnect": { "device_uuid": "abc-123", "device_name": "Studio" }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    assert!(find(&plan.skipped, "qconnect.device_uuid").is_some());
    // device_name is still applied verbatim.
    assert_eq!(find(&plan.applied, "qconnect.device_name").unwrap().new, "Studio");
    assert!(plan.writes.iter().all(|(k, _)| k != "qconnect.device_uuid"));
    cleanup(&p);
}

#[test]
fn dsd_downgrades_without_trust_flag() {
    // §5.3 step 4: dop/native → convert unless --trust-dsd (current is convert,
    // so this is a CHANGE — the no-change short-circuit does not fire).
    let p = scratch("dsd");
    let bundle = bundle_with(json!({ "audio": { "dsd_mode": "dop" } }));

    let plan_no_trust = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");
    let line = find(&plan_no_trust.adapted, "audio.dsd_mode").expect("dsd adapted");
    assert_eq!(line.old.as_deref(), Some("dop"));
    assert_eq!(line.new, "convert");
    assert_eq!(write_of(&plan_no_trust, "audio.dsd_mode"), Some(&json!("convert")));

    let opts = ImportOptions { trust_dsd: true, ..Default::default() };
    let plan_trust = plan(&bundle, &p, &opts, &live()).expect("plan");
    assert!(find(&plan_trust.adapted, "audio.dsd_mode").is_none());
    assert_eq!(find(&plan_trust.applied, "audio.dsd_mode").unwrap().new, "dop");
    cleanup(&p);
}

#[test]
fn ask_maps_to_always_fallback_in_adapted() {
    // §5.5: "ask" needs a UI the daemon lacks → always_fallback, in adapted,
    // never a silent skip.
    let p = scratch("ask");
    let bundle = bundle_with(json!({ "audio": { "quality_fallback_behavior": "ask" } }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    let line = find(&plan.adapted, "audio.quality_fallback_behavior").expect("adapted");
    assert_eq!(line.old.as_deref(), Some("ask"));
    assert_eq!(line.new, "always_fallback");
    assert!(find(&plan.skipped, "audio.quality_fallback_behavior").is_none());
    cleanup(&p);
}

#[test]
fn unknown_field_skipped_never_error() {
    // §5.3 step 3 / §7: unknown keys → skipped, never an error.
    let p = scratch("unknown");
    let bundle = bundle_with(json!({
        "audio": { "some_future_flag": true },
        "brand_new_domain": { "x": 1 }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("must not error");

    let a = find(&plan.skipped, "audio.some_future_flag").expect("unknown audio key skipped");
    assert!(a.why.contains("unknown field"), "{}", a.why);
    let d = find(&plan.skipped, "brand_new_domain").expect("unknown domain skipped");
    assert!(d.why.contains("unknown field"), "{}", d.why);
    cleanup(&p);
}

#[test]
fn secrets_double_gate() {
    // §3/§6: secrets present but no import-side --include-auth → skipped, and the
    // auth token is NOT queued for validation.
    let p = scratch("secrets");
    // A user must exist for the scrobbler secret to reach the gate (else §5.7
    // no-user skip fires first).
    std::fs::create_dir_all(&p.data_root).unwrap();
    write_last_user_id(&p.data_root, 1234567).unwrap();

    let bundle = bundle_with(json!({
        "integrations": { "scrobblers": { "lastfm_session_key": "d580secret" } },
        "auth": { "user_auth_token": "Bo4Asecret", "user_id": 1234567 }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    let token_line = find(&plan.skipped, "auth.user_auth_token").expect("auth token skipped");
    assert!(token_line.why.contains("--include-auth"), "{}", token_line.why);
    assert!(plan.auth_token.is_none(), "token must not be queued without the gate");

    let secret = find(&plan.skipped, "integrations.scrobblers.lastfm_session_key")
        .expect("scrobbler secret skipped");
    assert!(secret.why.contains("--include-auth"), "{}", secret.why);
    cleanup(&p);
}

#[test]
fn secret_applies_with_gate() {
    // The other half of the double gate: with --include-auth the token is queued
    // for validation and the scrobbler secret applies.
    let p = scratch("secret-gate");
    std::fs::create_dir_all(&p.data_root).unwrap();
    write_last_user_id(&p.data_root, 1234567).unwrap();
    let bundle = bundle_with(json!({
        "integrations": { "scrobblers": { "lastfm_session_key": "d580secret" } },
        "auth": { "user_auth_token": "Bo4Asecret", "user_id": 1234567 }
    }));
    let opts = ImportOptions { include_auth: true, ..Default::default() };

    let plan = plan(&bundle, &p, &opts, &live()).expect("plan");

    assert_eq!(plan.auth_token.as_deref(), Some("Bo4Asecret"));
    assert_eq!(plan.bundle_user_id, Some(1234567));
    assert!(find(&plan.applied, "integrations.scrobblers.lastfm_session_key").is_some());
    cleanup(&p);
}

#[test]
fn version_gate_rejects_newer() {
    // §5.6: a bundle newer than this importer is rejected.
    let p = scratch("version");
    let mut bundle = bundle_with(json!({ "audio": { "gapless_enabled": true } }));
    bundle.schema_version = 2;

    let err = plan(&bundle, &p, &ImportOptions::default(), &live()).unwrap_err();
    match err {
        BundleError::VersionTooNew { bundle, supported } => {
            assert_eq!(bundle, 2);
            assert_eq!(supported, 1);
        }
        other => panic!("expected VersionTooNew, got {other:?}"),
    }
    cleanup(&p);
}

#[test]
fn missing_device_non_tty_falls_back_safe() {
    // §5.3 step 4 non-TTY: backend→SystemDefault, device→null, intent flags→false,
    // all reported in adapted; no device_pick and never hangs.
    let p = scratch("nontty");
    let bundle = bundle_with(json!({
        "audio": {
            "output_device": "hw:9,9",
            "backend_type": "Jack",
            "exclusive_mode": true,
            "dac_passthrough": true
        }
    }));
    let opts = ImportOptions { non_tty: true, ..Default::default() };

    let plan = plan(&bundle, &p, &opts, &live()).expect("plan");

    assert!(plan.device_pick.is_none(), "non-tty must not request a pick");

    let dev = find(&plan.adapted, "audio.output_device").expect("device adapted");
    assert_eq!(dev.old.as_deref(), Some("hw:9,9"));
    assert_eq!(write_of(&plan, "audio.output_device"), Some(&Value::Null));

    let backend = find(&plan.adapted, "audio.backend_type").expect("backend adapted");
    assert_eq!(write_of(&plan, "audio.backend_type"), Some(&json!("SystemDefault")));
    assert!(backend.new.contains("SystemDefault"));

    for flag in ["audio.exclusive_mode", "audio.dac_passthrough"] {
        let l = find(&plan.adapted, flag).unwrap_or_else(|| panic!("{flag} must adapt"));
        assert_eq!(l.new, "false", "{flag} must reset to false");
        assert_eq!(write_of(&plan, flag), Some(&Value::Bool(false)));
    }
    cleanup(&p);
}

#[test]
fn found_device_applies_verbatim() {
    // A machine field that validates cleanly lands in APPLIED (§5.4).
    let p = scratch("found");
    let bundle = bundle_with(json!({
        "audio": {
            "output_device": "hw:1,0",
            "backend_type": "Alsa",
            "exclusive_mode": true
        }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    assert!(plan.device_pick.is_none());
    assert_eq!(find(&plan.applied, "audio.output_device").unwrap().new, "hw:1,0");
    assert_eq!(find(&plan.applied, "audio.backend_type").unwrap().new, "Alsa");
    assert_eq!(find(&plan.applied, "audio.exclusive_mode").unwrap().new, "true");
    assert!(plan.adapted.is_empty(), "clean validation must not adapt: {:?}", plan.adapted);
    cleanup(&p);
}

#[test]
fn absent_fields_leave_target_untouched() {
    // §7: only present fields have effects.
    let p = scratch("absent");
    let bundle = bundle_with(json!({ "playback": { "autoplay_mode": "track_only" } }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    // Exactly one write (the single present field); nothing audio/qconnect/etc.
    assert_eq!(plan.writes.len(), 1);
    assert_eq!(plan.writes[0].0, "playback.autoplay_mode");
    assert!(plan.writes.iter().all(|(k, _)| !k.starts_with("audio.")));
    cleanup(&p);
}

#[test]
fn machine_caches_always_skipped() {
    // §2.2/§3: source-machine device caches are meaningless on the target.
    let p = scratch("caches");
    let bundle = bundle_with(json!({
        "audio": {
            "device_max_sample_rate": 768000,
            "device_sample_rate_limits": { "hw:4,0": 768000 }
        }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    for key in ["audio.device_max_sample_rate", "audio.device_sample_rate_limits"] {
        let l = find(&plan.skipped, key).unwrap_or_else(|| panic!("{key} must skip"));
        assert!(l.why.contains("device cache"), "{}", l.why);
    }
    assert!(plan
        .writes
        .iter()
        .all(|(k, _)| !k.contains("device_max_sample_rate") && !k.contains("device_sample_rate_limits")));
    cleanup(&p);
}

#[test]
fn roundtrip_same_box_is_noop() {
    // §7 acceptance invariant: export(daemon) → plan(same box) ⇒ adapted EMPTY,
    // applied values == current values, skipped == the always-skip caches only.
    let p = scratch("roundtrip");
    std::fs::create_dir_all(&p.data_root).unwrap();

    // Configure a realistic daemon (never "ask", never "remember_last", a real
    // device that this box's LiveSystem enumerates).
    {
        let audio = AudioSettingsStore::new_at(&p.data_root).unwrap();
        audio.set_backend_type(Some(AudioBackendType::Alsa)).unwrap();
        audio.set_output_device(Some("hw:1,0")).unwrap();
        audio.set_exclusive_mode(true).unwrap();
        audio.set_dsd_mode("dop").unwrap(); // a working DSD daemon
        audio.set_quality_fallback_behavior("always_fallback").unwrap();
        audio.set_gapless_enabled(true).unwrap();

        let pb = PlaybackPreferencesStore::new_at(&p.data_root).unwrap();
        pb.set_persist_session(true).unwrap();

        qconnect_kv_write(
            &p.data_root.join("qconnect_settings.db"),
            "device_name",
            Some("Estudio"),
        )
        .unwrap();
        qconnect_kv_write(
            &p.data_root.join("qconnect_settings.db"),
            "startup_mode",
            Some("on"),
        )
        .unwrap();
    }

    let src = ExportSource::Daemon(ProfilePaths {
        config_root: p.config_root.clone(),
        data_root: p.data_root.clone(),
    });
    let bundle = export(src, &ExportOptions::default()).expect("export");

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    assert!(
        plan.adapted.is_empty(),
        "roundtrip must not adapt anything: {:?}",
        plan.adapted
    );
    assert!(plan.device_pick.is_none());
    // dsd dop survived without --trust-dsd (no-change short-circuit).
    assert_eq!(find(&plan.applied, "audio.dsd_mode").unwrap().new, "dop");
    assert_eq!(find(&plan.applied, "audio.output_device").unwrap().new, "hw:1,0");
    assert_eq!(find(&plan.applied, "qconnect.startup_mode").unwrap().new, "on");

    // Every skipped line must be one of the always-skip caches.
    for l in &plan.skipped {
        assert!(
            l.key.contains("device_max_sample_rate") || l.key.contains("device_sample_rate_limits"),
            "unexpected skip in roundtrip: {} ({})",
            l.key,
            l.why
        );
    }
    cleanup(&p);
}

#[test]
fn remember_last_maps_to_on_in_adapted() {
    // §5.5: startup_mode remember_last → on (daemon has no last-state tracking).
    let p = scratch("remember");
    let bundle = bundle_with(json!({ "qconnect": { "startup_mode": "remember_last" } }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    let line = find(&plan.adapted, "qconnect.startup_mode").expect("adapted");
    assert_eq!(line.old.as_deref(), Some("remember_last"));
    assert_eq!(line.new, "on");
    cleanup(&p);
}

#[test]
fn library_folders_skipped_on_daemon() {
    // §2.6: the P0 daemon has no local library.
    let p = scratch("folders");
    let bundle = bundle_with(json!({
        "library_folders": [ { "path": "/mnt/music", "network_fs": false } ]
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    let line = find_contains(&plan.skipped, "library_folders").expect("skipped");
    assert!(line.why.contains("no local library"), "{}", line.why);
    cleanup(&p);
}

#[test]
fn apply_writes_are_idempotent_and_persist() {
    // §5.3 step 6: pure setter writes; a second apply is safe and lands the same
    // values. Exercises the whole applied/adapted write path end-to-end.
    let p = scratch("apply");
    let bundle = bundle_with(json!({
        "playback": { "autoplay_mode": "track_only", "persist_session": false },
        "audio": {
            "output_device": "hw:1,0",
            "backend_type": "Alsa",
            "gapless_enabled": true,
            "dsd_mode": "dop"
        },
        "prefs": { "streaming_quality": "cd" },
        "qconnect": { "device_name": "Kitchen", "startup_mode": "on" }
    }));
    let opts = ImportOptions { trust_dsd: true, ..Default::default() };

    let plan = plan(&bundle, &p, &opts, &live()).expect("plan");
    apply(&plan, &p, None).expect("apply once");
    apply(&plan, &p, None).expect("apply twice (idempotent)");

    let audio = AudioSettingsStore::new_at(&p.data_root).unwrap().get_settings().unwrap();
    assert_eq!(audio.output_device.as_deref(), Some("hw:1,0"));
    assert_eq!(audio.backend_type, Some(AudioBackendType::Alsa));
    assert!(audio.gapless_enabled);
    assert_eq!(audio.dsd_mode, "dop");

    let pb = PlaybackPreferencesStore::new_at(&p.data_root).unwrap().get_preferences().unwrap();
    assert!(!pb.persist_session);

    assert_eq!(daemon_prefs::load_at(&p.data_root).streaming_quality, "cd");
    assert_eq!(
        qconnect_kv_read(&p.data_root.join("qconnect_settings.db"), "device_name").as_deref(),
        Some("Kitchen")
    );
    cleanup(&p);
}

#[test]
fn exact_alsa_mixer_route_map_survives_bundle_plan_and_apply() {
    let p = scratch("alsa-mixer-map");
    let route = "alsa-route-v1|card=by-id:usb-D50-00|pcm=hw|DEV=0";
    let bundle = bundle_with(json!({
        "audio": {
            "output_device": "hw:1,0",
            "backend_type": "Alsa",
            "alsa_hardware_volume": false,
            "alsa_hardware_volume_controls": {
                (route): { "name": "D50 III", "index": 0 }
            }
        }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");
    assert!(
        find(&plan.applied, "audio.alsa_hardware_volume_controls").is_some(),
        "exact route map must be classified"
    );
    assert!(plan.routing_critical_changed);
    apply(&plan, &p, None).expect("apply map");

    let audio = AudioSettingsStore::new_at(&p.data_root).unwrap().get_settings().unwrap();
    let route_key =
        serde_json::from_value::<qbz_audio::alsa_hardware_volume::StableAlsaRouteKey>(json!(route))
            .unwrap();
    assert_eq!(
        audio
            .alsa_hardware_volume_controls
            .get(&route_key)
            .map(|control| (control.name.as_str(), control.index)),
        Some(("D50 III", 0))
    );
    cleanup(&p);
}

#[test]
fn bundle_json_roundtrips_flat() {
    // The on-disk shape is flat (§2.9): header + domains at the top level.
    let bundle = bundle_with(json!({ "audio": { "gapless_enabled": true } }));
    let text = bundle.to_json_string().unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["source"]["profile"], "desktop");
    assert_eq!(v["audio"]["gapless_enabled"], true);

    let reparsed = Bundle::parse(&text).unwrap();
    assert_eq!(reparsed.schema_version, 1);
    assert!(reparsed.domains.contains_key("audio"));
}

#[test]
fn parse_rejects_missing_version() {
    let err = Bundle::parse(r#"{ "audio": {} }"#).unwrap_err();
    assert!(matches!(err, BundleError::VersionMalformed));
}

#[test]
fn secret_values_never_render_in_summary() {
    // §5.4: SECRET-class values are MASKED in the summary lines — the raw value
    // rides only the write list (terminal scrollback / CI logs must never see it).
    let p = scratch("mask");
    std::fs::create_dir_all(&p.data_root).unwrap();
    write_last_user_id(&p.data_root, 1).unwrap();
    let bundle = bundle_with(json!({
        "integrations": { "scrobblers": {
            "lastfm_session_key": "d580REALSECRET",
            "listenbrainz_token": ""
        } }
    }));
    let opts = ImportOptions { include_auth: true, ..Default::default() };

    let plan = plan(&bundle, &p, &opts, &live()).expect("plan");

    let key_line = find(&plan.applied, "integrations.scrobblers.lastfm_session_key").unwrap();
    assert_eq!(key_line.new, "(secret, applied)");
    let empty_line = find(&plan.applied, "integrations.scrobblers.listenbrainz_token").unwrap();
    assert_eq!(empty_line.new, "(empty)");

    // No rendered bucket line anywhere carries the raw secret.
    let rendered = format!("{:?} {:?} {:?}", plan.applied, plan.adapted, plan.skipped);
    assert!(!rendered.contains("d580REALSECRET"), "secret leaked into summary: {rendered}");

    // The write list still carries the real value so apply works.
    assert_eq!(
        write_of(&plan, "integrations.scrobblers.lastfm_session_key"),
        Some(&json!("d580REALSECRET"))
    );
    cleanup(&p);
}

#[test]
fn contains_secrets_keys_on_actual_secret_values() {
    // §3 warning trigger: any secret VALUE present (auth token OR a non-blank
    // scrobbler secret) — not the auth domain alone.
    let none = bundle_with(json!({ "audio": { "gapless_enabled": true } }));
    assert!(!none.contains_secrets());

    let blank = bundle_with(json!({
        "integrations": { "scrobblers": { "lastfm_session_key": "", "listenbrainz_token": "" } }
    }));
    assert!(!blank.contains_secrets(), "blank secrets are not secrets");

    let scrob = bundle_with(json!({
        "integrations": { "scrobblers": { "lastfm_session_key": "sk-live" } }
    }));
    assert!(scrob.contains_secrets());

    let auth = bundle_with(json!({ "auth": { "user_auth_token": "tok" } }));
    assert!(auth.contains_secrets());
}

#[test]
fn device_pick_names_the_backend() {
    // §5.4 prompt: "Available on Alsa:" — the pick carries the backend name.
    let p = scratch("pick-backend");
    let bundle = bundle_with(json!({
        "audio": { "output_device": "hw:9,9", "backend_type": "Alsa" }
    }));

    let plan = plan(&bundle, &p, &ImportOptions::default(), &live()).expect("plan");

    let pick = plan.device_pick.expect("TTY plan must request a pick");
    assert_eq!(pick.backend, "Alsa");
    assert_eq!(pick.wanted, "hw:9,9");
    cleanup(&p);
}

#[test]
fn audio_writer_rejects_an_unimplemented_key() {
    let paths = scratch("unsupported-audio-writer");
    let value = json!(true);
    assert!(apply_audio_writes(&paths.data_root, &[("not_implemented", &value)]).is_err());
    cleanup(&paths);
}

#[test]
fn negative_normalization_target_reaches_persistent_storage() {
    let paths = scratch("negative-normalization");
    let value = json!(-18.5);
    apply_audio_writes(&paths.data_root, &[("normalization_target_lufs", &value)]).unwrap();
    let store = AudioSettingsStore::new_at(&paths.data_root).unwrap();
    assert_eq!(store.get_settings().unwrap().normalization_target_lufs, -18.5);
    cleanup(&paths);
}
