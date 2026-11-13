// Copyright (c) 2026 tre4surehunter9
//! eth0's network card, whichever it is (virtio-net, or else an RTL8139),
//! as one smoltcp Device: each driver receives and sends whole frames.

use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use core::sync::atomic::Ordering;

use super::Stats;

use super::rtl8139::Rtl8139;
use super::virtio_net::VirtioNet;

/// The largest Ethernet frame (without its CRC).
pub const MTU: usize = 1514;

pub enum Card {
    Virtio(VirtioNet),
    Rtl(Rtl8139),
}

impl Card {
    /// The card there is: virtio-net first (it's faster), else an RTL8139.
    pub fn find() -> Option<Card> {
        if let Some(v) = VirtioNet::find() {
            return Some(Card::Virtio(v));
        }
        let pci = super::pci::scan().into_iter().find(|d| d.vendor == Rtl8139::VENDOR && d.device == Rtl8139::DEVICE)?;
        Rtl8139::new(&pci).map(Card::Rtl)
    }

    pub fn mac(&self) -> [u8; 6] {
        match self {
            Card::Virtio(v) => v.mac,
            Card::Rtl(r) => r.mac,
        }
    }

    pub fn irq(&self) -> u8 {
        match self {
            Card::Virtio(v) => v.irq,
            Card::Rtl(r) => r.irq,
        }
    }

    /// Its name, for the boot log and /proc.
    pub fn name(&self) -> &'static str {
        match self {
            Card::Virtio(_) => "virtio-net",
            Card::Rtl(_) => "RTL8139",
        }
    }

    fn receive_frame(&mut self, out: &mut [u8; MTU + 4]) -> Option<usize> {
        if !super::ETH_UP.load(Ordering::Relaxed) {
            // Down: frames that arrive are thrown away.
            let mut scratch = [0u8; MTU + 4];
            while self.receive_any(&mut scratch).is_some() {}
            return None;
        }
        let n = self.receive_any(out)?;
        super::STATS[1].received(n);
        Some(n)
    }

    fn receive_any(&mut self, out: &mut [u8; MTU + 4]) -> Option<usize> {
        match self {
            Card::Virtio(v) => v.receive_frame(out),
            Card::Rtl(r) => r.receive_frame(out),
        }
    }

    fn send_frame(&mut self, frame: &[u8]) {
        if !super::ETH_UP.load(Ordering::Relaxed) {
            super::STATS[1].tx_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        super::STATS[1].sent(frame.len());
        match self {
            Card::Virtio(v) => v.send_frame(frame),
            Card::Rtl(r) => r.send_frame(frame),
        }
    }
}

pub struct Rx {
    frame: [u8; MTU + 4],
    len: usize,
}

pub struct Tx<'a>(&'a mut Card);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.frame[..self.len])
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = [0u8; MTU];
        let r = f(&mut buf[..len]);
        self.0.send_frame(&buf[..len]);
        r
    }
}

impl Device for Card {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _t: Instant) -> Option<(Rx, Tx<'_>)> {
        let mut frame = [0u8; MTU + 4];
        let len = self.receive_frame(&mut frame)?;
        Some((Rx { frame, len }, Tx(self)))
    }

    fn transmit(&mut self, _t: Instant) -> Option<Tx<'_>> {
        Some(Tx(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps
    }
}

/// A device whose traffic is counted (the loopback interface's: eth0's
/// card counts its own).
pub struct Counted<D>(pub D, pub &'static Stats);

pub struct CountedRx<T>(T, &'static Stats);
pub struct CountedTx<T>(T, &'static Stats);

impl<T: RxToken> RxToken for CountedRx<T> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        let stats = self.1;
        self.0.consume(|b| {
            stats.received(b.len());
            f(b)
        })
    }
}

impl<T: TxToken> TxToken for CountedTx<T> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        self.1.sent(len);
        self.0.consume(len, f)
    }
}

impl<D: Device> Device for Counted<D> {
    type RxToken<'a> = CountedRx<D::RxToken<'a>> where D: 'a;
    type TxToken<'a> = CountedTx<D::TxToken<'a>> where D: 'a;

    fn receive(&mut self, t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let stats = self.1;
        self.0.receive(t).map(|(r, x)| (CountedRx(r, stats), CountedTx(x, stats)))
    }

    fn transmit(&mut self, t: Instant) -> Option<Self::TxToken<'_>> {
        let stats = self.1;
        self.0.transmit(t).map(|x| CountedTx(x, stats))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.0.capabilities()
    }
}
