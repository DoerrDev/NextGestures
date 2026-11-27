use gesture_core::{Action, Config, FireMode, Gesture, TriggerButton, recognizer};
use std::hint::black_box;
use std::time::Instant;

#[test]
#[ignore = "timing baseline, run explicitly in release mode"]
fn perf_save_existing_config() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("config.toml");
    for count in [0, 32, 128] {
        let mut config = Config::default();
        config.profiles[0].gestures = (0..count)
            .map(|i| Gesture {
                id: format!("perf-{i}"),
                name: format!("Gesture {i}"),
                enabled: true,
                deletable: true,
                trigger: Some(TriggerButton::Right),
                region: None,
                wheel: vec![],
                left_click: false,
                dwell_ms: 0,
                fire_mode: FireMode::OnRelease,
                points: recognizer::normalize(&[
                    [0.0, 0.0],
                    [100.0 + i as f32, 0.0],
                    [100.0 + i as f32, 80.0],
                ])
                .unwrap(),
                times: vec![],
                action: Action::None,
                hotkey: vec![],
            })
            .collect();
        config.save(&path)?;
        let mut document: toml::Table = toml::from_str(&std::fs::read_to_string(&path)?)?;
        document["settings"].as_table_mut().unwrap().insert(
            "future_perf_marker".into(),
            toml::Value::String("preserve".into()),
        );
        std::fs::write(&path, toml::to_string(&document)?)?;
        let mut load_times = Vec::new();
        let mut save_times = Vec::new();
        for i in 0..17 {
            config.settings.trail_width = if i % 2 == 0 { 5.0 } else { 6.0 };

            let start = Instant::now();
            config.save(&path)?;
            let save = start.elapsed();
            let start = Instant::now();
            let loaded = black_box(Config::load(&path)?);
            let load = start.elapsed();

            assert_eq!(loaded.settings.trail_width, config.settings.trail_width);
            assert_eq!(loaded.profiles[0].gestures.len(), count);
            if i >= 2 {
                save_times.push(save);
                load_times.push(load);
            }
        }
        let text = std::fs::read_to_string(&path)?;
        let document: toml::Table = toml::from_str(&text)?;
        assert_eq!(
            document["settings"]["future_perf_marker"].as_str(),
            Some("preserve")
        );
        load_times.sort_unstable();
        save_times.sort_unstable();
        println!(
            "{count:>3} gestures, {} bytes: load {:.2} ms, save {:.2} ms",
            text.len(),
            load_times[7].as_secs_f64() * 1e3,
            save_times[7].as_secs_f64() * 1e3,
        );
    }
    Ok(())
}
