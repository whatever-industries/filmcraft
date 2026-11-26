//! One read-only bin history worker per editor. A reply belongs only to its requesting view.
use serde_json::Value;
use std::sync::{Arc, Mutex, mpsc};

type Reply<T> = Result<T, String>;

#[derive(Clone, Debug)]
pub(super) struct Pending<T = Value>(Arc<Mutex<mpsc::Receiver<Reply<T>>>>);

impl<T> PartialEq for Pending<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Pending<T> {
    pub fn poll(&self) -> Option<Reply<T>> {
        match self.0.lock().unwrap_or_else(|e| e.into_inner()).try_recv() {
            Ok(reply) => Some(reply),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(tl!("Bin history worker stopped").to_owned())),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
type Job = Box<dyn FnOnce() + Send>;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct Worker {
    jobs: Option<mpsc::Sender<Job>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    pub(super) fn submit<T: Send + 'static>(&mut self, ctx: &egui::Context, read: impl FnOnce() -> Reply<T> + Send + 'static) -> Result<Pending<T>, String> {
        if self.jobs.is_none() {
            let (tx, rx) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("bin-history".into())
                .spawn(move || {
                    for job in rx {
                        job();
                    }
                })
                .map_err(|e| e.to_string())?;
            self.jobs = Some(tx);
        }
        let (reply, rx) = mpsc::channel();
        let pending = Pending(Arc::new(Mutex::new(rx)));
        let view = Arc::downgrade(&pending.0);
        let ctx = ctx.clone();
        let job = Box::new(move || {
            if view.strong_count() != 0 && reply.send(read()).is_ok() {
                ctx.request_repaint();
            }
        });
        if self.jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok()) {
            Ok(pending)
        } else {
            self.jobs = None;
            Err(tl!("Bin history worker stopped").to_owned())
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn late_failure_and_abandoned_queued_read_cannot_replace_the_latest_report() {
        let mut worker = Worker::default();
        let ctx = egui::Context::default();
        let (started, running) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        let old = worker
            .submit(&ctx, move || {
                started.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(30)).unwrap();
                Err::<Value, _>("old bin is damaged".into())
            })
            .unwrap();
        running.recv_timeout(Duration::from_secs(30)).unwrap();
        drop(old);
        let (read, observed) = mpsc::channel();
        let abandoned = worker
            .submit(&ctx, move || {
                read.send(()).unwrap();
                Ok(Value::Null)
            })
            .unwrap();
        drop(abandoned);
        let latest = worker.submit(&ctx, || Ok(Value::from("new bin"))).unwrap();
        assert!(latest.poll().is_none());
        resume.send(()).unwrap();
        let reply = latest.0.lock().unwrap().recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(reply.unwrap(), Value::from("new bin"));
        assert_eq!(observed.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }
}
