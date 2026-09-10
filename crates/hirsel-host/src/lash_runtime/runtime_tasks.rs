//! A history generation owns every host callback it starts.
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(crate) struct RuntimeTasks(Arc<Mutex<Option<Vec<tokio::task::JoinHandle<()>>>>>);
impl RuntimeTasks {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Some(Vec::new()))))
    }
    pub(crate) fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        let mut state = self.0.lock().expect("runtime task ownership poisoned");
        if let Some(tasks) = state.as_mut() {
            tasks.retain(|t| !t.is_finished());
            tasks.push(tokio::spawn(work));
        }
    }
    pub(crate) async fn stop(&self) {
        let tasks = self
            .0
            .lock()
            .expect("runtime task ownership poisoned")
            .take()
            .unwrap_or_default();
        for task in &tasks {
            task.abort();
        }
        // Abort is not completion: drain before replacing SQL history/projections.
        for task in tasks {
            let _ = task.await;
        }
    }
}
