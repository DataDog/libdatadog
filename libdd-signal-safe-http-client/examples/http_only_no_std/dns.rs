// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str,
    sync::atomic::{AtomicU16, Ordering},
};

use libdd_signal_safe_http_client::io::{
    embedded_io::ErrorKind,
    embedded_nal_async::{AddrType, Dns},
};
use low_dns::{DnsQuestion, Header, HeaderKind, Name, Packet, Rdata, ResponseCode};
use rustix::{
    event::{self, PollFd, PollFlags, Timespec},
    fd::OwnedFd,
    fs::{self, Mode, OFlags},
    io,
    net::{self, AddressFamily, RecvFlags, SendFlags, SocketType},
};

const MAX_NAME_SERVERS: usize = 3;
const MAX_SEARCH_DOMAINS: usize = 6;
const MAX_CNAME_DEPTH: usize = 8;
const RESOLV_CONF_PATH: &str = "/etc/resolv.conf";
const HOSTS_PATH: &str = "/etc/hosts";
const RESOLV_CONF_BUFFER_LEN: usize = 2048;
const HOSTS_BUFFER_LEN: usize = 8192;
const DNS_PORT: u16 = 53;
const DEFAULT_NDOTS: u8 = 1;
const DNS_TIMEOUT: Timespec = Timespec {
    tv_sec: 2,
    tv_nsec: 0,
};

pub(super) struct RustixDnsResolver {
    name_servers: [Option<SocketAddr>; MAX_NAME_SERVERS],
    search_domains: [[u8; low_dns::name::MAX_NAME_LENGTH]; MAX_SEARCH_DOMAINS],
    search_domain_lens: [usize; MAX_SEARCH_DOMAINS],
    ndots: u8,
    next_id: AtomicU16,
}

impl RustixDnsResolver {
    pub(super) fn from_resolv_conf() -> Self {
        let mut resolver = Self {
            name_servers: [None; MAX_NAME_SERVERS],
            search_domains: [[0; low_dns::name::MAX_NAME_LENGTH]; MAX_SEARCH_DOMAINS],
            search_domain_lens: [0; MAX_SEARCH_DOMAINS],
            ndots: DEFAULT_NDOTS,
            next_id: AtomicU16::new(0x4400),
        };

        let mut buffer = [0_u8; RESOLV_CONF_BUFFER_LEN];
        if let Ok(len) = read_file(RESOLV_CONF_PATH, &mut buffer) {
            parse_resolv_conf(&buffer[..len], &mut resolver);
        }

        resolver
    }

    pub(super) fn name_server_count(&self) -> usize {
        self.name_servers
            .iter()
            .filter(|name_server| name_server.is_some())
            .count()
    }

    pub(super) fn search_domain_count(&self) -> usize {
        self.search_domain_lens
            .iter()
            .filter(|len| **len != 0)
            .count()
    }

    pub(super) const fn ndots(&self) -> u8 {
        self.ndots
    }

    fn push_name_server(&mut self, addr: SocketAddr) {
        for slot in &mut self.name_servers {
            if slot.is_none() {
                *slot = Some(addr);
                return;
            }
        }
    }

    const fn clear_search_domains(&mut self) {
        self.search_domain_lens = [0; MAX_SEARCH_DOMAINS];
    }

    fn push_search_domain(&mut self, domain: &[u8]) {
        let domain = strip_trailing_dot_bytes(domain);
        if !valid_host_bytes(domain) {
            return;
        }

        for (slot, len) in self
            .search_domains
            .iter_mut()
            .zip(self.search_domain_lens.iter_mut())
        {
            if *len == 0 {
                slot[..domain.len()].copy_from_slice(domain);
                *len = domain.len();
                return;
            }
        }
    }

    fn resolve_ipv4(&self, host: &str) -> Result<Ipv4Addr, ErrorKind> {
        validate_host(host)?;

        let mut hosts_buffer = [0_u8; HOSTS_BUFFER_LEN];
        let hosts_len = read_file(HOSTS_PATH, &mut hosts_buffer).unwrap_or_default();
        let hosts = &hosts_buffer[..hosts_len];
        let mut candidate_buffer = [0_u8; low_dns::name::MAX_NAME_LENGTH];
        let absolute_first = count_dots(host.as_bytes()) >= self.ndots || has_trailing_dot(host);

        if absolute_first {
            if let Ok(ip) = self.resolve_candidate(strip_trailing_dot(host), hosts) {
                return Ok(ip);
            }
        }

        if !has_trailing_dot(host) {
            for domain in self.search_domains() {
                let Some(candidate) = append_search_domain(host, domain, &mut candidate_buffer)
                else {
                    continue;
                };
                if let Ok(ip) = self.resolve_candidate(candidate, hosts) {
                    return Ok(ip);
                }
            }
        }

        if !absolute_first {
            return self.resolve_candidate(host, hosts);
        }

        Err(ErrorKind::AddrNotAvailable)
    }

    fn resolve_candidate(&self, host: &str, hosts: &[u8]) -> Result<Ipv4Addr, ErrorKind> {
        let mut current_buffer = [0_u8; low_dns::name::MAX_NAME_LENGTH];
        let mut cname_buffer = [0_u8; low_dns::name::MAX_NAME_LENGTH];
        let mut current_len = copy_host(host, &mut current_buffer)?;

        for _ in 0..MAX_CNAME_DEPTH {
            let current = str::from_utf8(&current_buffer[..current_len])
                .map_err(|_| ErrorKind::InvalidInput)?;
            if let Some(ip) = resolve_hosts_ipv4(hosts, current) {
                return Ok(ip);
            }

            let mut saw_cname = false;

            for name_server in self.name_servers.iter().flatten() {
                match self.query_a_or_cname(*name_server, current, &mut cname_buffer) {
                    Ok(QueryAnswer::A(ip)) => return Ok(ip),
                    Ok(QueryAnswer::Cname(len)) => {
                        current_buffer[..len].copy_from_slice(&cname_buffer[..len]);
                        current_len = len;
                        saw_cname = true;
                        break;
                    }
                    Err(_) => {}
                }
            }

            if !saw_cname {
                return Err(ErrorKind::AddrNotAvailable);
            }
        }

        Err(ErrorKind::InvalidData)
    }

    const fn search_domains(&self) -> SearchDomains<'_> {
        SearchDomains {
            domains: &self.search_domains,
            lens: &self.search_domain_lens,
            next: 0,
        }
    }

    fn query_a_or_cname(
        &self,
        name_server: SocketAddr,
        host: &str,
        cname_buffer: &mut [u8; low_dns::name::MAX_NAME_LENGTH],
    ) -> Result<QueryAnswer, ErrorKind> {
        let id = self.next_query_id();
        let mut query_name_buffer = Name::max_buffer();
        let mut query_packet_buffer = Packet::max_buffer();
        let query = build_a_query(id, host, &mut query_name_buffer, &mut query_packet_buffer);

        let fd = net::socket(
            AddressFamily::INET,
            SocketType::DGRAM,
            Some(net::ipproto::UDP),
        )
        .map_err(map_errno)?;

        net::sendto(&fd, query.as_bytes(), SendFlags::empty(), &name_server).map_err(map_errno)?;
        wait_readable(&fd)?;

        let mut response_buffer = Packet::max_buffer();
        let (read, _, _) =
            net::recvfrom(&fd, &mut response_buffer, RecvFlags::empty()).map_err(map_errno)?;
        let response =
            Packet::parse(&response_buffer[..read]).map_err(|_| ErrorKind::InvalidData)?;
        if response.header().truncated() {
            return Self::query_a_or_cname_tcp(
                name_server,
                id,
                query.as_bytes(),
                host,
                cname_buffer,
            );
        }

        parse_query_response(&response, id, host, cname_buffer)
    }

    fn query_a_or_cname_tcp(
        name_server: SocketAddr,
        id: u16,
        query: &[u8],
        host: &str,
        cname_buffer: &mut [u8; low_dns::name::MAX_NAME_LENGTH],
    ) -> Result<QueryAnswer, ErrorKind> {
        let fd = net::socket(
            AddressFamily::INET,
            SocketType::STREAM,
            Some(net::ipproto::TCP),
        )
        .map_err(map_errno)?;
        net::connect(&fd, &name_server).map_err(map_errno)?;
        send_dns_tcp_query(&fd, query)?;
        wait_readable(&fd)?;

        let mut response_buffer = Packet::max_buffer();
        let read = recv_dns_tcp_response(&fd, &mut response_buffer)?;
        let response =
            Packet::parse(&response_buffer[..read]).map_err(|_| ErrorKind::InvalidData)?;
        parse_query_response(&response, id, host, cname_buffer)
    }

    fn next_query_id(&self) -> u16 {
        let mut id = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if id == 0 {
            id = 1;
        }
        id
    }
}

enum QueryAnswer {
    A(Ipv4Addr),
    Cname(usize),
}

struct SearchDomains<'a> {
    domains: &'a [[u8; low_dns::name::MAX_NAME_LENGTH]; MAX_SEARCH_DOMAINS],
    lens: &'a [usize; MAX_SEARCH_DOMAINS],
    next: usize,
}

impl<'a> Iterator for SearchDomains<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < MAX_SEARCH_DOMAINS {
            let index = self.next;
            self.next += 1;
            let len = self.lens[index];
            if len != 0 {
                return Some(&self.domains[index][..len]);
            }
        }

        None
    }
}

fn build_a_query<'a>(
    id: u16,
    host: &'a str,
    name_buffer: &'a mut [u8; low_dns::name::MAX_NAME_LENGTH],
    packet_buffer: &'a mut [u8; low_dns::packet::MAX_PACKET_SIZE],
) -> Packet<'a> {
    let name = Name::from_str_into_buf(host, name_buffer);
    Packet::builder(packet_buffer)
        .header(Header::builder().id(id).recursion_desired(true).build())
        .question(DnsQuestion::a(name))
        .build()
}

fn parse_query_response(
    response: &Packet<'_>,
    id: u16,
    queried: &str,
    cname_buffer: &mut [u8; low_dns::name::MAX_NAME_LENGTH],
) -> Result<QueryAnswer, ErrorKind> {
    if response.header().kind() != HeaderKind::Response
        || response.header().id() != id
        || response.header().response_code() != ResponseCode::NoError
    {
        return Err(ErrorKind::AddrNotAvailable);
    }

    let mut cname_len = None;
    for answer in response.answers() {
        if *answer.name() != *queried {
            continue;
        }

        match answer.rdata() {
            Rdata::A { ip } => return Ok(QueryAnswer::A(*ip)),
            Rdata::CNAME { name } => {
                let target = name
                    .as_str(cname_buffer)
                    .map_err(|_| ErrorKind::InvalidData)?;
                validate_host(target)?;
                cname_len = Some(target.len());
            }
            _ => {}
        }
    }

    cname_len
        .map(QueryAnswer::Cname)
        .ok_or(ErrorKind::AddrNotAvailable)
}

fn send_dns_tcp_query(fd: &OwnedFd, query: &[u8]) -> Result<(), ErrorKind> {
    let len = u16::try_from(query.len()).map_err(|_| ErrorKind::InvalidInput)?;
    send_all(fd, &len.to_be_bytes())?;
    send_all(fd, query)
}

fn recv_dns_tcp_response(
    fd: &OwnedFd,
    response_buffer: &mut [u8; low_dns::packet::MAX_PACKET_SIZE],
) -> Result<usize, ErrorKind> {
    let mut len_buffer = [0_u8; 2];
    recv_exact(fd, &mut len_buffer)?;

    let len = usize::from(u16::from_be_bytes(len_buffer));
    if len > response_buffer.len() {
        return Err(ErrorKind::InvalidData);
    }

    recv_exact(fd, &mut response_buffer[..len])?;
    Ok(len)
}

fn send_all(fd: &OwnedFd, bytes: &[u8]) -> Result<(), ErrorKind> {
    let mut written_total = 0_usize;
    while written_total < bytes.len() {
        match net::send(fd, &bytes[written_total..], SendFlags::empty()) {
            Ok(0) => return Err(ErrorKind::WriteZero),
            Ok(written) => {
                written_total = written_total
                    .checked_add(written)
                    .ok_or(ErrorKind::OutOfMemory)?;
            }
            Err(io::Errno::INTR) => {}
            Err(errno) => return Err(map_errno(errno)),
        }
    }

    Ok(())
}

fn recv_exact(fd: &OwnedFd, buffer: &mut [u8]) -> Result<(), ErrorKind> {
    let mut read_total = 0_usize;
    while read_total < buffer.len() {
        wait_readable(fd)?;
        match net::recv(fd, &mut buffer[read_total..], RecvFlags::empty()) {
            Ok((0, _)) => return Err(ErrorKind::InvalidData),
            Ok((read, _)) => {
                read_total = read_total.checked_add(read).ok_or(ErrorKind::OutOfMemory)?;
            }
            Err(io::Errno::INTR) => {}
            Err(errno) => return Err(map_errno(errno)),
        }
    }

    Ok(())
}

impl Dns for RustixDnsResolver {
    type Error = ErrorKind;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        match addr_type {
            AddrType::IPv4 | AddrType::Either => self.resolve_ipv4(host).map(IpAddr::V4),
            AddrType::IPv6 => Err(ErrorKind::Unsupported),
        }
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(ErrorKind::Unsupported)
    }
}

fn read_file(path: &str, buffer: &mut [u8]) -> Result<usize, ErrorKind> {
    let fd = fs::openat(
        fs::CWD,
        path,
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(map_errno)?;

    let mut used = 0_usize;
    while used < buffer.len() {
        let read = io::read(&fd, &mut buffer[used..]).map_err(map_errno)?;
        if read == 0 {
            break;
        }
        used = used.checked_add(read).ok_or(ErrorKind::OutOfMemory)?;
    }

    Ok(used)
}

fn parse_resolv_conf(bytes: &[u8], resolver: &mut RustixDnsResolver) {
    for line in Lines::new(bytes) {
        let line = strip_comment(line);
        let mut tokens = Tokens::new(line);
        let Some(kind) = tokens.next() else {
            continue;
        };

        match kind {
            b"nameserver" => {
                let Some(addr) = tokens.next().and_then(parse_ipv4) else {
                    continue;
                };
                resolver.push_name_server(SocketAddr::new(IpAddr::V4(addr), DNS_PORT));
            }
            b"search" => {
                resolver.clear_search_domains();
                for domain in tokens {
                    resolver.push_search_domain(domain);
                }
            }
            b"domain" => {
                let Some(domain) = tokens.next() else {
                    continue;
                };
                resolver.clear_search_domains();
                resolver.push_search_domain(domain);
            }
            b"options" => {
                for option in tokens {
                    if let Some(ndots) = parse_ndots(option) {
                        resolver.ndots = ndots;
                    }
                }
            }
            _ => {}
        }
    }
}

fn resolve_hosts_ipv4(bytes: &[u8], host: &str) -> Option<Ipv4Addr> {
    let host = strip_trailing_dot(host).as_bytes();
    for line in Lines::new(bytes) {
        let line = strip_comment(line);
        let mut tokens = Tokens::new(line);
        let Some(ip) = tokens.next().and_then(parse_ipv4) else {
            continue;
        };

        for name in tokens {
            if host_bytes_eq(name, host) {
                return Some(ip);
            }
        }
    }

    None
}

fn strip_comment(line: &[u8]) -> &[u8] {
    for (index, byte) in line.iter().enumerate() {
        if *byte == b'#' || *byte == b';' {
            return &line[..index];
        }
    }

    line
}

fn parse_ipv4(bytes: &[u8]) -> Option<Ipv4Addr> {
    let mut octets = [0_u8; 4];
    let mut part = 0_usize;
    let mut value = 0_u16;
    let mut has_digit = false;

    for byte in bytes {
        if byte.is_ascii_digit() {
            value = value
                .checked_mul(10)?
                .checked_add(u16::from(*byte - b'0'))?;
            if value > u16::from(u8::MAX) {
                return None;
            }
            has_digit = true;
        } else if *byte == b'.' {
            if !has_digit || part >= 3 {
                return None;
            }
            octets[part] = u8::try_from(value).ok()?;
            part += 1;
            value = 0;
            has_digit = false;
        } else {
            return None;
        }
    }

    if !has_digit || part != 3 {
        return None;
    }
    octets[part] = u8::try_from(value).ok()?;

    Some(Ipv4Addr::from(octets))
}

fn parse_ndots(bytes: &[u8]) -> Option<u8> {
    let rest = bytes.strip_prefix(b"ndots:")?;
    let mut value = 0_u8;
    let mut has_digit = false;

    for byte in rest {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(*byte - b'0')?;
        has_digit = true;
    }

    if has_digit {
        Some(value.min(15))
    } else {
        None
    }
}

fn validate_host(host: &str) -> Result<(), ErrorKind> {
    if valid_host_bytes(strip_trailing_dot(host).as_bytes()) {
        Ok(())
    } else {
        Err(ErrorKind::InvalidInput)
    }
}

fn valid_host_bytes(bytes: &[u8]) -> bool {
    let bytes = strip_trailing_dot_bytes(bytes);
    if bytes.is_empty() || bytes.len() > low_dns::name::MAX_NAME_LENGTH {
        return false;
    }

    for label in bytes.split(|byte| *byte == b'.') {
        if label.is_empty() || label.len() > low_dns::name::MAX_LABEL_LENGTH {
            return false;
        }
    }

    true
}

fn copy_host(
    host: &str,
    buffer: &mut [u8; low_dns::name::MAX_NAME_LENGTH],
) -> Result<usize, ErrorKind> {
    let host = strip_trailing_dot(host);
    validate_host(host)?;
    if host.len() > buffer.len() {
        return Err(ErrorKind::InvalidInput);
    }

    buffer[..host.len()].copy_from_slice(host.as_bytes());
    Ok(host.len())
}

fn append_search_domain<'a>(
    host: &str,
    domain: &[u8],
    buffer: &'a mut [u8; low_dns::name::MAX_NAME_LENGTH],
) -> Option<&'a str> {
    let host = strip_trailing_dot(host).as_bytes();
    let domain = strip_trailing_dot_bytes(domain);
    let len = host.len().checked_add(1)?.checked_add(domain.len())?;
    if host.is_empty() || domain.is_empty() || len > buffer.len() {
        return None;
    }

    buffer[..host.len()].copy_from_slice(host);
    buffer[host.len()] = b'.';
    buffer[host.len() + 1..len].copy_from_slice(domain);
    if !valid_host_bytes(&buffer[..len]) {
        return None;
    }

    str::from_utf8(&buffer[..len]).ok()
}

fn count_dots(bytes: &[u8]) -> u8 {
    let mut count = 0_u8;
    for byte in bytes {
        if *byte == b'.' {
            count = count.saturating_add(1);
        }
    }

    count
}

const fn has_trailing_dot(host: &str) -> bool {
    matches!(host.as_bytes().last(), Some(b'.'))
}

fn strip_trailing_dot(host: &str) -> &str {
    match host.strip_suffix('.') {
        Some(stripped) => stripped,
        None => host,
    }
}

fn strip_trailing_dot_bytes(bytes: &[u8]) -> &[u8] {
    if matches!(bytes.last(), Some(b'.')) {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    }
}

fn host_bytes_eq(left: &[u8], right: &[u8]) -> bool {
    let left = strip_trailing_dot_bytes(left);
    let right = strip_trailing_dot_bytes(right);
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn wait_readable(fd: &OwnedFd) -> Result<(), ErrorKind> {
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    let ready = event::poll(&mut fds, Some(&DNS_TIMEOUT)).map_err(map_errno)?;
    if ready == 0 {
        return Err(ErrorKind::TimedOut);
    }

    let revents = fds[0].revents();
    if revents.intersects(PollFlags::IN) {
        Ok(())
    } else {
        Err(ErrorKind::Other)
    }
}

const fn map_errno(errno: io::Errno) -> ErrorKind {
    match errno {
        io::Errno::INTR => ErrorKind::Interrupted,
        io::Errno::CONNREFUSED => ErrorKind::ConnectionRefused,
        io::Errno::CONNRESET => ErrorKind::ConnectionReset,
        io::Errno::NOENT => ErrorKind::NotFound,
        io::Errno::ADDRINUSE => ErrorKind::AddrInUse,
        io::Errno::ADDRNOTAVAIL => ErrorKind::AddrNotAvailable,
        io::Errno::INVAL => ErrorKind::InvalidInput,
        io::Errno::TIMEDOUT => ErrorKind::TimedOut,
        io::Errno::NOMEM => ErrorKind::OutOfMemory,
        _ => ErrorKind::Other,
    }
}

struct Lines<'a> {
    bytes: &'a [u8],
}

impl<'a> Lines<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() {
            return None;
        }

        for (index, byte) in self.bytes.iter().enumerate() {
            if *byte == b'\n' {
                let line = &self.bytes[..index];
                self.bytes = &self.bytes[index + 1..];
                return Some(line);
            }
        }

        let line = self.bytes;
        self.bytes = &[];
        Some(line)
    }
}

struct Tokens<'a> {
    bytes: &'a [u8],
}

impl<'a> Tokens<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.bytes = trim_start(self.bytes);
        if self.bytes.is_empty() {
            return None;
        }

        for (index, byte) in self.bytes.iter().enumerate() {
            if is_space(*byte) {
                let token = &self.bytes[..index];
                self.bytes = &self.bytes[index + 1..];
                return Some(token);
            }
        }

        let token = self.bytes;
        self.bytes = &[];
        Some(token)
    }
}

const fn trim_start(mut bytes: &[u8]) -> &[u8] {
    while let Some((byte, rest)) = bytes.split_first() {
        if !is_space(*byte) {
            break;
        }
        bytes = rest;
    }

    bytes
}

const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r')
}
