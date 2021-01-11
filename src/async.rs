use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread::LocalKey;

#[pin_project::pin_project]
pub struct StencilContext<F, D: 'static> {
  #[pin]
  future: F,
  state: D,
  thread_local: &'static LocalKey<RefCell<D>>,
}

impl<F, D> StencilContext<F, D>
where
  F: Future,
  D: 'static,
{
  pub(crate) fn with_stencil(
    future: F,
    state: D,
    thread_local: &'static LocalKey<RefCell<D>>,
  ) -> StencilContext<F, D> {
    StencilContext {
      future,
      state,
      thread_local,
    }
  }
}

impl<F, D> Future for StencilContext<F, D>
where
  F: Future,
  D: 'static,
{
  type Output = F::Output;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let mut this = self.project();
    let state_mut = &mut this.state;
    this
      .thread_local
      .with(|s| std::mem::swap(&mut *s.borrow_mut(), state_mut));
    let result = this.future.poll(cx);
    // Perhaps, should be done via Drop
    this
      .thread_local
      .with(|s| std::mem::swap(&mut *s.borrow_mut(), state_mut));
    result
  }
}
