use super::*;
use crate::events::{Filter, Sink};
use crate::paths::Paths;
use std::sync::atomic::AtomicBool;

#[test]
fn a_remote_missed_invalidates_unfiltered_document_readers_with_the_local_sequence() {
    let root = std::path::PathBuf::from(format!(
        "/var/tmp/hc-doc-missed-{}-{}",
        std::process::id(),
        crate::platform::random_hex()
    ));
    let paths = Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.clone(),
        config: root.join("config.toml"),
    };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();
    let machine = Machine {
        name: "rohan".into(),
        target: "rohan".into(),
        via: false,
        temporary: false,
        session: None,
        id: "remote".into(),
        host: "rohan".into(),
        version: "test".into(),
    };
    let (tx, mut rx) = mpsc::channel(4);
    let (filtered_tx, mut filtered_rx) = mpsc::channel(4);
    {
        let mut st = core.lock();
        st.events.seq = 7;
        st.machines.push(machine.clone());
        core.subscribe(&mut st, Filter::default(), Sink::Plain { tx, behind: Arc::new(AtomicBool::new(false)) });
        core.subscribe(
            &mut st,
            Filter { terminal: Some("t1".into()), ..Default::default() },
            Sink::Plain { tx: filtered_tx, behind: Arc::new(AtomicBool::new(false)) },
        );
    }
    let (frames, _rx) = mpsc::unbounded_channel();
    let line = Line::new(frames, &json!({}));
    mirror_events(&core, &machine, &line, b"{\"method\":\"missed\",\"params\":{\"seq\":9999,\"terminals\":[]}}\n");
    let msg: Value = serde_json::from_str(&rx.try_recv().expect("local missed notification")).unwrap();
    assert_eq!(msg["method"], "missed");
    assert_eq!(msg["params"]["seq"], 7);
    assert!(filtered_rx.try_recv().is_err());
    core.lock().machines.clear();
    mirror_event(&core, &machine, json!({"type": "doc", "data": {"name": "app", "version": 1}}));
    assert!(rx.try_recv().is_err(), "removed machines must not emit doc events");
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn absent_native_session_stays_canonical_across_events_and_reconnects() {
    let root = std::env::temp_dir().join(format!("hc-native-mirror-{}", crate::platform::random_hex()));
    let paths = Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.clone(),
        config: root.join("config.toml"),
    };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();
    let machine = Machine {
        name: "rohan".into(),
        target: "rohan".into(),
        via: false,
        temporary: false,
        session: None,
        id: "remote".into(),
        host: "rohan".into(),
        version: "test".into(),
    };
    let mut registry = crate::terminals::Registry::default();
    let id = registry
        .create(crate::terminals::NewTerminal { argv: vec!["unused".into()], keep: true, ..Default::default() }, 0)
        .unwrap();
    registry.terminals.get_mut(&id).unwrap().reported = true;
    // Keep the existing empty-record omission behavior out of this native-field regression.
    registry
        .terminals
        .get_mut(&id)
        .unwrap()
        .records
        .insert("child".into(), serde_json::from_value(json!({"state": "idle"})).unwrap());
    let mut record = serde_json::to_value(crate::terminals::Public(registry.terminals[&id].clone())).unwrap();
    assert!(record.get("resume_session").is_none());
    mirror_replace(&core, &machine, vec![record.clone()]);
    let (tx, mut events) = mpsc::unbounded_channel();
    core.subscribe(&mut core.lock(), Filter::default(), Sink::Waiter(tx));
    for (kind, data) in [
        (
            "status",
            json!({"status": null, "records": {"child": {"state": "idle"}}, "reported": true, "resume_session": null}),
        ),
        ("keep", json!({"keep": false, "resume_session": null})),
    ] {
        if kind == "keep" {
            let mut bound = record.clone();
            bound["resume_session"] = "native-session".into();
            mirror_replace(&core, &machine, vec![bound]);
            assert_eq!(events.try_recv().unwrap()["type"], "created");
            record["keep"] = false.into();
        }
        mirror_event(&core, &machine, json!({"type": kind, "terminal": "rohan/t1", "data": data}));
        assert_eq!(events.try_recv().unwrap()["type"], kind);
        mirror_replace(&core, &machine, vec![record.clone()]);
        assert!(events.try_recv().is_err(), "unchanged reconnect must not emit created after {kind}");
    }
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
