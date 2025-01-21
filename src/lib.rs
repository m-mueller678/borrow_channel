#![no_std]
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
        impl ChannelLock for UnsyncLock<$T> {
            type CounterInner = $T;
            type Counter = Cell<$T>;
        }

        impl ChannelLock for SyncLock<$T> {
            type CounterInner = $T;
            type Counter = Isotope<$T>;
        }
    };
}

impl_channel_lock!(u64);
impl_channel_lock!(u32);
impl_channel_lock!(u16);
impl_channel_lock!(u8);

trait ChannelLock {
    type CounterInner: CounterInner;
    type Counter: Radium<Item = Self::CounterInner>;
}

trait CounterInner: From<u8> + Shl<u32, Output = Self> + NumericOps + Debug + Copy + Nuclear {}

fn counter_bits<C: CounterInner>() -> u32 {
    size_of::<C>() as u32 * 8
}

fn state_empty<C: CounterInner>() -> C {
    C::from(0u8)
}

fn state_locked<C: CounterInner>() -> C {
    C::from(1u8) << (counter_bits::<C>() - 2)
}

fn state_filled<C: CounterInner>() -> C {
    C::from(2u8) << (counter_bits::<C>() - 2)
}

fn state_mask<C: CounterInner>() -> C {
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
            transmute::<&MaybeUninit<T::Borrowed<'_>>, &T::Borrowed<'_>>(&*self.channel.data.get())
        }
    }
}

impl<T: ChannelBorrow, L: ChannelLock> BorrowChannelGuard<'_, T, L> {
    pub fn with<'b>(&'b mut self, f: impl FnOnce(T::Borrowed<'b>)) {
        f(unsafe {
            transmute::<T::Borrowed<'_>, T::Borrowed<'_>>(
                (*self.channel.data.get()).assume_init_read(),
            )
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
