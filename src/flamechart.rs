use crate::r#async::StencilContext;
use crate::soa;
use crate::SetConfigOutcome;
use crate::ALLOWED_ENTRY_POINTS;
use log::{error, warn};
use once_cell::sync::OnceCell;
use serde_derive::{Deserialize, Serialize};
use std::cell::RefCell;
use std::future::Future;
use std::marker::PhantomData;
use std::time::{Duration, SystemTime};

/// Maximum number of events captured under a single root
/// ~600KB of overhead per thread where tracing is used, including other fields
const CAPACITY: usize = 100_000;

/// Size, in characters, at which trace output is split into multiple lines
const MAXIMUM_TRACE_LINE_SIZE: usize = 8000;

/// Generational index
#[derive(Debug, Clone, Copy)]
struct IndexRef {
  index: usize,
  generation: usize,
  uuid: usize,
}

/// Description of an event span
#[derive(Debug, Serialize, Deserialize)]
pub struct Callsite {
  name: &'static str,
  loc: &'static str,
  start: SystemTime,
  end: Option<Duration>,
  parent: Option<usize>,
  uuid: usize,
  depth: usize,
}

impl Callsite {
  /// The registered name of this span
  pub fn name(&self) -> &'static str {
    self.name
  }
  /// The filepath and line of this span
  pub fn loc(&self) -> &'static str {
    self.loc
  }
  /// The start time for this span,
  /// as a delta against the oldest parent in
  /// the span tree
  pub fn start_time(&self) -> &SystemTime {
    &self.start
  }
  /// The end time for this span,
  /// as a delta against the oldest parent in
  /// the span tree
  pub fn end_time(&self) -> Option<&Duration> {
    self.end.as_ref()
  }
  /// The immediate parent of this span in the span array
  pub fn parent_site_index(&self) -> Option<usize> {
    self.parent
  }
}

/// Guard for a Callsite lifetime
/// Can't be moved between threads, shouldn't be moved at all
pub struct Until {
  source: IndexRef,
  //Prevents passing Until off to a thread where it doesn't belong
  // FIXME: Once you are ready for async and find that code doesn't compile because it is not `Send`,
  //  just remove this `_no_send` field. This will remove some of the guard rails against incorrect
  //  API use, but with proper use of `with_async_context`, should be fine.
  _no_send: PhantomData<*const ()>,
}

impl Drop for Until {
  fn drop(&mut self) {
    STENCIL_TRACE.with(|s| s.borrow_mut().pop(&self.source));
  }
}

pub type LoggingCallback = Box<dyn Fn(&[Callsite]) -> Option<String> + Send + Sync>;

struct StencilThreadData {
  /// The pre-sized array for holding current Callsites
  trace: soa::SpeedVec<'static, Callsite, usize>,
  /// The current active span
  current: Option<usize>,
  /// The start time of the current tree
  start_time: Option<SystemTime>,
  /// Logging callback for the current tree
  callback: Option<LoggingCallback>,
  /// Monotonically growing generation
  generation: usize,
  /// Monotonically growing (per generation) callsite id
  uuid: usize,
  /// Maximum depth from stencil root
  max_depth: usize,
  /// data corruption has occured
  consistency_violation: bool,
  /// data was discarded purely for volume
  trimmed: bool,
}

impl StencilThreadData {
  fn new() -> Self {
    Self {
      trace: soa::SpeedVec::with_capacity(CAPACITY, &|c| c.depth),
      current: None,
      start_time: None,
      callback: None,
      generation: 0,
      uuid: 0,
      max_depth: 0,
      consistency_violation: false,
      trimmed: false,
    }
  }

  fn unwind(&mut self) {
    if self.trimmed {
      error!(
        "Discarded one or more trace items after exceeding capactity of {}",
        CAPACITY
      );
    }
    let log_item = if let Some(f) = self.callback.take() {
      f(&self.trace.inner())
    } else {
      // Use the registerd handler if it is available
      if let Some(f) = DEFAULT_CALLBACK.get() {
        f(&self.trace.inner())
      } else {
        Some(format!(
          "No trace handler registered - Raw Trace : '{}'",
          serde_json::to_string(self.trace.inner())
            .unwrap_or_else(|_| "Trace Serialization Error".to_string())
        ))
      }
    };

    if let Some(item) = log_item {
      let len = item.chars().count();
      if len > MAXIMUM_TRACE_LINE_SIZE {
        if self.consistency_violation {
          error!("Trace invariant violation - tracing may be invalid for this event",);
        }
        let splits = len / MAXIMUM_TRACE_LINE_SIZE
          + (if len % MAXIMUM_TRACE_LINE_SIZE > 0 {
            1
          } else {
            0
          });
        let mut char_iter = item.chars().peekable();
        let mut ct = 1;

        loop {
          let line: String = (0..MAXIMUM_TRACE_LINE_SIZE)
            .filter_map(|_| char_iter.next())
            .collect();
          warn!("split stencil trace ({}/{}): '{}'", ct, splits, line);
          if char_iter.peek().is_none() {
            break;
          }
          ct += 1;
        }
      }
      if self.consistency_violation {
        error!(
          "Trace invariant violation - tracing may be invalid for this event: '{}'",
          item
        );
      } else {
        warn!("{}", item);
      }
    }
    self.trace.clear();
    self.start_time.take();
    self.current.take();
    self.consistency_violation = false;
    self.trimmed = false;
    self.generation += 1;
  }

  fn push(&mut self, name: &'static str, loc: &'static str) -> Option<Until> {
    if self.trace.len() == 0 {
      if let Some(allowed_entry_points) = ALLOWED_ENTRY_POINTS.get() {
        if !allowed_entry_points.contains(&name) {
          return None;
        }
      }
    }
    let parent = self.current;
    let generation = self.generation;
    self.start_time.get_or_insert_with(SystemTime::now);
    let start = SystemTime::now();
    let depth = parent
      .as_ref()
      .and_then(|idx| self.trace.get(*idx).map(|item| item.depth + 1))
      .unwrap_or(0);
    if parent.is_some() && depth == 0 {
      //The parent of this entry was trimmed, so this entry is trimmed
      return None;
    }
    if self.trace.len() >= CAPACITY && depth >= self.max_depth {
      //This entry is going to be trimmed later, but we can short-circuit and remove it now
      return None;
    }
    let uuid = self.uuid;
    self.uuid += 1;
    let callsite = Callsite {
      name,
      loc,
      start,
      end: None,
      parent,
      uuid,
      depth,
    };
    self.trace.push(callsite);
    if self.max_depth < depth {
      self.max_depth = depth;
    }
    let current_index = if self.trace.len() <= CAPACITY {
      self.trace.len() - 1
    } else {
      self.trimmed = true;

      let mut max_depth_iter = self
        .trace
        .iter_fast()
        .enumerate()
        .filter_map(|(idx, depth)| {
          if *depth == self.max_depth {
            Some(idx)
          } else {
            None
          }
        });
      if let Some(swap_idx) = max_depth_iter.next() {
        if max_depth_iter.next().is_none() {
          self.max_depth -= 1;
        }
        self.trace.swap_remove(swap_idx);
        swap_idx
      } else {
        self.consistency_violation = true;
        return None;
      }
    };
    self.current = Some(current_index);
    Some(Until {
      source: IndexRef {
        index: current_index,
        generation,
        uuid,
      },
      _no_send: PhantomData,
    })
  }

  fn pop(&mut self, i: &IndexRef) {
    if self.generation != i.generation {
      error!(
        "Trace desynced - dropped guard {:?} in generation {}",
        i, self.generation,
      );
    }
    let uuid_cmp = self.trace.get(i.index).map(|item| item.uuid != i.uuid);
    if uuid_cmp.is_none() || uuid_cmp.expect("UUID Compare") {
      //index ref is invalid
      return;
    }
    let end = self.trace.get(i.index).and_then(|t| t.start.elapsed().ok());
    let parent = self.trace.get_mut(i.index).as_mut().and_then(|callsite| {
      callsite.end = end;
      callsite.parent
    });
    if parent.is_some() {
      self.current = parent;
      if end
        .and_then(|e| DURATION_MINIMUM.get().map(|min| e < *min))
        .unwrap_or(false)
      {
        // It is safe to discard an short open trace via swap-remove
        // because the traces 'after' it are its children, which were logically also
        // discarded for the same reason; and those 'before' it don't have a reference
        // to it
        self.trace.swap_remove(i.index);
      }
    } else {
      self.unwind();
    }
  }
}

/// Convert an array of `Callsite`s into a 'stackcollapse'
/// equivalent to the FlameGraph pre-processing step.
/// The output of this function is suitable to directly
/// render as part of a flamegraph.
/// Should be used offline as a post-process step for traces using
/// the default handler; as it is both allocating and comparatively slow.
pub fn call_sites_to_flame_chart(stack: &[Callsite]) -> String {
  let shadow = compute_shadow(stack);
  stack
    .iter()
    .enumerate()
    .map(|(idx, item)| {
      let name = get_all_parents(stack, idx);
      format!(
        "{} {}\n",
        name,
        item.end.map(|e| e.as_micros() + shadow[idx]).unwrap_or(0),
      )
    })
    .collect()
}

/// Helper for computing non-shadowed parent time
/// Absurdly non-optimal, but stack size is limited
/// and should only be called in offline situations.
fn compute_shadow(stack: &[Callsite]) -> Vec<u128> {
  let mut shadow = vec![0; stack.len()];
  let mut traversal: Vec<(usize, &Callsite)> = stack.iter().enumerate().collect();
  traversal.sort_by_key(|k| k.0);
  traversal.reverse();
  for (_, site) in traversal {
    if let Some(p) = site.parent {
      shadow[p] += site.end.map(|end| end.as_micros()).unwrap_or(0);
    }
  }
  shadow
}

/// Helper for collapsing Callsites into stackcollapse
fn get_all_parents(tree: &[Callsite], target: usize) -> String {
  let mut result = vec![];
  let mut target = Some(target);
  while let Some(t) = target {
    result.push(format!("{}:{}`{}", tree[t].name, tree[t].loc, t));
    target = tree[t].parent;
  }
  result.reverse();
  result.join(";")
}

static DEFAULT_CALLBACK: OnceCell<LoggingCallback> = OnceCell::new();
static DURATION_MINIMUM: OnceCell<Duration> = OnceCell::new();

thread_local!(static STENCIL_TRACE: RefCell<StencilThreadData> = RefCell::new(StencilThreadData::new()));

pub(crate) fn async_context<F: std::future::Future>(future: F) -> impl Future<Output = F::Output> {
  StencilContext::with_stencil(future, StencilThreadData::new(), &STENCIL_TRACE)
}

/// Set the callback to invoke whenever a flame chart trace is complete. If the callback returns
/// `Some(msg)`, `msg` will be logged. If it returns None, it won't log, and the only effect will
/// be any side-effects of the callback (which you can use to handle the trace any way you like).
///
/// Remember also to [enable_flame_chart_collection].
pub fn set_flame_chart_handler(handler: LoggingCallback) -> SetConfigOutcome {
  if DEFAULT_CALLBACK.get().is_none() {
    DEFAULT_CALLBACK.get_or_init(|| handler);
    SetConfigOutcome::Set
  } else {
    SetConfigOutcome::AlreadyInit
  }
}

/// Set the minimum duration entry to be traced.
///
/// Remember also to [enable_flame_chart_collection].
pub fn set_flame_chart_min_duration(minimum: Duration) -> SetConfigOutcome {
  if DURATION_MINIMUM.get().is_none() {
    DURATION_MINIMUM.get_or_init(|| minimum);
    SetConfigOutcome::Set
  } else {
    SetConfigOutcome::AlreadyInit
  }
}

/// PREFER the outln! macro!
/// Create a new span named `name` and track it until the returned guard goes out of scope.
pub fn outline(name: &'static str, loc: &'static str) -> Option<Until> {
  STENCIL_TRACE.with(|s| s.borrow_mut().push(name, loc))
}

/// PREFER the outln! macro!
/// Create a new span named `name` and track it until the returned guard goes out of scope.
/// If there isn't a callback yet to control logging the span, registers one.
pub fn outline_with(
  name: &'static str,
  loc: &'static str,
  callback: LoggingCallback,
) -> Option<Until> {
  STENCIL_TRACE.with(|s| {
    if (*s.borrow()).callback.is_none() {
      (*s.borrow_mut()).callback.replace(callback);
    }
  });
  outline(name, loc)
}

/// Helper for creating spans. Avoids two common pitfalls of the function version:
/// 1) Forgetting to bind the span guard - spans create a guard that has to be bound to a symbol, else the span immediately closes
/// 2) Moving the span guard - dropping guards out of order can cause issues with tracing (for example, putting one of the guards into a Box)
/// This macro automatically binds the guard, and due to macro hygene the guard is immovable.
/// Call as:
/// outln!() - anonymous event with file and line number
/// outln!($name) - named event with file and line number
/// outln!($name, $callback) - named event with file and line number, to be logged using $callback
#[cfg(test)]
macro_rules! outln {
  ($name:expr, $callback:expr) => (
    outln!(wrap outline_with($name, outln!(file@line), $callback));
  );
  ($name:expr) => (
    outln!(wrap outline($name, outln!(file@line)));
  );
  () => (
    outln!("");
  );
  (file@line) => (
    concat!(file!(), ":", line!())
  );
  (wrap $until:expr) => (
    let _outln = $until;
  );
}

mod test {
  #[test]
  fn simple() {
    use super::*;
    outln!(
      "A",
      Box::new(|items| {
        assert_eq!(
          items
            .iter()
            .map(|i| (i.name.chars().last(), i.parent))
            .collect::<Vec<_>>(),
          vec![
            (Some('A'), None),
            (Some('B'), Some(0)),
            (Some('C'), Some(1))
          ]
        );
        None
      })
    );
    outln!("B");
    outln!("C");
  }
  #[test]
  fn multiple_children() {
    use super::*;
    outln!(
      "A",
      Box::new(|items| {
        assert_eq!(
          items
            .iter()
            .map(|i| (i.name.chars().last(), i.parent))
            .collect::<Vec<_>>(),
          vec![
            (Some('A'), None),
            (Some('B'), Some(0)),
            (Some('C'), Some(0)),
            (Some('D'), Some(2))
          ]
        );
        None
      })
    );
    {
      outln!("B");
    }
    outln!("C");
    outln!("D");
  }
  #[test]
  fn generations() {
    use super::*;
    std::thread::spawn(|| {
      STENCIL_TRACE.with(|s| assert_eq!(s.borrow().generation, 0));
      {
        outln!("A");
        outln!("B");
      }
      {
        STENCIL_TRACE.with(|s| assert_eq!(s.borrow().generation, 1));
        outln!("C");
        STENCIL_TRACE.with(|s| assert_eq!(s.borrow().generation, 1));
      }
      STENCIL_TRACE.with(|s| assert_eq!(s.borrow().generation, 2));
    })
    .join()
    .unwrap();
  }

  #[test]
  fn no_drop() {
    use super::*;
    outln!(
      "Top-level",
      Box::new(|items| {
        assert_eq!(items.len(), CAPACITY);
        let max_depth = items
          .iter()
          .map(|item| item.depth)
          .fold(0, |a, b| if a > b { a } else { b });
        assert_eq!(max_depth, CAPACITY / 2);
        None
      })
    );
    let mut container = vec![];
    {
      for _ in 0..(CAPACITY * 2) {
        container.push(outline("stacked", "here"));
      }
      // Don't convert this to something more efficient, we need to
      // control the drop order, specifically dropping newest first
      while !container.is_empty() {
        container.pop();
      }
    }
    {
      for _ in 0..(CAPACITY * 2) {
        container.push(outline("stacked", "here"));
      }
    }
  }
  #[test]
  fn depth_fan() {
    use super::*;
    {
      static LAYER: usize = 4;
      outln!(
        "Top-level",
        Box::new(|items| {
          let a_ct = LAYER;
          let b_ct = LAYER * LAYER;
          let c_ct = LAYER * LAYER * LAYER;
          let remainder = CAPACITY - (a_ct + b_ct + c_ct + 1);
          assert_eq!(
            items.iter().filter(|item| item.name == "Top-level").count(),
            1
          );
          assert_eq!(items.iter().filter(|item| item.name == "a").count(), a_ct);
          assert_eq!(items.iter().filter(|item| item.name == "b").count(), b_ct);
          assert_eq!(items.iter().filter(|item| item.name == "c").count(), c_ct);
          assert_eq!(
            items.iter().filter(|item| item.name == "inner").count(),
            remainder
          );
          assert_eq!(items.iter().filter(|item| item.parent.is_none()).count(), 1);
          assert_eq!(
            items
              .iter()
              .filter_map(|item| item
                .parent
                .and_then(|parent| items.get(parent).filter(|p| p.name == "Top-level")))
              .count(),
            a_ct,
          );
          assert_eq!(
            items
              .iter()
              .filter_map(|item| item
                .parent
                .and_then(|parent| items.get(parent).filter(|p| p.name == "a")))
              .count(),
            b_ct,
          );
          assert_eq!(
            items
              .iter()
              .filter_map(|item| item
                .parent
                .and_then(|parent| items.get(parent).filter(|p| p.name == "b")))
              .count(),
            c_ct,
          );
          assert_eq!(
            items
              .iter()
              .filter_map(|item| item
                .parent
                .and_then(|parent| items.get(parent).filter(|p| p.name == "c")))
              .count(),
            remainder
          );
          None
        })
      );
      for _ in 0..LAYER {
        outln!("a");
        for _ in 0..LAYER {
          outln!("b");
          for _ in 0..LAYER {
            outln!("c");
            for _ in 0..CAPACITY {
              outln!("inner");
            }
          }
        }
      }
    }
  }
}
