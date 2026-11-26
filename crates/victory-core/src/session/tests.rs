//! A whole session in a temporary folder: the only test that starts one
//! (the session is the process's own).

use super::*;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn a_session_writes_events_and_a_summary() {
    let base = tempfile::tempdir().unwrap();
    let subscriber = tracing_subscriber::registry().with(SessionLayer);
    tracing::subscriber::with_default(subscriber, || {
        let dir = start_in(
            base.path(),
            BuildInfo {
                version: "0.0.0-test",
                git: "abc1234",
                profile: "test",
            },
            Some(Path::new(r"D:\Jogos\Titulo")),
        )
        .expect("session folder");
        assert!(dir.starts_with(base.path()));

        // Menu frames, then world frames.
        let mut subs = 0;
        for i in 0..40 {
            subs += if i < 10 { 2 } else { 12 };
            frame_presented(subs);
        }
        tracing::info!(
            spark.kind = "frame_rate",
            spark.fps = 30.0,
            spark.ms = 33.3,
            spark.preparing_ms = 12.0,
            spark.gpu_ms = 20.0,
            spark.draws = 1500.0,
            spark.dropped = 0.0,
            "30.0 fps, 33.3 ms/frame"
        );
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        tracing::warn!(path = %format!(r"{home}\AppData\x"), "a warning with a personal path");
        tracing::warn!(r"game file D:\Jogos\Titulo\data.rpf missing");
        // A flood from one call site: only the first RATE_EVENTS are written.
        for i in 0..100 {
            tracing::info!(i, "noisy");
        }
        tracing::debug!("below the file's level");
        finish("teste");

        let events = std::fs::read_to_string(dir.join("eventos.jsonl")).unwrap();
        let lines: Vec<&str> = events.lines().collect();
        assert!(
            lines[0].starts_with(r#"{"kind":"session","format":1,"#),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains(r#""git":"abc1234""#));
        assert!(lines.iter().all(|l| l.starts_with('{') && l.ends_with('}')));
        assert!(events.contains(r#""spark.fps":30"#), "{events}");
        assert!(events.contains("<JOGO>\\\\data.rpf"), "{events}");
        if home.len() >= 4 {
            assert!(
                !events
                    .to_ascii_lowercase()
                    .contains(&home.to_ascii_lowercase().replace('\\', "\\\\"))
            );
            assert!(events.contains("<HOME>\\\\AppData"), "{events}");
        }
        let noisy = lines
            .iter()
            .filter(|l| l.contains(r#""message":"noisy""#))
            .count();
        assert_eq!(noisy, RATE_EVENTS as usize);
        assert!(!events.contains("below the file's level"));

        let md = std::fs::read_to_string(dir.join("resumo.md")).unwrap();
        assert!(md.contains("## Próximo alvo"), "{md}");
        assert!(md.contains("Fim: teste"), "{md}");
        assert!(md.contains("a warning with a personal path"));
        assert!(md.contains("repetidos suprimidos"), "{md}");
        let json = std::fs::read_to_string(dir.join("resumo.json")).unwrap();
        assert!(json.contains(r#""end":"teste""#));
        assert!(json.contains(r#""git":"abc1234""#));
        assert!(!dir.join("resumo.md.tmp").exists());

        // After the end: nothing is queued, and a second end is harmless.
        tracing::warn!("after the end");
        finish("again");
        crash(Crash {
            what: "panic".into(),
            ..Crash::default()
        });
        let json = std::fs::read_to_string(dir.join("resumo.json")).unwrap();
        assert!(json.contains(r#""what":"panic""#), "{json}");
    });
}
