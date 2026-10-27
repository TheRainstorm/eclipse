#![cfg(not(feature = "enabled"))]

#[tokio::test]
async fn disabled_plugin_rejects_network_entries_and_leaves_the_engine_registry_empty() {
    assert!(!uu_compat::available());
    uu_compat::register().unwrap();
    assert!(uu_core::plugins::registry().get("uu").is_err());
    let config = uu_core::Config {
        data_dir: std::env::temp_dir().join("eclipse-disabled-plugin-unused"),
        state_dir: std::env::temp_dir().join("eclipse-disabled-plugin-unused-state"),
        signaling_host: "127.0.0.1:9".into(),
    };
    assert!(uu_compat::SignalClient::new(&config).is_err());
    let error = uu_compat::socketio::SocketIo::connect("ws://127.0.0.1:9", "generated").await.err().unwrap();
    assert!(error.to_string().contains("not installed or enabled"));
    assert!(uu_compat::controller::spawn(config, "generated-device".into(), "0.0.0.0".into()).is_err());
}
