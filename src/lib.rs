#![no_std]

use core::cell::Cell;
use core::marker::PhantomData;
use scopeguard::defer;

unsafe trait ChannelBorrow {
    const IS_SHARED: bool;
    type Borrowed<'a>;
    fn to_ptr(b: Self::Borrowed<'_>) -> *mut ();
    fn from_ptr<F: for<'a> FnOnce(Self::Borrowed<'a>) -> R, R>(ptr: *mut (), f: F) -> R;
}

struct LocalBorrowMutChannel<T: ChannelBorrow> {
    ptr: Cell<*mut ()>,
    count: Cell<usize>,
    _p: PhantomData<T>,
}

impl<T: ChannelBorrow> LocalBorrowMutChannel<T> {
    pub fn new() {}

    pub fn lend<R>(&self, p: T::Borrowed<'_>, f: impl FnOnce() -> R) -> R {
        assert!(self.ptr.get().is_null());
        defer! {
            if self.count.get() != 0{
                abort();
            }
        }
        self.ptr.set(T::to_ptr(p));
        f()
    }

    pub fn borrow<R>(&self, f: impl for<'a> FnOnce(T::Borrowed<'a>) -> R) -> R {
        let p = self.ptr.get();
        let old_count = self.count.get();
        if !T::IS_SHARED {
            assert_eq!(old_count, 0);
        }
        self.count.set(old_count + 1);
        defer!(self.count.set(self.count.get() - 1));
        T::from_ptr(p, f)
    }
}

extern "C" fn abort() {
    panic!("abort");
}

fn main() {}
