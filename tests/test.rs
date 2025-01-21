use borrow_channel::BorrowChannel;
use std::cell::RefCell;
use std::rc::Rc;

const _: () = assert!(cfg!(feature = "unsafe_disable_abort"));

#[test]
#[should_panic]
fn leak_guard() {
    let c = BorrowChannel::<&String, _>::new_sync();
    let s = make_string(42);
    let guard = c.lend(&s, || c.borrow());
    drop(s);
    drop(guard);
}

fn make_string(n: usize) -> String {
    String::from(format!("hello, world {n}!\n")).repeat(5)
}

#[test]
fn get_mut() {
    let channel = Rc::new(BorrowChannel::<&String, _>::new_unsync());
    let mut_channel = Rc::new(BorrowChannel::<&mut String, _>::new_unsync());
    let reads_a = || {
        assert_eq!(&make_string(1), channel.borrow().get_mut());
    };
    let writes_a = || *mut_channel.borrow().get_mut() = make_string(1);
    let mut a = make_string(0);
    mut_channel.lend(&mut a, writes_a);
    channel.lend(&mut a, reads_a);
}

#[test]
fn ref_cell() {
    let channel = Rc::new(BorrowChannel::<&RefCell<String>, _>::new_unsync());
    let reads_a = || {
        assert_eq!(make_string(1), *channel.borrow().get().borrow());
    };
    let writes_a = || *channel.borrow().get().borrow_mut() = make_string(1);
    let multi_guard = || {
        let g1 = channel.borrow();
        let mut g2 = channel.borrow();
        *g1.get().borrow_mut() = make_string(2);
        assert_eq!(*g2.get_mut().borrow(), make_string(2));
    };
    let a = RefCell::new(make_string(0));
    channel.lend(&a, writes_a);
    channel.lend(&a, reads_a);
    channel.lend(&a, multi_guard);
}
