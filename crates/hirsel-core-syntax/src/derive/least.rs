/// A LEAST: a constant plus `least` calls.
pub(super) struct Least {
    pub(super) fixed: u64,
    pub(super) calls: Vec<String>,
}
impl Least {
    pub(super) fn fixed(n: u64) -> Self {
        Least {
            fixed: n,
            calls: vec![],
        }
    }
    pub(super) fn plus(mut self, other: Least) -> Self {
        self.fixed += other.fixed;
        self.calls.extend(other.calls);
        self
    }
    pub(super) fn text(self) -> String {
        let mut terms = self.calls;
        if self.fixed > 0 || terms.is_empty() {
            terms.insert(0, self.fixed.to_string());
        }
        terms.join(" + ")
    }
}
