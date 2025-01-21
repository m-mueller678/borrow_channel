use borrow_channel::BorrowChannel;
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
fn local() {
    let channel = Rc::new(BorrowChannel::<&String, _>::new_unsync());
    let mut_channel = Rc::new(BorrowChannel::<&mut String, _>::new_unsync());
    let reads_a = || {
        channel.borrow().with(|a| {
            assert_eq!(&make_string(1), a);
        })
    };
    let writes_a = || mut_channel.borrow().with(|a| *a = make_string(1));
    let mut a = make_string(0);
    mut_channel.lend(&mut a, writes_a);
    channel.lend(&mut a, reads_a);
}
