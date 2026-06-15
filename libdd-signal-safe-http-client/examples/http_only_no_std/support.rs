// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod dns;
mod downloads;
mod linux_net;
mod logger;
mod verifier;

use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

use dns::RustixDnsResolver;
use downloads::DOWNLOADS;
use linux_net::RustixTcpConnector;
use logger::Logger;

pub fn run() -> i32 {
    Logger::line("libdd signal-safe HTTP example");
    Logger::line("runtime: origin + rustix");
    Logger::line("features: no_std, no_alloc, http only");
    Logger::line("dns: /etc/hosts + low_dns UDP/TCP resolver using /etc/resolv.conf");

    let dns = RustixDnsResolver::from_resolv_conf();
    Logger::field_usize("nameservers", dns.name_server_count());
    Logger::field_usize("search domains", dns.search_domain_count());
    Logger::field_usize("ndots", usize::from(dns.ndots()));
    Logger::field_usize("downloads", DOWNLOADS.len());

    let tcp = RustixTcpConnector;

    match block_on(verifier::verify_downloads(&tcp, &dns, DOWNLOADS)) {
        Ok(()) => {
            Logger::line("result: ok");
            0
        }
        Err(error) => {
            Logger::field_str("result", error.as_str());
            1
        }
    }
}

fn block_on<F>(future: F) -> F::Output
where
    F: Future,
{
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}
