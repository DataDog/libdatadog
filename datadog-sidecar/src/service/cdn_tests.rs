// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::service::DynamicInstrumentationConfigState;
use crate::service::blocking::{SidecarTransport, ping, set_cdn_configuration};
use crate::shm_remote_config::{CdnRegistration, RemoteConfigManager, RemoteConfigUpdate};
use libdd_ffe::cdn::{CdnConfig, CdnEndpoint};
use libdd_ffe::rules_based::UniversalFlagConfig;
use libdd_remote_config::{Target, default_registry};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn registration(url: &str) -> CdnRegistration {
    CdnRegistration {
        config: CdnConfig {
            endpoint: CdnEndpoint::custom(url).unwrap(),
            language: "php".into(),
            version: "test".into(),
            poll_interval: Duration::from_millis(50),
            request_timeout: Duration::from_secs(1),
        },
        agent_endpoint: libdd_common::Endpoint::default(),
        target: Arc::new(Target::new(
            format!("test-{}", rand::random::<u64>()),
            "test".into(),
            "".into(),
            vec![],
            vec![],
        )),
    }
}
fn manager(registration: &CdnRegistration) -> RemoteConfigManager {
    let mut manager = RemoteConfigManager::new_with_registry(
        registration.invariants().unwrap(),
        Arc::new(default_registry().with::<UniversalFlagConfig>().unwrap()),
    );
    manager.track_target(&registration.target);
    manager
}

#[test]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
fn cdn_registration_does_not_wait_for_sidecar_reply() {
    let (_server, client) = SeqpacketConn::socketpair().unwrap();
    let mut transport = SidecarTransport::from(client);
    transport
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    // No server reads or replies. A request/reply transaction would time out.
    let start = Instant::now();
    set_cdn_configuration(
        &mut transport,
        Some(registration("http://127.0.0.1:1/config")),
    )
    .unwrap();
    assert!(start.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
async fn cdn_thread_mode_rejects_subscriptions_without_http() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = registration(&format!("http://{}/config", listener.local_addr().unwrap()));
    let (connection, _peer) = SeqpacketConn::socketpair().unwrap();
    let server = SidecarServer::default().with_cdn_fetching(false);
    let mut handler =
        ConnectionSidecarHandler::new(server, OwnedServerConn::new(connection).unwrap());
    handler.set_cdn_configuration(Some(config)).await;
    assert!(handler.cdn_subscription.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native HTTP, IPC and shared memory")]
async fn cdn_http_ipc_delivers_through_ordinary_rc_and_deduplicates() {
    let http = httpmock::MockServer::start_async().await;
    let mock = http.mock_async(|when, then| {
        when.method("GET").path("/config");
        then.status(200).json_body(serde_json::json!({"data":{"type":"universal-flag-configuration","attributes":{
            "format":"SERVER","createdAt":"2026-11-06T00:00:00Z","environment":{"name":"test"},"flags":{}
        }}}));
    }).await;
    let registration = registration(&http.url("/config"));
    let (connection, client) = SeqpacketConn::socketpair().unwrap();
    let server = SidecarServer::default().with_cdn_fetching(true);
    let task = tokio::spawn(server.accept_connection(connection));
    tokio::task::spawn_blocking(move || {
        let mut transport = SidecarTransport::from(client);
        set_cdn_configuration(&mut transport, Some(registration.clone())).unwrap();
        let mut reader = manager(&registration);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let RemoteConfigUpdate::Add { value, .. } = reader.fetch_update() {
                assert_eq!(
                    value.path.as_str(),
                    crate::shm_remote_config::publication::CDN_PATH
                );
                assert!(
                    value
                        .data
                        .unwrap()
                        .downcast::<UniversalFlagConfig>()
                        .is_some()
                );
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(reader.fetch_update(), RemoteConfigUpdate::None));
        let generation = reader.current_remote_config_generation();
        std::thread::sleep(Duration::from_millis(180));
        assert!(matches!(reader.fetch_update(), RemoteConfigUpdate::None));
        assert_eq!(
            reader.current_remote_config_generation(),
            generation,
            "identical 200 responses republished config"
        );
        set_cdn_configuration(&mut transport, None).unwrap();
        ping(&mut transport).unwrap();
        assert!(
            matches!(reader.fetch_update(), RemoteConfigUpdate::None),
            "disconnect must retain last good flags"
        );
    })
    .await
    .unwrap();
    assert!(mock.calls_async().await >= 2);
    task.abort();
}

#[test]
#[cfg_attr(miri, ignore = "requires an actual listener thread and native IPC")]
fn real_thread_listener_does_not_start_cdn_http() {
    let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    http.set_nonblocking(true).unwrap();
    let mut config = crate::config::FromEnv::config();
    config.self_telemetry = false;
    crate::setup::MasterListener::start(config).unwrap();
    let mut transport =
        crate::setup::connect_to_master(i32::try_from(std::process::id()).unwrap()).unwrap();
    set_cdn_configuration(
        &mut transport,
        Some(registration(&format!(
            "http://{}/config",
            http.local_addr().unwrap()
        ))),
    )
    .unwrap();
    ping(&mut transport).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        http.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(transport);
    crate::setup::MasterListener::shutdown().unwrap();
}

#[tokio::test]
#[cfg_attr(miri, ignore = "requires native HTTP and shared memory")]
async fn shared_cdn_poller_cancels_only_after_last_connection() {
    let store = crate::shm_remote_config::publication::PublicationStore::default();
    let configs = super::super::ffe_config::CdnConfigs::default();
    let r = registration("http://127.0.0.1:1/config");
    let first = configs.subscribe(r.clone(), &store).unwrap();
    let second = configs.subscribe(r.clone(), &store).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    let weak = Arc::downgrade(&first);
    drop(first);
    assert!(weak.upgrade().is_some());
    drop(second);
    assert!(weak.upgrade().is_none());
    let replacement = configs.subscribe(r, &store).unwrap();
    assert!(!std::ptr::eq(weak.as_ptr(), Arc::as_ptr(&replacement)));
}

#[test]
#[cfg_attr(miri, ignore = "requires process signals and native shared memory")]
fn cdn_notifications_share_the_published_view_and_require_an_active_opted_in_application() {
    // Isolate process-global signal and in-process callback setup from other tests.
    const CHILD: &str = "LIBDATADOG_CDN_NOTIFY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "service::sidecar_server::cdn_tests::cdn_notifications_share_the_published_view_and_require_an_active_opted_in_application", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    use crate::shm_remote_config::{RemoteConfigReader, ddog_set_rc_notify_fn};
    use libdd_ffe::{cdn::ConfigurationSink, rules_based::Configuration};
    use libdd_remote_config::fetch::ConfigInvariants;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SIGNALS: AtomicUsize = AtomicUsize::new(0);
    static VIEWS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
    extern "C" fn signal(_: libc::c_int) {
        SIGNALS.fetch_add(1, Ordering::SeqCst);
    }
    extern "C" fn inproc(id: *const ConfigInvariants, target: *const Arc<Target>) {
        // SAFETY: publication invokes the callback synchronously with live references.
        let mut reader = unsafe { RemoteConfigReader::new(&*id, &*target) };
        let (_, data) = reader.read();
        VIEWS.lock().unwrap().push(data.to_vec());
    }
    // SAFETY: this isolated process has not started the sidecar runtime yet.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGVTALRM, &action, std::ptr::null_mut()),
            0
        );
        let mut unblocked: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblocked);
        libc::sigaddset(&mut unblocked, libc::SIGVTALRM);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblocked, std::ptr::null_mut()),
            0
        );
        ddog_set_rc_notify_fn(Some(inproc));
    }
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let (local, _peer) = SeqpacketConn::socketpair().unwrap();
        let mut handler = ConnectionSidecarHandler::new(SidecarServer::default().with_cdn_fetching(true), OwnedServerConn::from_async(local.into_async_conn().unwrap(), libdd_ipc::platform::PeerCredentials { pid: std::process::id(), uid: 0, gid: 0 }));
        let r = registration("http://127.0.0.1:1/config");
        handler.set_connection_config(InstanceId::new("notify", "runtime"), SessionConfig::for_test()).await;
        handler.set_cdn_configuration(Some(r.clone())).await;
        let publisher = handler.cdn_subscription.as_ref().unwrap().subscription.publisher.clone();
        let config = |env: &str| Configuration::from_server_response(UniversalFlagConfig::from_json(format!(r#"{{"createdAt":"2026-11-06T00:00:00Z","environment":{{"name":"{env}"}},"flags":{{}}}}"#).into_bytes()).unwrap());
        publisher.apply(config("one")).unwrap();
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 0);
        let mut reader = RemoteConfigReader::new(&r.invariants().unwrap(), &r.target);
        assert_eq!(VIEWS.lock().unwrap().last().unwrap(), reader.read().1);
        handler.set_connection_config(InstanceId::new("notify", "runtime"), SessionConfig { remote_config_enabled: true, endpoint: libdd_common::Endpoint { url: "http://127.0.0.1:1".parse().unwrap(), ..Default::default() }, ..SessionConfig::for_test() }).await;
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 0, "session replacement must await application registration");
        let application = ApplicationConfig { service_name: "notify".into(), env_name: "test".into(), app_version: "".into(), global_tags: vec![], dynamic_instrumentation_state: DynamicInstrumentationConfigState::NotSet };
        handler.set_application(Some(application.clone()), 0).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1, "late application missed the existing publication");
        publisher.apply(config("one")).unwrap();
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1, "unchanged config notified again");
        let mut current = manager(&r);
        while !matches!(current.fetch_update(), RemoteConfigUpdate::None) {}
        handler.set_application(Some(application), current.current_remote_config_generation()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1, "already-current application was notified again");
        publisher.apply(config("two")).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 2);
        assert_eq!(VIEWS.lock().unwrap().last().unwrap(), reader.read().1);
        handler.set_application(None, 0).await;
        publisher.apply(config("three")).unwrap();
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 2, "inactive application received a signal");
        handler.server.shutdown();
    });
}
