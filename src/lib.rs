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

/// A type that can be used with BorrowChannel.
///
/// # Safety
/// `Borrowed` must behave like a reference with regard to reborrowing:
/// - Creating a `Borrowed<'short>` from a `Borrowed<'long>` by transmuting/copying its bytes must be safe if `'long` outlives `'short`.
/// - Multiple such reborrowed copies may be made.
/// - If `Shared` is `false`, users of the trait must not create copies with overlapping lifetimes.
///
/// Moreover, `Borrowed` must have no interior mutability (the unstable [Freeze](core::marker::Freeze) trait).
/// This requirement is to enable implementing Deref.
/// It may be relaxed once Freeze is stabilized.
pub unsafe trait Reborrowable {
    const IS_SHARED: bool;
    type Borrowed<'a>;
}

/// Used in a [BorrowChannel] that does not support access from multiple threads.
pub struct UnsyncLock<T: Nuclear>(Cell<T>);

/// Used in a [BorrowChannel] that supports access from multiple threads.
pub struct SyncLock<T: Nuclear>(Isotope<T>);

macro_rules! impl_channel_lock {
    ($T:ty) => {
        impl CounterInnerPriv for $T {}
        impl Lock for UnsyncLock<$T> {}
        impl ChannelLockPriv for UnsyncLock<$T> {
            type CounterInner = $T;
            type Counter = Cell<$T>;
            fn counter(&self) -> &Self::Counter {
                &self.0
            }
            fn new() -> Self {
                Self(Radium::new(0))
            }
        }
        impl Lock for SyncLock<$T> {}
        impl ChannelLockPriv for SyncLock<$T> {
            type CounterInner = $T;
            type Counter = Isotope<$T>;
            fn counter(&self) -> &Self::Counter {
                &self.0
            }
            fn new() -> Self {
                Self(Radium::new(0))
            }
        }
    };
}

impl_channel_lock!(u64);
impl_channel_lock!(u32);
impl_channel_lock!(u16);
impl_channel_lock!(u8);

/// A type parameter on [BorrowChannel] that describes the synchronization strategy.
#[allow(private_bounds)]
pub trait Lock: ChannelLockPriv {}

trait ChannelLockPriv {
    type CounterInner: CounterInnerPriv;
    type Counter: Radium<Item = Self::CounterInner>;
    fn counter(&self) -> &Self::Counter;
    fn new() -> Self;
}

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

/// A channel for sending borrows.
pub struct BorrowChannel<T: Reborrowable, L: Lock> {
    data: UnsafeCell<MaybeUninit<T::Borrowed<'static>>>,
    count: L,
    _p: PhantomData<T>,
}

/// Returned from [borrow](BorrowChannel::borrow).
pub struct BorrowChannelGuard<'a, T: Reborrowable, L: Lock> {
    channel: &'a BorrowChannel<T, L>,
}

impl<T: Reborrowable, L: Lock> Drop for BorrowChannelGuard<'_, T, L> {
    fn drop(&mut self) {
        self.channel.count().fetch_sub(1u8.into(), Release);
    }
}

impl<'a, T: Reborrowable, L: Lock> Deref for BorrowChannelGuard<'a, T, L> {
    type Target = T::Borrowed<'a>;

    fn deref(&self) -> &Self::Target {
        unsafe {
            transmute::<&MaybeUninit<T::Borrowed<'static>>, &T::Borrowed<'_>>(
                &*self.channel.data.get(),
            )
        }
    }
}

impl<T: Reborrowable, L: Lock> BorrowChannelGuard<'_, T, L> {
    pub fn with<'b>(&'b mut self, f: impl FnOnce(T::Borrowed<'b>)) {
        f(unsafe {
            let ptr: *const MaybeUninit<T::Borrowed<'static>> = self.channel.data.get();
            ptr.cast::<T::Borrowed<'_>>().read()
        })
    }
}

impl<T: Reborrowable, L: Lock> BorrowChannel<T, L> {
    /// Construct a channel with an arbitrary `ChannelLock`.
    /// Callers must ensure the lock will not overflow.
    /// Consider using [new_sync](Self::new_sync) or [new_unsync](Self::new_unsync), which use a 62 bit counter, making overflow practically impossible.
    ///
    /// TODO: document when overflows might occur
    pub unsafe fn new() -> Self {
        BorrowChannel {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            count: L::new(),
            _p: PhantomData,
        }
    }

    fn count(&self) -> &L::Counter {
        self.count.counter()
    }

    /// Provide a borrow to the channel while running `f`.
    ///
    /// While `f` executes, the borrow can be retrieved using [borrow](Self::borrow).
    /// If the borrow is still in use when `f` exits (return or unwind), the program aborts.
    pub fn lend<R>(&self, borrow: T::Borrowed<'_>, f: impl FnOnce() -> R) -> R {
        self.count()
            .compare_exchange(state_empty(), state_locked(), Acquire, Relaxed)
            .expect("borrow channel is not empty");
        defer! {
            if self.count().compare_exchange(state_filled(),state_empty(),Acquire,Relaxed).is_err(){
                abort();
            }
        }
        unsafe {
            self.data.get().write(MaybeUninit::new(transmute::<
                T::Borrowed<'_>,
                T::Borrowed<'static>,
            >(borrow)));
        }
        self.count().store(state_filled(), Release);
        f()
    }

    /// Use a borrow that was provided using [borrow](Self::borrow).
    ///
    /// If the borrow is still in use when the providing call to [borrow](Self::borrow) ends, the program aborts.
    pub fn borrow(&self) -> BorrowChannelGuard<'_, T, L> {
        let old_count = self.count().fetch_add(1u8.into(), Acquire);
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

impl<T: Reborrowable> BorrowChannel<T, UnsyncLock<u64>> {
    /// Construct a channel that can not be shared across threads.
    pub fn new_unsync() -> Self {
        unsafe { Self::new() }
    }
}

impl<T: Reborrowable> BorrowChannel<T, SyncLock<u64>> {
    /// Construct a channel that can be shared across threads.
    pub fn new_sync() -> Self {
        unsafe { Self::new() }
    }
}

extern "C" fn abort() {
    // guaranteed to abort
    panic!("abort");
}

unsafe impl<T: Sized> Reborrowable for &'static T {
    const IS_SHARED: bool = true;
    type Borrowed<'a> = &'a T;
}

unsafe impl<T: Sized> Reborrowable for &'static mut T {
    const IS_SHARED: bool = false;
    type Borrowed<'a> = &'a mut T;
}

unsafe impl<T: Reborrowable, L: Lock> Sync for BorrowChannel<T, L>
where
    L: Sync,
    T: Send + Sync,
{
}
