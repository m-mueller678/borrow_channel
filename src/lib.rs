#![no_std]

use core::cell::Cell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr;
use scopeguard::defer;

unsafe trait ChannelBorrow {
    const IS_SHARED: bool;
    type Borrowed<'a>;
    fn to_ptr(b: Self::Borrowed<'_>) -> *mut ();
    unsafe fn from_ptr<'a>(p: *mut ()) -> Self::Borrowed<'a>;
}

struct LocalBorrowMutChannel<T: ChannelBorrow> {
    ptr: Cell<*mut ()>,
    count: Cell<usize>,
    _p: PhantomData<T>,
}

struct LocalBorrowChannelGuard<'a, T: ChannelBorrow> {
    channel: &'a LocalBorrowMutChannel<T>,
    borrowed: T::Borrowed<'a>,
}

impl<'a, T: ChannelBorrow> Drop for LocalBorrowChannelGuard<'a, T> {
    fn drop(&mut self) {
        self.channel.count.set(self.channel.count.get() - 1)
    }
}

impl<'a, T: ChannelBorrow> Deref for LocalBorrowChannelGuard<'a, T> {
    type Target = T::Borrowed<'a>;

    fn deref(&self) -> &Self::Target {
        &self.borrowed
    }
}

impl<'a, T: ChannelBorrow> DerefMut for LocalBorrowChannelGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.borrowed
    }
}

impl<T: ChannelBorrow> LocalBorrowMutChannel<T> {
    pub fn new() -> Self {
        LocalBorrowMutChannel {
            ptr: Cell::new(ptr::null_mut()),
            count: Cell::new(0),
            _p: PhantomData,
        }
    }

    pub fn lend<R>(&self, p: T::Borrowed<'_>, f: impl FnOnce() -> R) -> R {
        assert!(self.ptr.get().is_null());
        debug_assert!(self.count.get() == 0);
        defer! {
            if self.count.get() != 0{
                abort();
            }
            self.ptr.set(ptr::null_mut());
        }
        self.ptr.set(T::to_ptr(p));
        f()
    }

    pub fn borrow(&self) -> LocalBorrowChannelGuard<'_, T> {
        let p = self.ptr.get();
        let old_count = self.count.get();
        if !T::IS_SHARED {
            assert!(
                old_count == 0,
                "attempt to get multiple borrows into non shared channel"
            );
        }
        self.count.set(old_count + 1);
        LocalBorrowChannelGuard {
            channel: self,
            borrowed: unsafe { T::from_ptr(p) },
        }
    }
}

extern "C" fn abort() {
    // guaranteed to abort
    panic!("abort");
}

unsafe impl<T: Sized> ChannelBorrow for &'static T {
    const IS_SHARED: bool = true;
    type Borrowed<'a> = &'a T;

    fn to_ptr(b: Self::Borrowed<'_>) -> *mut () {
        b as *const T as *const () as *mut ()
    }

    unsafe fn from_ptr<'a>(p: *mut ()) -> Self::Borrowed<'a> {
        unsafe { &*(p as *const T) }
    }
}

unsafe impl<T: Sized> ChannelBorrow for &'static mut T {
    const IS_SHARED: bool = false;
    type Borrowed<'a> = &'a mut T;

    fn to_ptr(b: Self::Borrowed<'_>) -> *mut () {
        b as *mut T as *mut ()
    }

    unsafe fn from_ptr<'a>(p: *mut ()) -> Self::Borrowed<'a> {
        unsafe { &mut *(p as *mut T) }
    }
}
