//! To declare a span:
//! ```
//! use stencil::*;
//! fn myExistingFunctionRelatedToKittens() {
//!   outln!("kittens"); // measures the span from now until it's dropped
//!   // kitten related functionality
//! }
//! ```
//!
//! To log flame graphs:
//! ```
//! use stencil::*;
//! enable_flame_graph_collection(true);
//! set_flame_graph_handler(Box::new(|trace| {
//!   let flamegraph: String = trace.as_flame_graph_input().unwrap();
//!   Some(format!("flamegraph: {}", flamegraph)) // will be logged
//! }));
//! ```
//!
//! To log flame charts:
//! ```
//! use stencil::*;
//! enable_flame_chart_collection(true);
//! set_flame_chart_handler(Box::new(|callsites| {
//!   let flamechart: String = call_sites_to_flame_chart(callsites);
//!   Some(format!("flamechart: {}", flamechart)) // will be logged
//! }));
//! ```

mod r#async;
mod flamechart;
mod flamegraph;
mod soa;
mod vec_trie;

use once_cell::sync::OnceCell;
use std::future::Future;
use std::sync::atomic::AtomicBool;

pub use flamechart::call_sites_to_flame_chart;
#[doc(hidden)]
pub use flamechart::outline;
pub use flamechart::outline_with;
pub use flamechart::set_flame_chart_handler;
pub use flamechart::set_flame_chart_min_duration;
pub use flamechart::Callsite;
#[doc(hidden)]
pub use flamegraph::Span as FlameGraphSpan;
pub use flamegraph::{set_flame_graph_handler, FlameGraph};

#[doc(hidden)]
pub static FLAME_GRAPH_ENABLED: AtomicBool = AtomicBool::new(false);
#[doc(hidden)]
pub static FLAME_CHART_ENABLED: AtomicBool = AtomicBool::new(false);

pub enum SetConfigOutcome {
  Set,
  AlreadyInit,
}

/// Enable (or disable by passing `false`) the collection of flame graphs.
/// You will probably also want to [set a handler](set_flame_graph_handler).
pub fn enable_flame_graph_collection(enabled: bool) {
  FLAME_GRAPH_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Enable (or disable by passing `false`) the collection of flame charts.
/// You will probably also want to [set a handler](set_flame_chart_handler).
pub fn enable_flame_chart_collection(enabled: bool) {
  FLAME_CHART_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

static ALLOWED_ENTRY_POINTS: OnceCell<Vec<&'static str>> = OnceCell::new();

/// If set, any trace whose outermost span is does not have one of the given names will be ignored.
pub fn limit_entry_points(entry_points: Vec<&'static str>) -> SetConfigOutcome {
  if ALLOWED_ENTRY_POINTS.get().is_none() {
    ALLOWED_ENTRY_POINTS.get_or_init(|| entry_points);
    SetConfigOutcome::Set
  } else {
    SetConfigOutcome::AlreadyInit
  }
}

/// Run future with a dedicated stencil context.
pub fn with_async_context<F: Future>(future: F) -> impl Future<Output = F::Output> {
  flamechart::async_context(flamegraph::async_context(future))
}

/// Declare a span to be traced: for flame graphs and/or flame charts, whatever is enabled.
///
/// The span begins when the macro is called, and ends when the guard it constructs is `drop`ed at
/// the end of the block.
///
/// Call as:
///
/// - `outln!($name)` - named event with file and line number
/// - `outln!($name, $callback)` - named event with file and line number, to be logged using $callback
#[macro_export]
macro_rules! outln {
  ($name:expr) => {
    $crate::outln!(@private, $name, None)
  };
  ($name:expr, $callback:expr) => {
    $crate::outln!(@private, $name, Some($callback))
  };
  (@private, $name:expr, $callback:expr) => {
    let _flamegraph = if $crate::FLAME_GRAPH_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
      Some($crate::FlameGraphSpan::new(
        $name,
        concat!(file!(), ":", line!()),
        $callback,
      ))
    } else {
      None
    };
    let _flamechart = if $crate::FLAME_CHART_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
      if let Some(callback) = $callback {
        $crate::outline_with($name, concat!(file!(), ":", line!()), callback)
      } else {
        $crate::outline($name, concat!(file!(), ":", line!()))
      }
    } else {
      None
    };
  };
}
