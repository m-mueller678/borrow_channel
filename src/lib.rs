#![no_std]
//! A channel for sending borrows.
//! This is useful when you want to borrow data but cannot accept a reference to it via function arguments.
//! For example, because a closure is called by code you do not control and which is unaware of the borrowing.
//!
//! A reference can be inserted via [lend](BorrowChannel::lend).
//! While the function passed to lend executes, the reference can be reborrowed using [borrow](BorrowChannel::borrow).
//! If lend attempts to return while the data is still borrowed, the program is aborted.
//!
//! ```rust
//! # use crate::borrow_channel::BorrowChannel;
//! # use std::rc::Rc;
//! let channel = Rc::new(BorrowChannel::<&i32, _>::new_unsync());
//! let mut_channel = Rc::new(BorrowChannel::<&mut i32, _>::new_unsync());
//!
//! let reads_a = || {
//!     assert_eq!(**channel.borrow(), 42);
//! };
//! let writes_a = || mut_channel.borrow().with(|b| *b = 42);
//!
//! let mut a = 0;
//! mut_channel.lend(&mut a, writes_a);
//! channel.lend(&mut a, reads_a);
//! ```

use core::cell::{Cell, UnsafeCell};
use core::fmt::Debug;
use core::marker::PhantomData;
use core::mem::{transmute, MaybeUninit};
use core::ops::{Deref, Shl};
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use radium::marker::{Nuclear, NumericOps};
use radium::{Isotope, Radium};
use scopeguard::defer;

/// This trait can be implemented on reference-like types to make them usable with BorrowChannel.
///
/// # Safety
/// `Borrowed` must behave like a reference with regard to reborrowing:
/// - Creating a `Borrowed<'a>` from a `Borrowed<'b>` by transmuting/copying its bytes must be safe if `'a` outlives `'b`.
/// - Multiple such reborrowed copies may be made.
/// - If `Shared` is `false`, users of the trait must not create copies with overlapping lifetimes.
///
/// Moreover, `Borrowed` must have no interior mutability (the unstable [Freeze](core::marker::Freeze) trait).
/// This requirement is to enable implementing Deref.
/// It may be relaxed once Freeze is stabilized.
pub unsafe trait ChannelBorrow {
    const IS_SHARED: bool;
    type Borrowed<'a>;
}

pub struct UnsyncLock<T: CounterInner> {
    _p: PhantomData<T>,
}
pub struct SyncLock<T: CounterInner> {
    _p: PhantomData<T>,
}

macro_rules! impl_channel_lock {
    ($T:ty) => {
        impl CounterInner for $T {}
        impl CounterInnerPriv for $T {}
        impl ChannelLock for UnsyncLock<$T> {}
        impl ChannelLockPriv for UnsyncLock<$T> {
            type CounterInner = $T;
            type Counter = Cell<$T>;
        }
        impl ChannelLock for SyncLock<$T> {}
        impl ChannelLockPriv for SyncLock<$T> {
            type CounterInner = $T;
            type Counter = Isotope<$T>;
        }
    };
}

impl_channel_lock!(u64);
impl_channel_lock!(u32);
impl_channel_lock!(u16);
impl_channel_lock!(u8);

#[allow(private_bounds)]
pub trait ChannelLock: ChannelLockPriv {}

trait ChannelLockPriv {
    type CounterInner: CounterInnerPriv;
    type Counter: Radium<Item = Self::CounterInner>;
}

#[allow(private_bounds)]
pub trait CounterInner: CounterInnerPriv {}
trait CounterInnerPriv:
    From<u8> + Shl<u32, Output = Self> + NumericOps + Debug + Copy + Nuclear
{
}

fn counter_bits<C: CounterInnerPriv>() -> u32 {
    size_of::<C>() as u32 * 8
}

fn state_empty<C: CounterInnerPriv>() -> C {
    C::from(0u8)
}

fn state_locked<C: CounterInnerPriv>() -> C {
    C::from(1u8) << (counter_bits::<C>() - 2)
}

fn state_filled<C: CounterInnerPriv>() -> C {
    C::from(2u8) << (counter_bits::<C>() - 2)
}

fn state_mask<C: CounterInnerPriv>() -> C {
    C::from(3u8) << (counter_bits::<C>() - 2)
}

pub struct BorrowChannel<T: ChannelBorrow, L: ChannelLock> {
    data: UnsafeCell<MaybeUninit<T::Borrowed<'static>>>,
    count: L::Counter,
    _p: PhantomData<T>,
}

pub struct BorrowChannelGuard<'a, T: ChannelBorrow, L: ChannelLock> {
    channel: &'a BorrowChannel<T, L>,
}

impl<T: ChannelBorrow, L: ChannelLock> Drop for BorrowChannelGuard<'_, T, L> {
    fn drop(&mut self) {
        self.channel.count.fetch_sub(1u8.into(), Release);
    }
}

impl<'a, T: ChannelBorrow, L: ChannelLock> Deref for BorrowChannelGuard<'a, T, L> {
    type Target = T::Borrowed<'a>;

    fn deref(&self) -> &Self::Target {
        unsafe {
            transmute::<&MaybeUninit<T::Borrowed<'static>>, &T::Borrowed<'_>>(
                &*self.channel.data.get(),
            )
        }
    }
}

impl<T: ChannelBorrow, L: ChannelLock> BorrowChannelGuard<'_, T, L> {
    pub fn with<'b>(&'b mut self, f: impl FnOnce(T::Borrowed<'b>)) {
        f(unsafe {
            let ptr: *const MaybeUninit<T::Borrowed<'static>> = self.channel.data.get();
            ptr.cast::<T::Borrowed<'_>>().read()
        })
    }
}

impl<T: ChannelBorrow> BorrowChannel<T, UnsyncLock<u64>> {
    pub fn new_unsync() -> Self {
        unsafe { Self::new() }
    }
}

impl<T: ChannelBorrow> BorrowChannel<T, SyncLock<u64>> {
    pub fn new_sync() -> Self {
        unsafe { Self::new() }
    }
}

impl<T: ChannelBorrow, L: ChannelLock> BorrowChannel<T, L> {
    /// Create a BorrowChannel with an arbitrary `ChannelLock`.
    /// Callers must ensure the lock will not overflow.
    /// Consider using [new_sync](Self::new_sync) and [new_unsync](Self::new_unsync), which use a 62 bit counter, making overflow practically impossible.
    ///
    /// TODO: document when overflows might occur
    pub unsafe fn new() -> Self {
        BorrowChannel {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            count: L::Counter::new(state_empty()),
            _p: PhantomData,
        }
    }

    pub fn lend<R>(&self, p: T::Borrowed<'_>, f: impl FnOnce() -> R) -> R {
        self.count
            .compare_exchange(state_empty(), state_locked(), Acquire, Relaxed)
            .expect("borrow channel is not empty");
        defer! {
            if self.count.compare_exchange(state_filled(),state_empty(),Acquire,Relaxed).is_err(){
                abort();
            }
        }
        unsafe {
            self.data.get().write(MaybeUninit::new(transmute::<
                T::Borrowed<'_>,
                T::Borrowed<'static>,
            >(p)));
        }
        self.count.store(state_filled(), Release);
        f()
    }

    pub fn borrow(&self) -> BorrowChannelGuard<'_, T, L> {
        let old_count = self.count.fetch_add(1u8.into(), Acquire);
        let guard = BorrowChannelGuard { channel: self };
        assert!(
            old_count & state_mask() == state_filled(),
            "channel is empty"
        );
        if !T::IS_SHARED {
            assert!(
                old_count == state_filled(),
                "channel is already borrowed from"
            );
        }
        debug_assert!(
            (old_count + 1u8.into()) & state_mask() == state_filled(),
            "channel counter overflow"
        );
        guard
    }
}

extern "C" fn abort() {
    // guaranteed to abort
    panic!("abort");
}

unsafe impl<T: Sized> ChannelBorrow for &'static T {
    const IS_SHARED: bool = true;
    type Borrowed<'a> = &'a T;
}

unsafe impl<T: Sized> ChannelBorrow for &'static mut T {
    const IS_SHARED: bool = false;
    type Borrowed<'a> = &'a mut T;
}
