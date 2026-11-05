//! Typed operation-local single flight. The wait graph follows salsa's
//! runtime/dependency_graph.rs add_edge-before-block protocol. Unlike S1
//! evaluation, an illegal fact cycle returns its key and never duplicates work.
use crate::ViewRefusal;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FactError {
    Refused,
    Admission,
    Unavailable,
    View(ViewRefusal),
    Cycle(String),
    Aborted,
}
impl From<FactError> for hirsel_types::QueryError {
    fn from(error: FactError) -> Self {
        match error {
            FactError::View(error) => error.into(),
            error => Self::Operational(hirsel_types::OperationalFailure {
                operation: "typed fact".into(),
                message: format!("{error:?}"),
            }),
        }
    }
}
#[derive(Default)]
pub struct FactGraph(Mutex<HashMap<ThreadId, Wait>>);
struct Wait {
    owner: ThreadId,
    token: Arc<()>,
}
impl FactGraph {
    fn wait(&self, owner: ThreadId, token: &Arc<()>, key: &impl Debug) -> Result<(), FactError> {
        let thread = std::thread::current().id();
        let mut waits = self.0.lock().unwrap();
        let mut next = owner;
        loop {
            if next == thread {
                return Err(FactError::Cycle(format!("{key:?}")));
            }
            match waits.get(&next) {
                Some(wait) => next = wait.owner,
                None => break,
            }
        }
        waits.insert(
            thread,
            Wait {
                owner,
                token: token.clone(),
            },
        );
        Ok(())
    }
    fn finish(&self, token: &Arc<()>) {
        self.0
            .lock()
            .unwrap()
            .retain(|_, wait| !Arc::ptr_eq(&wait.token, token));
    }
}
enum Slot<V, E> {
    Pending { owner: ThreadId, token: Arc<()> },
    Ready(Result<Arc<V>, E>),
}
struct Row<V, E> {
    slot: Slot<V, E>,
    _lease: Arc<dyn Send + Sync>,
}
pub struct FactTable<K, V, E = FactError> {
    rows: Mutex<HashMap<K, Row<V, E>>>,
    finished: Condvar,
    graph: Arc<FactGraph>,
}
impl<K, V, E> Default for FactTable<K, V, E> {
    fn default() -> Self {
        Self::new(Arc::default())
    }
}
impl<K, V, E> FactTable<K, V, E> {
    pub fn new(graph: Arc<FactGraph>) -> Self {
        Self {
            rows: Mutex::default(),
            finished: Condvar::new(),
            graph,
        }
    }
    pub fn graph(&self) -> Arc<FactGraph> {
        self.graph.clone()
    }
    pub fn evict_ready(&self) -> u64
    where
        K: Eq + Hash,
    {
        self.evict_ready_when(|_| true)
    }
    pub fn is_empty(&self) -> bool {
        self.rows.lock().unwrap().is_empty()
    }
    pub fn evict_ready_when(&self, evict: impl Fn(&Arc<V>) -> bool) -> u64
    where
        K: Eq + Hash,
    {
        let removed = {
            let mut rows = self.rows.lock().unwrap();
            let keys = rows
                .extract_if(|_, row| match &row.slot {
                    Slot::Ready(Ok(value)) => evict(value),
                    Slot::Ready(Err(_)) => true,
                    Slot::Pending { .. } => false,
                })
                .collect::<Vec<_>>();
            if rows.is_empty() {
                rows.shrink_to_fit();
            }
            keys
        };
        let count = removed.len() as u64;
        drop(removed);
        count
    }
}
impl<K: Clone + Eq + Hash + Debug, V, E: Clone + From<FactError>> FactTable<K, V, E> {
    /// Admission runs outside both locks. Only immutable, key-determined errors
    /// pass `retain`; operational refusals release the claim and wake waiters.
    pub fn get_or_try_init(
        &self,
        key: K,
        admit: impl FnOnce() -> Result<Arc<dyn Send + Sync>, E>,
        compute: impl FnOnce() -> Result<Arc<V>, E>,
        retain: impl FnOnce(&Result<Arc<V>, E>) -> bool,
    ) -> Result<Arc<V>, E> {
        let mut lease = None;
        let mut admit = Some(admit);
        let token = loop {
            let mut rows = self.rows.lock().unwrap();
            match rows.get(&key).map(|row| &row.slot) {
                Some(Slot::Ready(value)) => return value.clone(),
                Some(Slot::Pending { owner, token }) => {
                    let token = token.clone();
                    self.graph.wait(*owner, &token, &key)?;
                    rows = self.finished.wait(rows).unwrap();
                    self.graph
                        .0
                        .lock()
                        .unwrap()
                        .remove(&std::thread::current().id());
                    drop(rows);
                }
                None => {
                    let Some(lease) = lease.take() else {
                        drop(rows);
                        lease = Some(admit.take().expect("admission called once")()?);
                        continue;
                    };
                    let token = Arc::new(());
                    rows.insert(
                        key.clone(),
                        Row {
                            slot: Slot::Pending {
                                owner: std::thread::current().id(),
                                token: token.clone(),
                            },
                            _lease: lease,
                        },
                    );
                    break token;
                }
            }
        };
        let mut claim = Claim {
            table: self,
            key,
            token,
            completed: false,
        };
        let value = compute();
        let keep = retain(&value);
        let removed = {
            let mut rows = self.rows.lock().unwrap();
            let removed = if keep {
                rows.get_mut(&claim.key).expect("pending claim").slot = Slot::Ready(value.clone());
                None
            } else {
                rows.remove(&claim.key)
            };
            self.graph.finish(&claim.token);
            self.finished.notify_all();
            removed
        };
        claim.completed = true;
        drop(removed);
        value
    }
}
struct Claim<'a, K: Eq + Hash, V, E> {
    table: &'a FactTable<K, V, E>,
    key: K,
    token: Arc<()>,
    completed: bool,
}
impl<K: Eq + Hash, V, E> Drop for Claim<'_, K, V, E> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let removed = {
            let mut rows = self.table.rows.lock().unwrap();
            let removed = rows.remove(&self.key);
            self.table.graph.finish(&self.token);
            self.table.finished.notify_all();
            removed
        };
        drop(removed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lease() -> Result<Arc<dyn Send + Sync>, FactError> {
        Ok(Arc::new(()))
    }
    fn keep(value: &Result<Arc<u32>, FactError>) -> bool {
        value.is_ok()
    }

    #[test]
    fn typed_fact_cycles_and_abort_do_not_publish_or_strand_waiters() {
        let graph = Arc::<FactGraph>::default();
        let first = FactTable::<u32, u32>::new(graph.clone());
        let second = FactTable::<&str, u32>::new(graph.clone());
        let result = first.get_or_try_init(
            17,
            lease,
            || {
                second.get_or_try_init(
                    "nested",
                    lease,
                    || {
                        first.get_or_try_init(
                            17,
                            lease,
                            || panic!("a live producer must never be duplicated"),
                            keep,
                        )
                    },
                    keep,
                )
            },
            keep,
        );
        assert_eq!(result, Err(FactError::Cycle("17".into())));
        assert!(first.is_empty() && second.is_empty());

        let table = FactTable::<u32, u32>::new(graph.clone());
        let ready = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let workers = [1, 2].map(|key| {
                let table = &table;
                let ready = &ready;
                scope.spawn(move || {
                    table.get_or_try_init(
                        key,
                        lease,
                        || {
                            ready.wait();
                            table.get_or_try_init(3 - key, lease, || Ok(Arc::new(9)), keep)
                        },
                        keep,
                    )
                })
            });
            let results = workers.map(|worker| worker.join().unwrap());
            assert!(results.iter().any(
                |result| matches!(result, Err(FactError::Cycle(key)) if key == "1" || key == "2")
            ));
        });

        let table = FactTable::<u32, u32>::new(graph.clone());
        let (running, started) = std::sync::mpsc::sync_channel(0);
        let (abort, aborted) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let table = &table;
            let producer = scope.spawn(move || {
                table.get_or_try_init(
                    31,
                    lease,
                    || {
                        running.send(()).unwrap();
                        aborted.recv().unwrap();
                        panic!("producer aborted before publishing")
                    },
                    keep,
                )
            });
            started.recv().unwrap();
            let waiter =
                scope.spawn(|| table.get_or_try_init(31, lease, || Ok(Arc::new(42)), keep));
            while graph.0.lock().unwrap().is_empty() {
                std::thread::yield_now();
            }
            abort.send(()).unwrap();
            assert!(producer.join().is_err());
            assert_eq!(*waiter.join().unwrap().unwrap(), 42);
        });
        assert_eq!(
            *table
                .get_or_try_init(31, lease, || panic!("ready result lost"), keep)
                .unwrap(),
            42
        );
        assert_eq!(
            table.get_or_try_init(
                32,
                || Err(FactError::Admission),
                || panic!("not admitted"),
                keep
            ),
            Err(FactError::Admission)
        );
        assert_eq!(
            *table
                .get_or_try_init(32, lease, || Ok(Arc::new(7)), keep)
                .unwrap(),
            7
        );
    }
}
