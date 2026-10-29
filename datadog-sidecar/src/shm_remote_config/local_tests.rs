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

fn mixed_source_wait_fixture() -> (RemoteConfigManager, RemoteConfigWriter, CdnPublisher) {
    let mut manager = manager();
    let target = target("mixed-source-wait");
    let writer = RemoteConfigWriter::new(&manager.invariants, &target).unwrap();
    publish(&writer, &[]);
    manager.track_target(&target);
    let (publisher, handle) = CdnPublisher::new().unwrap();
    manager.set_local_configuration(Some(handle)).unwrap();
    (manager, writer, publisher)
}

#[test]
#[cfg_attr(miri, ignore = "requires native shared memory and timed waits")]
fn mixed_source_wait_observes_pending_agent_changes_without_consuming_updates() {
    let (mut manager, writer, _publisher) = mixed_source_wait_fixture();
    let apm_path = path("APM_TRACING");
    let apm = store_shm(
        1,
        &apm_path,
        serde_json::to_vec(&dummy_dynamic_config(true)).unwrap(),
    )
    .unwrap();
    publish(&writer, &[(&apm_path, &apm)]);

    // The first wait must also discover an agent target not yet fetched.
    let started = std::time::Instant::now();
    manager.wait_for_change(Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_millis(500));
    let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() else {
        panic!("wait must leave the pending agent add for fetch_update");
    };
    assert_eq!(value.path.as_str(), apm_path.as_str());
    assert_none(&mut manager);

    // A subsequent generation must be observed too, even without futex support.
    publish(&writer, &[]);
    manager.wait_for_change(Duration::ZERO);
    assert_remove(manager.fetch_update(), apm_path.as_str());
    assert_none(&mut manager);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "requires native shared memory, threads, and timed waits"
)]
fn mixed_source_wait_bounds_agent_delivery_while_local_source_is_idle() {
    for state in [
        RemoteConfigDeliveryState::Starting,
        RemoteConfigDeliveryState::Ready,
        RemoteConfigDeliveryState::Stopped,
    ] {
        let (mut manager, writer, publisher) = mixed_source_wait_fixture();
        if state != RemoteConfigDeliveryState::Starting {
            apply(&publisher, "unchanged-cdn");
            assert_ffe(manager.fetch_update(), CDN_PATH, "unchanged-cdn");
        }
        if state == RemoteConfigDeliveryState::Stopped {
            publisher.stopped();
        }
        assert_none(&mut manager);
        assert_eq!(manager.local_configuration_state(), Some(state));
        // Prime each notification cursor directly: the first wait can otherwise
        // return immediately for the initial generation and mask the regression.
        manager
            .active_reader
            .as_mut()
            .unwrap()
            .0
            .wait_for_change(Duration::ZERO);
        manager
            .local_reader
            .as_mut()
            .unwrap()
            .wait_for_change(Duration::ZERO);

        let apm_path = path("APM_TRACING");
        let apm = store_shm(
            1,
            &apm_path,
            serde_json::to_vec(&dummy_dynamic_config(true)).unwrap(),
        )
        .unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                publish(&writer, &[(&apm_path, &apm)]);
            });
            let started = std::time::Instant::now();
            manager.wait_for_change(Duration::from_secs(2));
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "idle {state:?} local source must not hide the agent update"
            );
        });
        let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() else {
            panic!("wait must leave the new agent config for fetch_update");
        };
        assert_eq!(value.path.as_str(), apm_path.as_str());
        assert_none(&mut manager);
        assert_eq!(manager.local_configuration_state(), Some(state));
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires native shared memory and timed waits")]
fn mixed_source_wait_accepts_zero_timeout_and_does_not_spin_when_idle() {
    let (mut manager, _writer, _publisher) = mixed_source_wait_fixture();
    assert_none(&mut manager);
    manager.wait_for_change(Duration::ZERO);
    assert_none(&mut manager);

    let started = std::time::Instant::now();
    for _ in 0..8 {
        manager.wait_for_change(Duration::from_millis(5));
    }
    let elapsed = started.elapsed();
    // Idle positive waits must not become a busy loop. Do not bound scheduler
    // oversleep here; the mixed-source delivery test checks agent responsiveness.
    assert!(elapsed >= Duration::from_millis(20), "{elapsed:?}");
    assert_none(&mut manager);
}

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

#[test]
#[cfg(unix)]
#[cfg_attr(miri, ignore = "requires child processes and native shared memory")]
fn agent_directory_replacement_preserves_local_ffe_and_reapplies_agent_configs() {
    const CHILD: &str = "DD_TEST_LOCAL_FFE_AGENT_REPLACEMENT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "shm_remote_config::local_tests::agent_directory_replacement_preserves_local_ffe_and_reapplies_agent_configs"])
            .env(CHILD, "1").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let mut manager = manager();
    let target = target("local-ffe-agent-restart");
    let apm_path = path("APM_TRACING");
    let content = serde_json::to_vec(&dummy_dynamic_config(true)).unwrap();
    let old_file = store_shm(1, &apm_path, content.clone()).unwrap();
    let old_writer = RemoteConfigWriter::new(&manager.invariants, &target).unwrap();
    publish(&old_writer, &[(&apm_path, &old_file)]);
    manager.track_target(&target);
    let (publisher, handle) = CdnPublisher::new().unwrap();
    apply(&publisher, "unchanged-cdn");
    manager.set_local_configuration(Some(handle)).unwrap();
    assert_ffe(manager.fetch_update(), CDN_PATH, "unchanged-cdn");
    assert!(matches!(
        manager.fetch_update(),
        RemoteConfigUpdate::Add {
            limiter_index: 8,
            ..
        }
    ));
    assert_none(&mut manager);
    let previous_generation = manager.current_remote_config_generation();

    // The same agent file/limiter index belongs to a new arena; CDN is independent.
    let new_file = store_shm(1, &apm_path, content).unwrap();
    let new_writer = RemoteConfigWriter::new(&manager.invariants, &target).unwrap();
    publish(&new_writer, &[(&apm_path, &new_file)]);
    drop((old_writer, old_file));
    assert_remove(manager.fetch_update(), apm_path.as_str());
    let RemoteConfigUpdate::Add {
        value,
        limiter_index,
    } = manager.fetch_update()
    else {
        panic!("expected agent config to be reapplied");
    };
    assert_eq!(value.path.as_str(), apm_path.as_str());
    assert_eq!(limiter_index, 8);
    assert_none(&mut manager);
    assert!(manager.current_remote_config_generation() > previous_generation);
    assert_eq!(manager.active_configs.len(), 2);
    assert_eq!(
        manager.local_configuration_state(),
        Some(RemoteConfigDeliveryState::Ready)
    );

    // Suppressing arena-only reapplication must not suppress real CDN replacements.
    apply(&publisher, "changed-cdn");
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_ffe(manager.fetch_update(), CDN_PATH, "changed-cdn");
    assert_none(&mut manager);
    manager.set_local_configuration(None).unwrap();
    assert_remove(manager.fetch_update(), CDN_PATH);
    assert_none(&mut manager);
    assert_eq!(manager.active_configs.len(), 1);
}
