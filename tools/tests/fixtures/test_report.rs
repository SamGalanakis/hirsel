#[test]
fn a_pass() {}

#[test]
#[ignore = "the reporting regression deliberately executes a failing body"]
fn b_fail() {
    panic!("fixture failure output");
}
