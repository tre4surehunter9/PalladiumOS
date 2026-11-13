// Copyright (c) 2026 tre4surehunter9
//! Networking: a network card, the smoltcp TCP/IP stack, and the
//! interfaces it runs:
//!
//! - `eth0` on the RTL8139 card, configured by DHCP (QEMU's user network
//!   gives 10.0.2.15, with the gateway and DNS at 10.0.2.2 / 10.0.2.3),
//! - `lo`, the loopback interface (127.0.0.1): programs on PalladiumOS
//!   talking to each other, network card or not.
//!
//! The stack runs when someone needs it: blocking socket calls, the timer
//! (for retransmissions), idle time and the card's interrupt all call
//! `poll`, which moves packets and wakes processes waiting on sockets.

pub mod card;
pub mod pci;
pub mod rtl8139;
pub mod virtio_net;
pub mod socket;
pub mod unix;
pub mod netlink;

use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Loopback, Medium};
use smoltcp::socket::dhcpv4;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr, Ipv6Address, Ipv6Cidr};
use spin::Mutex;

use card::Card;

/// The wait key for processes blocked on a socket: every poll that changes
/// anything wakes them, and they check their socket again.
pub const WAIT_KEY: u64 = 0x7E7E_0000_4E45_5400;

/// An address configuration for eth0.
#[derive(Clone, Copy, Debug)]
pub struct IpConfig {
    pub address: Ipv4Cidr,
    pub gateway: Option<Ipv4Address>,
    pub dns: Option<Ipv4Address>,
}

pub struct Eth {
    pub iface: Interface,
    pub dev: Card,
    pub sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    pub config: Option<IpConfig>,
}

pub struct Lo {
    pub iface: Interface,
    pub dev: card::Counted<Loopback>,
    pub sockets: SocketSet<'static>,
}

pub struct Net {
    pub eth: Option<Eth>,
    pub lo: Lo,
    /// Closed TCP sockets still saying goodbye (removed once done).
    pub closing: alloc::vec::Vec<(socket::Which, SocketHandle)>,
}

pub static NET: Mutex<Option<Net>> = Mutex::new(None);

/// Each interface's traffic (0 lo, 1 eth0): frames and bytes received and
/// sent, and frames dropped, for /proc/net/dev (ifconfig, ip -s).
pub struct Stats {
    pub rx_packets: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub tx_packets: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub tx_dropped: AtomicU64,
}

impl Stats {
    const fn new() -> Stats {
        Stats {
            rx_packets: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            tx_dropped: AtomicU64::new(0),
        }
    }

    pub fn received(&self, bytes: usize) {
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn sent(&self, bytes: usize) {
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

pub static STATS: [Stats; 2] = [Stats::new(), Stats::new()];

/// eth0 is up (ifconfig eth0 down: no frames in or out until it's up again).
pub static ETH_UP: AtomicBool = AtomicBool::new(true);

/// Set eth0's IPv4 address and prefix length by hand (ifconfig eth0 ADDR
/// netmask MASK), keeping its gateway and DNS server if still on the
/// network. False: no eth0.
pub fn set_ipv4(addr: Option<[u8; 4]>, prefix: Option<u8>) -> bool {
    let mut guard = NET.lock();
    let Some(eth) = guard.as_mut().and_then(|n| n.eth.as_mut()) else { return false };
    let old = eth.config;
    let a = addr.map(Ipv4Address::from_octets).or(old.map(|c| c.address.address())).unwrap_or(Ipv4Address::UNSPECIFIED);
    // Without a netmask: the old one, or the address class's (as Linux).
    let p = prefix.or(old.map(|c| c.address.prefix_len())).unwrap_or(match a.octets()[0] {
        0..=127 => 8,
        128..=191 => 16,
        _ => 24,
    });
    let cidr = Ipv4Cidr::new(a, p);
    let gateway = old.and_then(|c| c.gateway).filter(|g| cidr.contains_addr(g));
    if gateway.is_none() {
        eth.iface.routes_mut().remove_default_ipv4_route();
    }
    apply(eth, IpConfig { address: cidr, gateway, dns: old.and_then(|c| c.dns) });
    true
}
/// Set by the card's interrupt: there's something to poll for.
static IRQ: AtomicBool = AtomicBool::new(false);
/// The card's IRQ line (0 = none) and I/O ports, for the interrupt handler.
static CARD_IRQ: AtomicU64 = AtomicU64::new(0);
static CARD_IO: AtomicU64 = AtomicU64::new(0);
/// A virtio-net card's ISR register (read to acknowledge its interrupt).
static CARD_ISR: AtomicU64 = AtomicU64::new(0);
/// Tick of the last timer-driven poll.
static LAST_POLL: AtomicU64 = AtomicU64::new(0);

pub fn now() -> Instant {
    Instant::from_millis((crate::process::ticks() * crate::process::TICK_NS / 1_000_000) as i64)
}

fn seed() -> u64 {
    crate::random::next_u64()
}

/// Find the card and set up the interfaces. Returns the card's MAC address
/// and IRQ, if there is one.
pub fn init() -> Option<([u8; 6], u8, &'static str)> {
    let eth = Card::find().map(|mut dev| {
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(dev.mac())));
        config.random_seed = seed();
        // IPv6: a global address from the router's advertisements (SLAAC:
        // QEMU's user network sends them, prefix fec0::/64)...
        config.slaac = true;
        let mut iface = Interface::new(config, &mut dev, now());
        // ...and the link-local one, from the MAC address (EUI-64).
        let link_local = link_local(dev.mac());
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(link_local, 64)));
        });
        let mut sockets = SocketSet::new(vec![]);
        let dhcp = sockets.add(dhcpv4::Socket::new());
        Eth { iface, dev, sockets, dhcp, config: None }
    });
    let found = eth.as_ref().map(|e| (e.dev.mac(), e.dev.irq(), e.dev.name()));
    match eth.as_ref().map(|e| &e.dev) {
        Some(Card::Rtl(r)) => CARD_IO.store(r.io_base() as u64, Ordering::SeqCst),
        Some(Card::Virtio(v)) => CARD_ISR.store(v.isr, Ordering::SeqCst),
        None => {}
    }
    if let Some((_, irq, _)) = found.filter(|f| f.1 != 0 && f.1 != 0xff) {
        CARD_IRQ.store(irq as u64, Ordering::SeqCst);
        crate::interrupts::unmask_irq(irq);
    }

    let mut dev = card::Counted(Loopback::new(Medium::Ip), &STATS[0]);
    let mut config = Config::new(HardwareAddress::Ip);
    config.random_seed = seed();
    let mut iface = Interface::new(config, &mut dev, now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8));
        let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(Ipv6Address::LOCALHOST, 128)));
    });
    let lo = Lo { iface, dev, sockets: SocketSet::new(vec![]) };
    *NET.lock() = Some(Net { eth, lo, closing: alloc::vec::Vec::new() });
    found
}

/// The card's interrupt: acknowledge it and note there's work.
pub fn interrupt() {
    let io = CARD_IO.load(Ordering::SeqCst) as u16;
    if io != 0 {
        rtl8139::ack_interrupt(io);
    }
    virtio_net::ack_interrupt(CARD_ISR.load(Ordering::SeqCst));
    IRQ.store(true, Ordering::SeqCst);
}

pub fn card_irq() -> u8 {
    CARD_IRQ.load(Ordering::SeqCst) as u8
}

/// Move packets for both interfaces; wake socket waiters if anything
/// happened. Safe to call from any process context (not interrupts).
pub fn poll() {
    let Some(mut guard) = NET.try_lock() else { return };
    let Some(net) = guard.as_mut() else { return };
    let t = now();
    let mut changed = false;
    if let Some(eth) = net.eth.as_mut() {
        changed |= eth.iface.poll(t, &mut eth.dev, &mut eth.sockets) != smoltcp::iface::PollResult::None;
        // DHCP: (re)configure eth0.
        let event = eth.sockets.get_mut::<dhcpv4::Socket>(eth.dhcp).poll();
        match event {
            Some(dhcpv4::Event::Configured(c)) => {
                let cfg = IpConfig { address: c.address, gateway: c.router, dns: c.dns_servers.first().copied() };
                apply(eth, cfg);
                changed = true;
            }
            Some(dhcpv4::Event::Deconfigured) => {
                // (Only the IPv4 address: IPv6's come from elsewhere.)
                eth.iface.update_ip_addrs(|a| a.retain(|c| !matches!(c, IpCidr::Ipv4(_))));
                eth.iface.routes_mut().remove_default_ipv4_route();
                eth.config = None;
            }
            None => {}
        }
    }
    socket::reap(net);
    let lo = &mut net.lo;
    // Loopback: a packet sent is received on the next pass.
    for _ in 0..4 {
        if lo.iface.poll(t, &mut lo.dev, &mut lo.sockets) == smoltcp::iface::PollResult::None {
            break;
        }
        changed = true;
    }
    drop(guard);
    if changed {
        crate::process::wake(WAIT_KEY);
    }
}

/// The IPv6 link-local address for a MAC address (fe80:: + EUI-64: the
/// MAC with ff:fe in the middle and the universal/local bit flipped).
fn link_local(mac: [u8; 6]) -> Ipv6Address {
    let w = |a: u8, b: u8| u16::from_be_bytes([a, b]);
    Ipv6Address::new(0xfe80, 0, 0, 0, w(mac[0] ^ 2, mac[1]), w(mac[2], 0xff), w(0xfe, mac[3]), w(mac[4], mac[5]))
}

/// The IPv6 addresses of the interfaces: (interface index: 1 lo, 2 eth0,
/// address, prefix length), for /proc/net/if_inet6, netlink and choosing
/// a source address.
pub fn ipv6_addresses() -> alloc::vec::Vec<(u32, Ipv6Address, u8)> {
    let mut out = alloc::vec::Vec::new();
    let guard = NET.lock();
    let Some(net) = guard.as_ref() else { return out };
    for c in net.lo.iface.ip_addrs() {
        if let IpCidr::Ipv6(c) = c {
            out.push((1, c.address(), c.prefix_len()));
        }
    }
    if let Some(eth) = net.eth.as_ref() {
        for c in eth.iface.ip_addrs() {
            if let IpCidr::Ipv6(c) = c {
                out.push((2, c.address(), c.prefix_len()));
            }
        }
    }
    out
}

/// eth0's IPv6 routes: (destination, prefix length, gateway): each of its
/// networks on the link (no gateway), and those through a router (from
/// its advertisements: the default route), for netlink (ip -6 route).
pub fn ipv6_routes() -> alloc::vec::Vec<([u8; 16], u8, Option<[u8; 16]>)> {
    let mut out = alloc::vec::Vec::new();
    let mut guard = NET.lock();
    let Some(eth) = guard.as_mut().and_then(|n| n.eth.as_mut()) else { return out };
    for c in eth.iface.ip_addrs() {
        if let IpCidr::Ipv6(c) = c {
            // The network: the address with only its prefix's bits.
            let mut net = c.address().octets();
            for (i, b) in net.iter_mut().enumerate() {
                let keep = (c.prefix_len() as usize).saturating_sub(i * 8).min(8);
                *b &= !(0xffu16 >> keep) as u8;
            }
            if !out.iter().any(|(d, p, _)| *d == net && *p == c.prefix_len()) {
                out.push((net, c.prefix_len(), None));
            }
        }
    }
    eth.iface.routes_mut().update(|routes| {
        for r in routes.iter() {
            if let (IpCidr::Ipv6(c), IpAddress::Ipv6(via)) = (r.cidr, r.via_router) {
                out.push((c.address().octets(), c.prefix_len(), Some(via.octets())));
            }
        }
    });
    out
}

fn apply(eth: &mut Eth, cfg: IpConfig) {
    eth.iface.update_ip_addrs(|addrs| {
        // (Only the IPv4 address: IPv6's come from elsewhere.)
        addrs.retain(|c| !matches!(c, IpCidr::Ipv4(_)));
        let _ = addrs.push(IpCidr::Ipv4(cfg.address));
    });
    if let Some(gw) = cfg.gateway {
        let _ = eth.iface.routes_mut().add_default_ipv4_route(gw);
    }
    eth.config = Some(cfg);
}

/// Without a DHCP answer: QEMU's user network's usual addresses.
pub fn use_static_config() {
    let mut guard = NET.lock();
    if let Some(eth) = guard.as_mut().and_then(|n| n.eth.as_mut()) {
        if eth.config.is_none() {
            let cfg = IpConfig {
                address: Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24),
                gateway: Some(Ipv4Address::new(10, 0, 2, 2)),
                dns: Some(Ipv4Address::new(10, 0, 2, 3)),
            };
            apply(eth, cfg);
        }
    }
}

/// /etc/resolv.conf, for libc's resolver: the DNS server DHCP gave us.
/// (Written at boot; poll() never touches files, it runs in odd places.)
pub fn write_resolv_conf() {
    let dns = config().and_then(|c| c.dns);
    if let Some(dns) = dns {
        // (On a fresh disk, /etc doesn't exist yet this early in the boot.)
        let _ = crate::filesystem::make_dir("/etc");
        let text = alloc::format!("# Written by PalladiumOS from DHCP\nnameserver {}\n", dns);
        let _ = crate::filesystem::write_file("/etc/resolv.conf", &text);
    }
}

/// eth0's configuration, if it has one.
pub fn config() -> Option<IpConfig> {
    NET.lock().as_ref().and_then(|n| n.eth.as_ref()).and_then(|e| e.config)
}

/// Is there a network card?
pub fn has_card() -> bool {
    NET.lock().as_ref().is_some_and(|n| n.eth.is_some())
}

/// Called often (timer ticks in user mode, idle loops): poll if the card
/// interrupted or a tick has passed (TCP timers, DHCP).
pub fn poll_if_due() {
    let t = crate::process::ticks();
    if IRQ.swap(false, Ordering::SeqCst) || LAST_POLL.swap(t, Ordering::SeqCst) != t {
        poll();
    }
}

/// Wait at boot (up to `ms` milliseconds) for DHCP to configure eth0.
pub fn wait_for_dhcp(ms: u64) -> Option<IpConfig> {
    let until = crate::process::ticks() + (ms * 1_000_000).div_ceil(crate::process::TICK_NS);
    while crate::process::ticks() < until {
        poll();
        if let Some(c) = config() {
            return Some(c);
        }
        crate::smp::halt();
    }
    None
}
