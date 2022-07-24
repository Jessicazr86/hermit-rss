/// An executor, which is run when idling on network I/O.
use crate::net::network_delay;
use async_task::{Runnable, Task};
use concurrent_queue::ConcurrentQueue;
use futures_lite::pin;
use once_cell::sync::Lazy;
use smoltcp::time::{Duration, Instant};
use std::sync::atomic::Ordering;
use std::{
	future::Future,
	sync::{atomic::AtomicBool, Arc},
	task::{Context, Poll, Wake},
};

/// A thread handle type
type Tid = u32;

extern "C" {
	fn sys_getpid() -> Tid;
	fn sys_yield();
	fn sys_wakeup_task(tid: Tid);
	fn sys_set_network_polling_mode(value: bool);
	fn sys_block_current_task_with_timeout(timeout: u64);
}

static QUEUE: Lazy<ConcurrentQueue<Runnable>> = Lazy::new(ConcurrentQueue::unbounded);

fn run_executor() {
	// execute all futures and reschedule them
	// ToDo: don't wake every Runnable immediatly
	//          -> mark futures safe to be detached, if they
	//             register a waker before Pending
	let mut wake_buf = Vec::with_capacity(QUEUE.len());
	while let Ok(runnable) = QUEUE.pop() {
		wake_buf.push(runnable.waker());
		runnable.run();
	}
	for waker in wake_buf {
		waker.wake()
	}
}

/// Spawns a future on the executor.
pub(crate) fn spawn<F, T>(future: F) -> Task<T>
where
	F: Future<Output = T> + Send + 'static,
	T: Send + 'static,
{
	let schedule = |runnable| QUEUE.push(runnable).unwrap();
	let (runnable, task) = async_task::spawn(future, schedule);
	runnable.schedule();
	task
}

struct ThreadNotify {
	/// The (single) executor thread.
	thread: Tid,
	/// A flag to ensure a wakeup is not "forgotten" before the next `block_current_task`
	unparked: AtomicBool,
}

impl ThreadNotify {
	pub fn new() -> Self {
		Self {
			thread: unsafe { sys_getpid() },
			unparked: AtomicBool::new(false),
		}
	}
}

impl Drop for ThreadNotify {
	fn drop(&mut self) {
		debug!("Dropping ThreadNotify!");
	}
}

impl Wake for ThreadNotify {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref()
	}

	fn wake_by_ref(self: &Arc<Self>) {
		// Make sure the wakeup is remembered until the next `park()`.
		let unparked = self.unparked.swap(true, Ordering::AcqRel);
		if !unparked {
			unsafe {
				sys_wakeup_task(self.thread);
			}
		}
	}
}

/// Blocks the current thread on `f`, running the executor when idling.
pub fn block_on<F, T>(future: F, timeout: Option<Duration>) -> Result<T, ()>
where
	F: Future<Output = T>,
{
	thread_local! {
		static CURRENT_THREAD_NOTIFY: Arc<ThreadNotify> = Arc::new(ThreadNotify::new());
	}

	CURRENT_THREAD_NOTIFY.with(|thread_notify| {
		// Polling mode => no NIC interrupts => NIC thread should not run
		unsafe { sys_set_network_polling_mode(true) }
		let start = Instant::now();
		let waker = thread_notify.clone().into();
		let mut cx = Context::from_waker(&waker);
		pin!(future);

		loop {
			// run background tasks
			run_executor();

			if let Poll::Ready(t) = future.as_mut().poll(&mut cx) {
				unsafe {
					// allow interrupts => NIC thread is able to run
					sys_set_network_polling_mode(false);
				}
				return Ok(t);
			}

			if let Some(duration) = timeout {
				if Instant::now() >= start + duration {
					unsafe {
						// allow interrupts => NIC thread is able to run
						sys_set_network_polling_mode(false);
					}
					return Err(());
				}
			}

			let now = Instant::now();
			let delay = network_delay(now).map(|d| d.total_millis()).unwrap_or(0);
			if delay > 100 {
				let unparked = thread_notify.unparked.swap(false, Ordering::AcqRel);
				if !unparked {
					unsafe {
						sys_block_current_task_with_timeout(delay);
						// allow interrupts => NIC thread is able to run
						sys_set_network_polling_mode(false);
						// switch to another task
						sys_yield();
						// Polling mode => no NIC interrupts => NIC thread should not run
						sys_set_network_polling_mode(true);
					}
				}
			}
		}
	})
}
