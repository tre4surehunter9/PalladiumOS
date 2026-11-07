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

pub mod pci;
pub mod rtl8139;
pub mod socket;

use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Loopback, Medium};
use smoltcp::socket::dhcpv4;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use spin::Mutex;

use rtl8139::Rtl8139;

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
    pub dev: Rtl8139,
    pub sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    pub config: Option<IpConfig>,
}

pub struct Lo {
    pub iface: Interface,
    pub dev: Loopback,
    pub sockets: SocketSet<'static>,
}

pub struct Net {
    pub eth: Option<Eth>,
    pub lo: Lo,
    /// Closed TCP sockets still saying goodbye (removed once done).
    pub closing: alloc::vec::Vec<(socket::Which, SocketHandle)>,
}

pub static NET: Mutex<Option<Net>> = Mutex::new(None);
/// Set by the card's interrupt: there's something to poll for.
static IRQ: AtomicBool = AtomicBool::new(false);
/// The card's IRQ line (0 = none) and I/O ports, for the interrupt handler.
static CARD_IRQ: AtomicU64 = AtomicU64::new(0);
static CARD_IO: AtomicU64 = AtomicU64::new(0);
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
pub fn init() -> Option<([u8; 6], u8)> {
    let card = pci::scan().into_iter().find(|d| d.vendor == Rtl8139::VENDOR && d.device == Rtl8139::DEVICE);
    let eth = card.and_then(|c| Rtl8139::new(&c)).map(|mut dev| {
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(dev.mac)));
        config.random_seed = seed();
        let iface = Interface::new(config, &mut dev, now());
        let mut sockets = SocketSet::new(vec![]);
        let dhcp = sockets.add(dhcpv4::Socket::new());
        Eth { iface, dev, sockets, dhcp, config: None }
    });
    let found = eth.as_ref().map(|e| (e.dev.mac, e.dev.irq));
    if let Some(e) = eth.as_ref() {
        CARD_IO.store(e.dev.io_base() as u64, Ordering::SeqCst);
    }
    if let Some((_, irq)) = found {
        CARD_IRQ.store(irq as u64, Ordering::SeqCst);
        crate::interrupts::unmask_irq(irq);
    }

    let mut dev = Loopback::new(Medium::Ip);
    let mut config = Config::new(HardwareAddress::Ip);
    config.random_seed = seed();
    let mut iface = Interface::new(config, &mut dev, now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8));
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
                eth.iface.update_ip_addrs(|a| a.clear());
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

fn apply(eth: &mut Eth, cfg: IpConfig) {
    eth.iface.update_ip_addrs(|addrs| {
        addrs.clear();
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

/// Wait at boot (up to `ticks`) for DHCP to configure eth0.
pub fn wait_for_dhcp(ticks: u64) -> Option<IpConfig> {
    let until = crate::process::ticks() + ticks;
    while crate::process::ticks() < until {
        poll();
        if let Some(c) = config() {
            return Some(c);
        }
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();
    }
    None
}
