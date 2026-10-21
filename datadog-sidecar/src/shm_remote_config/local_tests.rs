// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::cdn::CdnPublisher;
use super::*;
use libdd_ffe::cdn::ConfigurationSink;
use libdd_ffe::rules_based::{Configuration, UniversalFlagConfig};
use libdd_remote_config::config::dynamic::{DynamicConfigFile, tests::dummy_dynamic_config};

fn configuration(environment: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "format": "SERVER", "createdAt": "2026-10-21T00:00:00Z",
        "environment": {"name": environment}, "flags": {}
    }))
    .unwrap()
}

fn apply(publisher: &CdnPublisher, environment: &str) {
    publisher
        .apply(Configuration::from_server_response(
            UniversalFlagConfig::from_json(configuration(environment)).unwrap(),
        ))
        .unwrap();
}

fn manager() -> RemoteConfigManager {
    RemoteConfigManager::new_with_registry(
        ConfigInvariants {
            language: "php".into(),
            tracer_version: "test".into(),
            endpoint: libdd_common::Endpoint::from_slice(&format!(
                "http://rc-{}.invalid",
                rand::random::<u64>()
            )),
            agentless: None,
        },
        Arc::new(default_registry().with::<UniversalFlagConfig>().unwrap()),
    )
}

fn target(service: &str) -> Arc<Target> {
    Arc::new(Target::new(
        service.into(),
        "test".into(),
        "1".into(),
        vec![],
        vec![],
    ))
}

fn path(product: &str) -> RemoteConfigPath {
    RemoteConfigPath::parse(&format!(
        "datadog/1/{product}/{}/config",
        rand::random::<u64>()
    ))
    .unwrap()
}

fn publish(writer: &RemoteConfigWriter, files: &[(&RemoteConfigPath, &NamedShmHandle)]) {
    let mut directory = b"agent-runtime\n".to_vec();
    for (path, file) in files {
        directory.extend_from_slice(unsafe { file.get_path() });
        directory.extend_from_slice(b":8:");
        directory.extend_from_slice(BASE64_URL_SAFE_NO_PAD.encode(path.as_str()).as_bytes());
        directory.push(b'\n');
    }
    assert!(writer.write(&directory));
}

fn assert_ffe(update: RemoteConfigUpdate, path: &str, environment: &str) {
    let RemoteConfigUpdate::Add { value, .. } = update else {
        panic!("expected FFE add: {update:?}")
    };
    assert_eq!(value.path.as_str(), path);
    let parsed = value.data.unwrap();
    let flags = parsed
        .downcast::<UniversalFlagConfig>()
        .expect("shared registry must parse UFC");
    assert_eq!(flags.to_json(), configuration(environment));
}

fn assert_remove(update: RemoteConfigUpdate, path: &str) {
    let RemoteConfigUpdate::Remove(removed) = update else {
        panic!("expected removal: {update:?}")
    };
    assert_eq!(removed.as_str(), path);
}

fn assert_none(manager: &mut RemoteConfigManager) {
    assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
}

const CDN_PATH: &str = "datadog/0/FFE_FLAGS/cdn/config";

#[test]
#[cfg_attr(miri, ignore = "requires native shared memory")]
fn local_ffe_overrides_only_ffe_and_detach_restores_latest_agent_config() {
    let mut manager = manager();
    let target = target("source-selection");
    let writer = RemoteConfigWriter::new(&manager.invariants, &target).unwrap();
    let ffe_path = path("FFE_FLAGS");
    let apm_path = path("APM_TRACING");
    let ffe = store_shm(1, &ffe_path, configuration("agent-old")).unwrap();
    let apm = store_shm(
        1,
        &apm_path,
        serde_json::to_vec(&dummy_dynamic_config(true)).unwrap(),
    )
    .unwrap();
    publish(&writer, &[(&ffe_path, &ffe), (&apm_path, &apm)]);
    manager.track_target(&target);
    let RemoteConfigUpdate::Add {
        value,
        limiter_index,
    } = manager.fetch_update()
    else {
        panic!("expected APM add")
    };
    assert_eq!(limiter_index, 8);
    assert!(
        value
            .data
            .unwrap()
            .downcast::<DynamicConfigFile>()
            .is_some()
    );
    assert_ffe(manager.fetch_update(), ffe_path.as_str(), "agent-old");
    assert_none(&mut manager);

    let (publisher, handle) = CdnPublisher::new().unwrap();
    manager.set_local_configuration(Some(handle)).unwrap();
    assert_remove(manager.fetch_update(), ffe_path.as_str());
    assert_none(&mut manager);
    assert_eq!(
        manager.local_configuration_state(),
        Some(RemoteConfigDeliveryState::Starting)
    );
    // Even before CDN's first response, explicitly selected CDN must suppress agent FFE.
    let latest = store_shm(2, &ffe_path, configuration("agent-latest")).unwrap();
    publish(&writer, &[(&ffe_path, &latest), (&apm_path, &apm)]);
    assert_none(&mut manager);

    apply(&publisher, "cdn-first");
    assert_ffe(manager.fetch_update(), CDN_PATH, "cdn-first");
    assert_none(&mut manager);
    publisher.failed();
    assert_none(&mut manager);
    assert_eq!(
        manager.local_configuration_state(),
        Some(RemoteConfigDeliveryState::Stale)
    );
    publisher.confirmed();
    assert_none(&mut manager);
    assert_eq!(
        manager.local_configuration_state(),
        Some(RemoteConfigDeliveryState::Ready)
    );

    apply(&publisher, "cdn-next");
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_ffe(manager.fetch_update(), CDN_PATH, "cdn-next");
    assert_none(&mut manager);
    publisher.stopped();
    assert_none(&mut manager);
    assert_eq!(
        manager.local_configuration_state(),
        Some(RemoteConfigDeliveryState::Stopped)
    );
    assert_eq!(
        manager.active_configs.len(),
        2,
        "stopped retains last-good UFC and APM"
    );

    manager.set_local_configuration(None).unwrap();
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_ffe(manager.fetch_update(), ffe_path.as_str(), "agent-latest");
    assert_none(&mut manager);
    assert_eq!(manager.local_configuration_state(), None);
}

#[test]
#[cfg_attr(miri, ignore = "requires native shared memory")]
fn replacement_attachment_clears_old_credentials_and_accepts_revision_one() {
    let mut manager = manager();
    let (first, handle) = CdnPublisher::new().unwrap();
    apply(&first, "first-key");
    manager.set_local_configuration(Some(handle)).unwrap();
    assert_ffe(manager.fetch_update(), CDN_PATH, "first-key");
    assert_none(&mut manager);

    let (second, handle) = CdnPublisher::new().unwrap();
    manager
        .set_local_configuration(Some(handle.clone()))
        .unwrap();
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_none(&mut manager);
    apply(&first, "obsolete-key");
    assert_none(&mut manager);
    assert!(manager.active_configs.is_empty());
    apply(&second, "second-key");
    assert_ffe(manager.fetch_update(), CDN_PATH, "second-key");
    assert_none(&mut manager);

    // Reconnection can deliver the same payload at revision one. Remove then add it.
    manager.set_local_configuration(Some(handle)).unwrap();
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_ffe(manager.fetch_update(), CDN_PATH, "second-key");
    assert_none(&mut manager);
    manager.unload_configs(&[RemoteConfigProduct::FfeFlags]);
    assert_ffe(manager.fetch_update(), CDN_PATH, "second-key");
    assert_none(&mut manager);
    manager.reset_target();
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_none(&mut manager);
    assert_eq!(manager.local_configuration_state(), None);
}

#[test]
#[cfg_attr(miri, ignore = "requires native shared memory")]
fn target_switch_keeps_local_ffe_and_caches_the_complete_agent_directory() {
    let mut manager = manager();
    let first_target = target("first");
    let second_target = target("second");
    let first_writer = RemoteConfigWriter::new(&manager.invariants, &first_target).unwrap();
    let second_writer = RemoteConfigWriter::new(&manager.invariants, &second_target).unwrap();
    let first_path = path("FFE_FLAGS");
    let second_path = path("FFE_FLAGS");
    let first = store_shm(1, &first_path, configuration("first-agent")).unwrap();
    let second = store_shm(1, &second_path, configuration("second-agent")).unwrap();
    publish(&first_writer, &[(&first_path, &first)]);
    publish(&second_writer, &[(&second_path, &second)]);
    let (publisher, handle) = CdnPublisher::new().unwrap();
    apply(&publisher, "cdn");
    manager
        .set_local_configuration(Some(handle.clone()))
        .unwrap();
    manager.track_target(&first_target);
    assert_ffe(manager.fetch_update(), CDN_PATH, "cdn");
    assert_none(&mut manager);
    manager.track_target(&second_target);
    assert_none(&mut manager);
    manager.set_local_configuration(None).unwrap();
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_ffe(manager.fetch_update(), second_path.as_str(), "second-agent");
    assert_none(&mut manager);
    manager.track_target(&first_target);
    assert_remove(manager.fetch_update(), second_path.as_str());
    assert_ffe(manager.fetch_update(), first_path.as_str(), "first-agent");
    assert_none(&mut manager);

    manager.set_local_configuration(Some(handle)).unwrap();
    assert_remove(manager.fetch_update(), first_path.as_str());
    assert_ffe(manager.fetch_update(), CDN_PATH, "cdn");
    manager.reset();
    assert_none(&mut manager);
    assert_eq!(manager.local_configuration_state(), None);
    manager.track_target(&first_target);
    assert_ffe(manager.fetch_update(), first_path.as_str(), "first-agent");
    assert_none(&mut manager);
}
