//! The host copies of placed textures let go when the runtime unmaps or
//! maps their title memory (`crate::resource::evict_placed_in`): kept a few
//! frames before they are dropped (`VICTORY_EVICT_HOLD_FRAMES`), and each
//! one said in the log (`VICTORY_EVICT_LOG`). Both off by default: without
//! them a copy let go is retired at once, as before.
//!
//! Why: `crate::resource::host_resource` hands out the host copy's
//! resource as a plain borrow, not a reference. Its safety rests on the
//! copy staying while the texture is in use: `let_go_idle` takes only a
//! copy nothing asked for in seconds and frames, and the asking is noted
//! under the lock that taking needs. `evict_placed_in` takes one as soon
//! as only the texture holds it (no list, no slot), however recently it
//! was asked for. So a thread that has just borrowed the resource to make
//! a view of it (`descriptor::host`, the views of a slot: the borrow, then
//! `CreateShaderResourceView`, then the reference the slot keeps) or to
//! record a command with it can find it dropped under it, when no
//! submission runs and `crate::host::retire` drops at once. The card's
//! driver then reads a resource that is no more. Two runs on the PC ended
//! so in the driver (an access violation reading 0x3c, in builds with the
//! index of placed textures, which lets go of about twice as many copies
//! as the walk did); that this is the path is not shown yet.
//!
//! - `VICTORY_EVICT_HOLD_FRAMES=N` (1 to 600; unset, `0` or not a number
//!   off): the copy goes out of the texture at once, as before (the next
//!   use makes it anew from title memory), but it is dropped only once N
//!   frames have begun since, and then retired as usual (after the
//!   submissions running then). A borrow made before the letting go is
//!   then over, or has taken its own reference. Each letting go decides on
//!   its own, and what is held is dropped under either value, so it may
//!   alternate inside a run (`SPARK_AB`, `crate::switches::EVICT_HOLD`). A
//!   title that begins no frames would hold them until it does.
//! - `VICTORY_EVICT_LOG=1`: one line for each of the first [`LOGGED`]
//!   copies let go (the title texture, the host resource, its bytes, the
//!   strong count and the references when it went, and the frames since
//!   the texture was last asked for), one for each held copy dropped with
//!   a reference besides ours (someone took one after the letting go: the
//!   race above, caught), and once a second, if anything changed, the
//!   counts. Read once.

use std::collections::VecDeque;

/// Copies let go that are logged one by one; the rest are counted.
#[cfg(windows)]
const LOGGED: u64 = 256;

/// The longest hold, in frames.
pub(crate) const MOST_FRAMES: u64 = 600;

/// `VICTORY_EVICT_HOLD_FRAMES`: unset, `0` or not a number off (0); else
/// the frames, kept in 1 to [`MOST_FRAMES`].
pub(crate) fn hold_frames(value: Option<&str>) -> u64 {
    match value.and_then(|v| v.trim().parse::<u64>().ok()) {
        None | Some(0) => 0,
        Some(n) => n.min(MOST_FRAMES),
    }
}

/// Things held until a frame: in the order they came, each with the frame
/// from which it may go. A hold of 0 frames lets a thing go at the next
/// [`Held::due`].
pub(crate) struct Held<T> {
    queue: VecDeque<(u64, T)>,
}

impl<T> Held<T> {
    pub(crate) const fn new() -> Self {
        Held {
            queue: VecDeque::new(),
        }
    }

    /// Holds `thing`, which came at frame `now`, for `frames` frames.
    pub(crate) fn push(&mut self, now: u64, frames: u64, thing: T) {
        let due = now.saturating_add(frames);
        // Frames only go forward, and the hold may change between calls
        // (`SPARK_AB`): kept in order of the frame each may go, so that
        // `due` looks at the front only.
        let at = self.queue.partition_point(|(d, _)| *d <= due);
        self.queue.insert(at, (due, thing));
    }

    /// Takes the things that may go at frame `now`.
    pub(crate) fn due(&mut self, now: u64) -> Vec<T> {
        let n = self.queue.partition_point(|(d, _)| *d <= now);
        self.queue.drain(..n).map(|(_, t)| t).collect()
    }

    /// Things held.
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(windows)]
mod host {
    use super::{Held, LOGGED};
    use crate::resource::HostTexture;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::{Arc, Mutex, OnceLock};

    /// The copies held ([`super::Held`]).
    static HELD: Mutex<Held<(usize, Arc<HostTexture>)>> = Mutex::new(Held::new());

    /// Since the start: copies let go; of them, asked for in the frame
    /// they went or the one before; held; held copies dropped; of them,
    /// with a reference besides ours.
    static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

    /// The counts at the last summary line.
    static SAID: Mutex<[u64; 5]> = Mutex::new([0; 5]);

    /// `VICTORY_EVICT_LOG=1`, read once.
    fn logging() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var("VICTORY_EVICT_LOG").is_ok_and(|v| v.trim() == "1"))
    }

    /// The host copy `host` of title texture `texture` went out of it in
    /// `crate::resource::evict_placed_in` (only the texture held it), the
    /// texture last asked for at `used` (`crate::host::moment`), the copy
    /// of `bytes`: retired at once, as before, or held ([`super`]).
    pub(crate) fn let_go(texture: usize, host: Arc<HostTexture>, bytes: u64, used: u64) {
        let frames = crate::switches::EVICT_HOLD.get();
        let log = logging();
        if frames == 0 && !log {
            crate::host::retire(host);
            return;
        }
        let now = crate::frame::current();
        if log {
            let since = now.saturating_sub(used >> 32);
            let n = COUNTS[0].fetch_add(1, Relaxed);
            if since <= 1 {
                COUNTS[1].fetch_add(1, Relaxed);
            }
            if n < LOGGED {
                use windows::core::Interface;
                tracing::info!(
                    texture = format_args!("{texture:#x}"),
                    host = ?host.texture.as_raw(),
                    bytes,
                    strong = Arc::strong_count(&host),
                    references = crate::host::references(&host.texture),
                    frames_since_asked = since,
                    held_frames = frames,
                    "evict: host copy of a placed texture let go at an unmapping (VICTORY_EVICT_LOG)"
                );
            }
        }
        if frames == 0 {
            crate::host::retire(host);
            return;
        }
        if log {
            COUNTS[2].fetch_add(1, Relaxed);
        }
        let due = {
            let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
            held.push(now, frames, (texture, host));
            held.due(now)
        };
        drop_held(due, log);
    }

    /// Drops the held copies whose frames are over (and, with the log,
    /// says the counts if they changed): from the sweep, once a second,
    /// and at each letting go. Nothing to do with both switches off and
    /// nothing held.
    pub(crate) fn drain() {
        let log = logging();
        let due = {
            let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
            if held.len() == 0 && !log {
                return;
            }
            held.due(crate::frame::current())
        };
        drop_held(due, log);
        if log {
            let now = COUNTS.each_ref().map(|c| c.load(Relaxed));
            let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
            if *said != now {
                *said = now;
                let held = HELD.lock().unwrap_or_else(|e| e.into_inner()).len();
                tracing::info!(
                    "evict (VICTORY_EVICT_LOG): since the start {} host copies of placed textures let go at unmappings, {} of them asked for in the frame they went or the one before; {} held (VICTORY_EVICT_HOLD_FRAMES), {} dropped, {} of them with a reference besides ours (taken after the letting go); {held} held now",
                    now[0],
                    now[1],
                    now[2],
                    now[3],
                    now[4],
                );
            }
        }
    }

    /// Retires held copies whose frames are over, outside the lock; with
    /// the log, counts (and says, while few) those someone else took a
    /// reference to after they were let go.
    fn drop_held(due: Vec<(usize, Arc<HostTexture>)>, log: bool) {
        for (texture, host) in due {
            if log {
                COUNTS[3].fetch_add(1, Relaxed);
                let references = crate::host::references(&host.texture);
                let strong = Arc::strong_count(&host);
                if references > 1 || strong > 1 {
                    let n = COUNTS[4].fetch_add(1, Relaxed);
                    if n < LOGGED {
                        use windows::core::Interface;
                        tracing::warn!(
                            texture = format_args!("{texture:#x}"),
                            host = ?host.texture.as_raw(),
                            strong,
                            references,
                            "evict: a held host copy is dropped with a reference besides ours: something took it after it was let go (VICTORY_EVICT_HOLD_FRAMES kept it alive)"
                        );
                    }
                }
            }
            crate::host::retire(host);
        }
    }
}

#[cfg(windows)]
pub(crate) use host::{drain, let_go};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hold_reads_as_documented() {
        assert_eq!(hold_frames(None), 0);
        assert_eq!(hold_frames(Some("0")), 0);
        assert_eq!(hold_frames(Some("x")), 0);
        assert_eq!(hold_frames(Some(" 3 ")), 3);
        assert_eq!(hold_frames(Some("100000")), MOST_FRAMES);
    }

    /// A thing held for N frames goes at the frame N after it came, not
    /// before; things go in the order of their frames even when the hold
    /// changed between them.
    #[test]
    fn things_go_once_their_frames_are_over() {
        let mut held = Held::new();
        held.push(10, 3, 'a');
        assert!(held.due(10).is_empty());
        assert!(held.due(12).is_empty());
        held.push(12, 1, 'b');
        held.push(12, 5, 'c');
        assert_eq!(held.len(), 3);
        assert_eq!(held.due(13), ['a', 'b']);
        assert!(held.due(16).is_empty());
        assert_eq!(held.due(17), ['c']);
        assert_eq!(held.len(), 0);
    }

    /// Held for 0 frames: goes at the next look, in the same frame.
    #[test]
    fn a_hold_of_no_frames_goes_at_once() {
        let mut held = Held::new();
        held.push(7, 0, 1u8);
        assert_eq!(held.due(7), [1]);
    }
}
