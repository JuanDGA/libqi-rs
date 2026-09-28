#[qi_macros::object]
trait MyObject {
    #[qi::method(name = "goTo")]
    async fn go_to(&self, position: i32) -> Result<(), String>;

    #[qi::property]
    fn position(&self) -> i32;

    #[qi::signal]
    fn moving(&self) -> bool;

    fn local_helper(&self);
}

#[test]
fn test_object() {}

#[test]
fn test_object_compile_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/object/fail.rs");
}
