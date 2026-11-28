//! Giving back PC memory the host keeps for recording commands, for PCs
//! whose physical memory runs out before the card's does. Four parts, all
//! off by default:
//!
//! - **command allocators** (`VICTORY_ALLOCATOR_TRIM=1`,
//!   [`allocator_trim_ms`]): a D3D12 command allocator never gives back
//!   the memory of the most its list ever recorded into it, so a list that
//!   once took a loading frame's commands keeps that for the whole run;
//! - **idle pooled lists** (`VICTORY_LIST_POOL_IDLE_S`, [`idle_list_ms`]):
//!   the pool of host lists that have run (`crate::list`) never shrinks;
//! - **spare scratch chunks** (with the allocator trim): chunks of upload
//!   memory kept for the next list, up to 128 MiB, stay kept when no list
//!   asks for them any more;
//! - **indirect-work budgets** (`VICTORY_BUDGET_PACK=1`, [`budget_pack`]):
//!   each host list took a committed buffer of 64 KiB of PC memory for
//!   256 bytes.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, ID3D12CommandAllocator, ID3D12Device, ID3D12Resource,
};

/// The age, in milliseconds, past which a host list's command allocator
/// is taken anew at the list's next reset (and a prelude's before it
/// records again: the only moments an allocator holds nothing that still
/// runs); `None`, the default, keeps every allocator for the list's life.
/// `VICTORY_ALLOCATOR_TRIM=1` turns it on, `VICTORY_ALLOCATOR_TRIM_S` sets
/// the age in seconds (30; 5 to 3,600). Read once. The old allocator goes
/// behind the submissions still running (`crate::host::retire`). A title
/// list executed again without a reset keeps its allocator. Also turns on
/// the trim of idle lists ([`idle_list_ms`]) and of spare scratch chunks.
pub(crate) fn allocator_trim_ms() -> Option<u64> {
    static MS: OnceLock<Option<u64>> = OnceLock::new();
    *MS.get_or_init(|| {
        trim_ms(
            std::env::var("VICTORY_ALLOCATOR_TRIM").ok().as_deref(),
            std::env::var("VICTORY_ALLOCATOR_TRIM_S").ok().as_deref(),
        )
    })
}

/// [`allocator_trim_ms`] from the two variables' values.
fn trim_ms(on: Option<&str>, seconds: Option<&str>) -> Option<u64> {
    (on.map(str::trim) == Some("1")).then(|| {
        seconds
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(5, 3600)
            * 1000
    })
}

/// How long, in milliseconds, a host list may wait in the pool of lists
/// that have run (`crate::list`) before it is destroyed with its
/// allocators and buffers; `None` keeps them. `VICTORY_LIST_POOL_IDLE_S`
/// in seconds (at most 3,600; `0` keeps them); unset, the age of
/// [`allocator_trim_ms`]. Read once.
pub(crate) fn idle_list_ms() -> Option<u64> {
    static MS: OnceLock<Option<u64>> = OnceLock::new();
    *MS.get_or_init(|| {
        idle_ms(
            std::env::var("VICTORY_LIST_POOL_IDLE_S").ok().as_deref(),
            allocator_trim_ms(),
        )
    })
}

/// [`idle_list_ms`] from the variable's value and the allocator trim's age.
fn idle_ms(seconds: Option<&str>, trim: Option<u64>) -> Option<u64> {
    match seconds.and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(0) => None,
        Some(s) => Some(s.min(3600) * 1000),
        None => trim,
    }
}

/// Whether something made (or last looked at) at `made_ms` is at least
/// `limit_ms` old at `now_ms`.
pub(crate) fn aged(made_ms: u64, now_ms: u64, limit_ms: u64) -> bool {
    now_ms.saturating_sub(made_ms) >= limit_ms
}

/// Since the start: host list allocators taken anew, prelude allocators
/// taken anew, idle lists destroyed, bytes of spare scratch chunks freed,
/// and indirect-work budgets handed out as slots of a shared buffer.
pub(crate) static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Which allocator [`Allocator::ready`] counts in [`COUNTS`].
#[derive(Clone, Copy)]
pub(crate) enum Owner {
    /// A host list's own.
    List = 0,
    /// Its prelude's.
    Prelude = 1,
}

/// A command allocator and when it was made ([`crate::host::now_ms`]).
pub(crate) struct Allocator {
    allocator: ID3D12CommandAllocator,
    made_ms: u64,
}

impl Allocator {
    /// One made now.
    pub(crate) fn new(allocator: ID3D12CommandAllocator) -> Allocator {
        Allocator {
            allocator,
            made_ms: crate::host::now_ms(),
        }
    }

    /// The allocator to record with.
    pub(crate) fn get(&self) -> &ID3D12CommandAllocator {
        &self.allocator
    }

    /// Makes the allocator ready for its list's reset: a new one when it is
    /// old enough ([`allocator_trim_ms`]; a new one needs no reset), else
    /// the same one reset. Call only once every submission of commands
    /// recorded from it has finished. The error is the reset's.
    pub(crate) fn ready(
        &mut self,
        device: &ID3D12Device,
        owner: Owner,
    ) -> windows::core::Result<()> {
        let now = crate::host::now_ms();
        if let Some(limit) = allocator_trim_ms()
            && aged(self.made_ms, now, limit)
        {
            // Asked again only after another `limit`, made or not.
            self.made_ms = now;
            // SAFETY: plain object creation.
            let made = unsafe {
                device.CreateCommandAllocator::<ID3D12CommandAllocator>(
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                )
            };
            if let Ok(fresh) = made {
                crate::host::retire(std::mem::replace(&mut self.allocator, fresh));
                COUNTS[owner as usize].fetch_add(1, Relaxed);
                return Ok(());
            }
        }
        // SAFETY: the caller's promise: nothing recorded from it still runs.
        unsafe { self.allocator.Reset() }
    }
}

/// Whether host lists' indirect-work budgets are 256-byte slots of shared
/// 64 KiB buffers (`VICTORY_BUDGET_PACK=1`; off by default; read once).
/// Only a budget whose words shaders charge in the card's own memory
/// (`card_budget` in `crate::host`) takes a slot: its mapped words are then
/// only a copy's destination, so every list of a submission leaves a shared
/// buffer in the same state. A list whose shaders charge the mapped words
/// themselves keeps a buffer of its own. A slot goes back behind the
/// submissions running when its list goes.
pub(crate) fn budget_pack() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BUDGET_PACK").is_some_and(|v| v == "1"))
}

/// The bytes of one budget slot (a host list's budget words).
pub(crate) const BUDGET_SLOT_BYTES: u64 = 256;
/// The bytes of a buffer the slots are cut from: what a committed buffer
/// takes at least.
const BUDGET_SLAB_BYTES: u64 = 64 << 10;

/// Slots of `slot` bytes cut from slabs of `slab` bytes, handed out and
/// given back by (slab, offset).
pub(crate) struct Slots<T> {
    slabs: Vec<T>,
    free: Vec<(usize, u64)>,
    slot: u64,
    slab: u64,
}

impl<T> Slots<T> {
    const fn new(slot: u64, slab: u64) -> Self {
        Slots {
            slabs: Vec::new(),
            free: Vec::new(),
            slot,
            slab,
        }
    }

    /// A free slot as (slab, offset in it), the lowest of the newest slab
    /// first; with none free, `make` gives a new slab (`None`: no slot).
    fn take(&mut self, make: impl FnOnce() -> Option<T>) -> Option<(usize, u64)> {
        if self.free.is_empty() {
            let slab = self.slabs.len();
            self.slabs.push(make()?);
            self.free.extend(
                (0..self.slab / self.slot)
                    .rev()
                    .map(|i| (slab, i * self.slot)),
            );
        }
        self.free.pop()
    }

    /// The slot at `offset` of `slab` is free again.
    fn give(&mut self, slab: usize, offset: u64) {
        self.free.push((slab, offset));
    }

    /// Slab `i`.
    fn slab(&self, i: usize) -> Option<&T> {
        self.slabs.get(i)
    }
}

/// A slab of budget slots: the buffer and where it is mapped.
struct Slab(ID3D12Resource, usize);

// SAFETY: a D3D12 resource (free-threaded) and its persistent mapping;
// each slot of it is written only by the list that holds the slot.
unsafe impl Send for Slab {}

/// The slabs of budget slots ([`budget_pack`]); never freed (as many as
/// the most lists alive at once need, 256 slots each).
static BUDGET_SLOTS: Mutex<Slots<Slab>> =
    Mutex::new(Slots::new(BUDGET_SLOT_BYTES, BUDGET_SLAB_BYTES));

/// A host list's indirect-work budget words in memory the CPU maps: a
/// buffer of its own, or a slot of a shared one ([`budget_pack`]).
pub(crate) struct Budget {
    /// The buffer the words are in.
    pub(crate) buffer: ID3D12Resource,
    /// Where the words are mapped (a host address).
    pub(crate) data: usize,
    /// The words' offset in `buffer`, in bytes.
    pub(crate) offset: u64,
    /// The slot, given back when the budget goes.
    slot: Option<(usize, u64)>,
}

/// A slot given back when dropped (behind the submissions that may still
/// copy into it, `crate::host::retire`).
struct FreeSlot(usize, u64);

impl Drop for FreeSlot {
    fn drop(&mut self) {
        BUDGET_SLOTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .give(self.0, self.1);
    }
}

impl Drop for Budget {
    fn drop(&mut self) {
        if let Some((slab, offset)) = self.slot {
            crate::host::retire(FreeSlot(slab, offset));
        }
    }
}

impl Budget {
    /// Words in a buffer of their own (`buffer`, mapped at `data`).
    pub(crate) fn own(buffer: ID3D12Resource, data: *mut u8) -> Budget {
        Budget {
            buffer,
            data: data as usize,
            offset: 0,
            slot: None,
        }
    }

    /// Zeroed words in a slot of a shared buffer, a new buffer made by
    /// `make` (mapped) when no slot is free; `None` if none could be made.
    pub(crate) fn packed(
        make: impl FnOnce() -> Option<(ID3D12Resource, *mut u8)>,
    ) -> Option<Budget> {
        let mut slots = BUDGET_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
        let (slab, offset) =
            slots.take(|| make().map(|(buffer, data)| Slab(buffer, data as usize)))?;
        let Slab(buffer, base) = slots.slab(slab)?;
        let data = base + offset as usize;
        // SAFETY: a slot of a live mapped buffer, handed to this budget alone;
        // a slot given back has gone behind every submission that used it.
        unsafe { std::ptr::write_bytes(data as *mut u8, 0, BUDGET_SLOT_BYTES as usize) };
        COUNTS[4].fetch_add(1, Relaxed);
        Some(Budget {
            buffer: buffer.clone(),
            data,
            offset,
            slot: Some((slab, offset)),
        })
    }
}

/// For the memory line: [`COUNTS`] and the slabs of budget slots made.
#[cfg(feature = "diagnostics")]
pub(crate) fn counts() -> [u64; 6] {
    let mut out = [0; 6];
    for (o, c) in out.iter_mut().zip(&COUNTS) {
        *o = c.load(Relaxed);
    }
    out[5] = BUDGET_SLOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .slabs
        .len() as u64;
    out
}

/// Whether any part of this module is on (the memory line says it then).
#[cfg(feature = "diagnostics")]
pub(crate) fn any() -> bool {
    allocator_trim_ms().is_some() || idle_list_ms().is_some() || budget_pack()
}

/// From the host's once-a-second sweep: idle pooled lists past their age
/// go, and every ten seconds the spare scratch chunks no list took since
/// the last time.
pub(crate) fn sweep(now_ms: u64) {
    if let Some(limit) = idle_list_ms() {
        COUNTS[2].fetch_add(crate::list::trim_idle(now_ms, limit) as u64, Relaxed);
    }
    if allocator_trim_ms().is_some() {
        static LAST: AtomicU64 = AtomicU64::new(0);
        let last = LAST.load(Relaxed);
        if aged(last, now_ms, 10_000)
            && LAST
                .compare_exchange(last, now_ms, Relaxed, Relaxed)
                .is_ok()
        {
            COUNTS[3].fetch_add(crate::host::trim_spare_scratch(), Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Slots, aged, idle_ms, trim_ms};

    /// The allocator trim's age: off unless `1`, 30 s by default, held
    /// between 5 s and an hour, garbage taken as the default.
    #[test]
    fn the_allocator_age_is_off_unless_asked_and_clamped() {
        assert_eq!(trim_ms(None, Some("10")), None);
        assert_eq!(trim_ms(Some("0"), None), None);
        assert_eq!(trim_ms(Some("1"), None), Some(30_000));
        assert_eq!(trim_ms(Some(" 1 "), Some(" 12 ")), Some(12_000));
        assert_eq!(trim_ms(Some("1"), Some("1")), Some(5_000));
        assert_eq!(trim_ms(Some("1"), Some("99999")), Some(3_600_000));
        assert_eq!(trim_ms(Some("1"), Some("soon")), Some(30_000));
    }

    /// Idle lists: their own age when set (0 keeps them, an hour at most),
    /// else the allocator trim's.
    #[test]
    fn idle_lists_take_their_own_age_or_the_allocators() {
        assert_eq!(idle_ms(None, None), None);
        assert_eq!(idle_ms(None, Some(30_000)), Some(30_000));
        assert_eq!(idle_ms(Some("0"), Some(30_000)), None);
        assert_eq!(idle_ms(Some("7"), None), Some(7_000));
        assert_eq!(idle_ms(Some("7200"), None), Some(3_600_000));
        assert_eq!(idle_ms(Some("x"), Some(30_000)), Some(30_000));
    }

    #[test]
    fn age_counts_from_the_making_and_never_runs_backwards() {
        assert!(!aged(1_000, 30_999, 30_000));
        assert!(aged(1_000, 31_000, 30_000));
        assert!(!aged(5_000, 1_000, 30_000), "a clock read late is not old");
    }

    /// Budget slots: 256 to a slab, no slot handed out twice, slots given
    /// back are reused before a slab is added, and no slab when it cannot
    /// be made.
    #[test]
    fn budget_slots_are_unique_reused_and_grow_by_slabs() {
        let mut slots: Slots<u32> = Slots::new(256, 64 << 10);
        let mut made = 0;
        let mut make = || {
            made += 1;
            Some(made)
        };
        let taken: Vec<(usize, u64)> = (0..300).map(|_| slots.take(&mut make).unwrap()).collect();
        assert_eq!(slots.slabs.len(), 2);
        let mut unique = taken.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 300, "every slot once");
        assert_eq!(taken[0], (0, 0), "the lowest first");
        assert_eq!(taken[255], (0, 255 * 256));
        assert_eq!(taken[256], (1, 0));
        assert!(
            taken
                .iter()
                .all(|&(_, offset)| offset % 256 == 0 && offset < 64 << 10)
        );
        slots.give(0, 512);
        assert_eq!(
            slots.take(|| panic!("no new slab while one is free")),
            Some((0, 512))
        );
        assert_eq!(slots.slab(1), Some(&2));
        // All free taken: a slab that cannot be made gives no slot.
        while slots.free.pop().is_some() {}
        assert_eq!(slots.take(|| None), None);
        assert_eq!(slots.slabs.len(), 2);
    }
}
