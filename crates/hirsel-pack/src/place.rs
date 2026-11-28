//! A synchronous Place's activation authority and its generation-bound invocation state.
use hirsel_code::{Checkout, Loader, Refusal, Registered, Signature, Slot, Stop, Worker};
use hirsel_core_rt::run_stop::{EndReason, Execution};

/// The host retains the slot across replacements. Each activation gives its caller
/// a checkout, an independent execution in this pool and its own launch bindings.
pub struct PlaceSlot<F> {
    slot: Slot<F>,
    pool: u64,
}

impl<F: Signature> PlaceSlot<F> {
    pub fn new(loader: &Loader) -> Result<Self, Stop> {
        Ok(Self {
            pool: super::loader_handle(loader).ok_or(Stop::WrongPool)?,
            slot: loader.slot(),
        })
    }

    /// Run the launch's binder under its own execution before publishing its checkout.
    /// A refusal leaves the slot untouched, including on a shared-image hit.
    pub fn activate<B, E>(
        &self,
        expected: u64,
        bind: impl FnOnce(&Execution) -> Result<(Registered<F>, B), E>,
    ) -> Result<Place<B>, PlaceError<E>> {
        let mut execution =
            Execution::in_pool(self.pool).ok_or(PlaceError::Slot(Stop::WrongPool))?;
        let bound = bind(&execution);
        if let Some(reason) = execution.end_reason() {
            return Err(PlaceError::Ended(reason));
        }
        let (registration, bindings) = bound.map_err(PlaceError::Binding)?;
        execution.park();
        let checkout = self
            .slot
            .activate_checkout(expected, &registration)
            .map_err(PlaceError::Slot)?;
        Ok(Place {
            checkout,
            execution,
            bindings,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlaceError<E> {
    Binding(E),
    Slot(Stop),
    Ended(EndReason),
}

#[derive(Debug, PartialEq, Eq)]
pub enum DispatchError {
    Entry(Refusal),
    Ended(EndReason),
}

/// A dormant synchronous instance. Worker storage is borrowed for each invocation.
/// B contains the launch's typed arguments, limits and grant owners.
pub struct Place<B> {
    // A bound owner may need its execution custody and checkout during destruction.
    bindings: B,
    execution: Execution,
    checkout: Checkout,
}

impl<B> Place<B> {
    pub fn version(&self) -> u64 {
        self.checkout.version()
    }

    pub fn execution(&self) -> &Execution {
        &self.execution
    }

    pub fn dispatch<F: Signature, R>(
        &self,
        worker: &mut Worker,
        ordinal: usize,
        invoke: impl FnOnce(F, &Execution, &B) -> R,
    ) -> Result<Option<R>, DispatchError> {
        if let Some(reason) = self.execution.end_reason() {
            return Err(DispatchError::Ended(reason));
        }
        worker
            .dispatch_registered(&self.checkout, ordinal, |entry| {
                invoke(entry, &self.execution, &self.bindings)
            })
            .map_err(DispatchError::Entry)
    }
}
