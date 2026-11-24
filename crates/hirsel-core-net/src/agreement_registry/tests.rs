use super::*;
use std::sync::{
    Barrier,
    atomic::{AtomicU64, Ordering::Relaxed},
};

struct Charge<'a>(&'a AtomicU64, u64);
impl<'a> Charge<'a> {
    fn admit(payer: &'a AtomicU64, bytes: u64) -> Self {
        payer.fetch_add(bytes, Relaxed);
        Self(payer, bytes)
    }
}
impl Drop for Charge<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Relaxed);
    }
}

#[test]
fn last_in_flight_share_keeps_registry_protection() {
    let first_payer = AtomicU64::new(0);
    let second_payer = AtomicU64::new(0);
    let registry = Arc::new(AgreementRegistry::default());
    let a = registry.new_share().unwrap();
    let b = registry.new_share().unwrap();
    let shared = [0x78; 32];
    let first_schedule = Charge::admit(&first_payer, 512);
    assert_eq!(
        a.resolve(Side::Client, &shared, Charge::admit(&first_payer, 48)),
        RegistryVerdict::Accepted
    );
    drop(first_schedule);
    assert_eq!(first_payer.load(Relaxed), 48);
    let refused_schedule = Charge::admit(&second_payer, 512);
    assert_eq!(
        b.resolve(Side::Client, &shared, Charge::admit(&second_payer, 48)),
        RegistryVerdict::Issued
    );
    drop(refused_schedule);
    assert_eq!(first_payer.load(Relaxed), 0);
    assert_eq!(second_payer.load(Relaxed), 0);

    let a = registry.new_share().unwrap();
    let b = registry.new_share().unwrap();
    let ready = Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            let charge = Charge::admit(&first_payer, 48);
            ready.wait();
            a.resolve(Side::Client, &shared, charge)
        });
        let right = scope.spawn(|| {
            let charge = Charge::admit(&second_payer, 48);
            ready.wait();
            b.resolve(Side::Client, &shared, charge)
        });
        (left.join().unwrap(), right.join().unwrap())
    });
    assert_ne!(a, b);
    assert_eq!(first_payer.load(Relaxed), 0);
    assert_eq!(second_payer.load(Relaxed), 0);
}
