use crate::r#async::StencilContext;
use crate::vec_trie::{Index, VecTrie, Visitor};
use crate::SetConfigOutcome;
use crate::ALLOWED_ENTRY_POINTS;
use log::warn;
use once_cell::sync::OnceCell;
use std::cell::RefCell;
use std::fmt::Write;
use std::future::Future;
use std::time::{Duration, Instant};

/*****************************************************************************
 * Global settings                                                           *
 *****************************************************************************/

static FLAME_GRAPH_CALLBACK: OnceCell<LoggingCallback> = OnceCell::new();

/// A function to call whenever a flame graph trace is complete.
pub type LoggingCallback = Box<dyn Fn(&FlameGraph) -> Option<String> + Send + Sync>;

/// Set the callback to invoke whenever a flame graph trace is complete. If the callback returns
/// `Some(msg)`, `msg` will be logged. If it returns None, it won't log, and the only effect will
/// be any side-effects of the callback (which you can use to handle the trace any way you like).
///
/// Remember also to [enable_flame_graph_collection].
pub fn set_flame_graph_handler(handler: LoggingCallback) -> SetConfigOutcome {
  if FLAME_GRAPH_CALLBACK.get().is_none() {
    FLAME_GRAPH_CALLBACK.get_or_init(|| handler);
    SetConfigOutcome::Set
  } else {
    SetConfigOutcome::AlreadyInit
  }
}

/*****************************************************************************
 * Measuring spans                                                           *
 *****************************************************************************/

/// Measures the start & end of a span. The start is when it is constructed, the end is when it is
/// dropped. You should not use this type directly; it is only public so that macros may use it.
#[derive(Debug)]
pub struct Span {
  index: Index,
}

impl Span {
  /// DO NOT CALL THIS DIRECTLY. Use [crate::outln!] instead.
  pub fn new(
    name: &'static str,
    loc: &'static str,
    callback: Option<LoggingCallback>,
  ) -> Option<Span> {
    let index = TRACE.with(|s| {
      let mut trace = s.borrow_mut();
      if trace.trie.is_empty() {
        if let Some(allowed_entry_points) = ALLOWED_ENTRY_POINTS.get() {
          if !allowed_entry_points.contains(&name) {
            return None;
          }
        }
      }
      Some(trace.push(name, loc, callback))
    });
    if let Some(index) = index {
      Some(Span { index })
    } else {
      None
    }
  }
}

impl Drop for Span {
  fn drop(&mut self) {
    TRACE.with(|s| s.borrow_mut().pop(self.index));
  }
}

/*****************************************************************************
 * Global trace storage                                                      *
 *****************************************************************************/

thread_local!(static TRACE: RefCell<FlameGraph> = RefCell::new(FlameGraph::new()));

pub(crate) fn async_context<F: std::future::Future>(future: F) -> impl Future<Output = F::Output> {
  StencilContext::with_stencil(future, FlameGraph::new(), &TRACE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallSite {
  name: &'static str,
  loc: &'static str,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Measurement {
  duration: Duration,
  num_invocations: usize,
}

#[derive(Debug, Clone)]
struct StackFrame {
  index: Index,
  start: Instant,
}

/// Global store of flame graph data.
pub struct FlameGraph {
  trie: VecTrie<CallSite, Measurement>,
  stack: Vec<StackFrame>,
  callback: Option<LoggingCallback>,
}

impl FlameGraph {
  fn new() -> FlameGraph {
    FlameGraph {
      trie: VecTrie::new(),
      stack: Vec::new(),
      callback: None,
    }
  }

  fn push(
    &mut self,
    name: &'static str,
    loc: &'static str,
    callback: Option<LoggingCallback>,
  ) -> Index {
    if let Some(callback) = callback {
      if self.callback.is_none() {
        self.callback.replace(callback);
      }
    }
    let call_site = CallSite { name, loc };
    let parent = self.stack.last().map(|frame| frame.index);
    let child = self.trie.insert_child(parent, call_site);
    self.stack.push(StackFrame {
      index: child,
      start: Instant::now(),
    });
    child
  }

  fn pop(&mut self, index: Index) {
    while let Some(frame) = self.stack.pop() {
      if frame.index == index {
        let duration = frame.start.elapsed();
        self.trie.value_mut(index).duration += duration;
        self.trie.value_mut(index).num_invocations += 1;
        break;
      }
    }
    if self.stack.is_empty() {
      self.finish_trace();
    }
  }

  fn finish_trace(&mut self) {
    if let Some(callback) = FLAME_GRAPH_CALLBACK.get() {
      if let Some(logmsg) = callback(&self) {
        warn!("{}", logmsg);
      }
    }
    self.trie.clear();
  }

  /// The duration of the outermost call in this trace.
  pub fn total_duration(&self) -> Duration {
    if let Some(root) = self.trie.root() {
      root.value().duration
    } else {
      Duration::new(0, 0)
    }
  }

  /// The name of the outermost call in this trace.
  pub fn outermost_call(&self) -> &'static str {
    if let Some(root) = self.trie.root() {
      root.key().name
    } else {
      "Empty call stack"
    }
  }

  /// Turn this trace into input for a flame graph library.
  pub fn as_flame_graph_input(&self) -> Option<String> {
    if let Some(root) = self.trie.root() {
      let mut flame = String::new();
      let mut stack = vec![root];
      if write_flame_graph(&mut flame, &mut stack).is_err() {
        None
      } else {
        Some(flame)
      }
    } else {
      None
    }
  }
}

fn write_flame_graph<W: Write>(
  writer: &mut W,
  stack: &mut Vec<Visitor<CallSite, Measurement>>,
) -> Result<(), std::fmt::Error> {
  for (i, node) in stack.iter().enumerate() {
    let name = node.key().name;
    let loc = node.key().loc;
    let calls = node.value().num_invocations;
    write!(writer, "{} {} ({} calls)", name, loc, calls)?;
    if i + 1 < stack.len() {
      write!(writer, ";")?;
    }
  }
  if let Some(node) = stack.last() {
    let mut duration = node.value().duration;
    for child in node.children() {
      duration -= child.value().duration;
    }
    writeln!(writer, " {}", duration.as_micros())?;
    for child in node.children() {
      stack.push(child);
      write_flame_graph(writer, stack)?;
      stack.pop();
    }
  }
  Ok(())
}

#[cfg(test)]
fn write_trace_for_testing<W: Write>(
  writer: &mut W,
  node: Visitor<CallSite, Measurement>,
) -> Result<(), std::fmt::Error> {
  let name = node.key().name;
  let calls = node.value().num_invocations;
  write!(writer, "{} ({} calls)", name, calls)?;
  let childless = node.children().next().is_none();
  if childless {
    write!(writer, "; ")?;
  } else {
    write!(writer, " {{ ")?;
  }
  for child in node.children() {
    write_trace_for_testing(writer, child)?;
  }
  if !childless {
    write!(writer, "}}")?;
  }
  Ok(())
}

#[cfg(test)]
macro_rules! outln {
  ($name:expr) => {
    let _outln = Span::new($name, concat!(file!(), ":", line!()), None);
  };
}

#[test]
fn test_tracing() {
  use std::sync::atomic::AtomicBool;

  // Expository purposes only. Don't ever `outln!` recursive functions!
  fn fib(n: usize) -> usize {
    outln!("fib");
    if is_small(n) {
      n
    } else {
      fib(n - 1) + fib(n - 2)
    }
  }

  fn is_small(n: usize) -> bool {
    outln!("is_small");
    n <= 2
  }

  static IT_RAN: AtomicBool = AtomicBool::new(false);
  set_flame_graph_handler(Box::new(|traces: &FlameGraph| {
    let mut actual = String::new();
    let root = traces.trie.root().unwrap();
    write_trace_for_testing(&mut actual, root).unwrap();
    assert_eq!(
      actual,
      "fib (1 calls) { is_small (1 calls); fib (2 calls) { is_small (2 calls); }}"
    );
    IT_RAN.store(true, std::sync::atomic::Ordering::Relaxed);
    None
  }));

  fib(3);
  assert!(IT_RAN.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
#[ignore]
fn test_perf() {
  // Remember to run with --release. Also, disable the above test; they interfere with each other.
  //
  // A big/complex flame graph had the following performance:
  // - 1ms to construct a very large flame graph. (800 lines, avg. of 15 calls, 60kb.) As far as I
  //   can tell, this time is spent formatting strings.
  // - 600μs to zip and base64 encode it.
  // - 70ns/call to outln!, assuming that 100% of time was spent in tracing. This is for 5.4*10^6
  //   calls to outln!.
  //
  // A reasonably sized flame graph (60 lines) had the following performance:
  // - 120μs to construct a reasonably sized flame graph (60 lines, average line has 15 calls).
  // - 220μs to zip and base64 encode the flame graph.
  // - 70ns/call to outln!, assuming that 100% of time was spent in tracing. This is for 5.4*10^6
  //   calls to outln!.

  // Expository purposes only. Don't ever `outln!` recursive functions!
  fn fib(n: usize) -> usize {
    outln!("fib");
    if is_small(n) {
      2
    } else {
      fib(n - 1) + fib(n - 2) + 2
    }
  }

  fn is_small(n: usize) -> bool {
    outln!("is_small");
    n <= 2
  }

  fn fan(n: usize) -> usize {
    let name: &'static str = Box::leak(Box::new(format!("f{}", n)));
    outln!(name);
    fib(n) + 1
  }

  fn fanout(n: usize) -> usize {
    outln!("fanout");
    let mut sum = 1;
    for i in 0..n {
      sum += fan(i);
    }
    sum
  }

  fn trace_to_b64_gzip(flame_graph: &str) -> Option<String> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(flame_graph.as_bytes()).ok()?;
    let gzipped = encoder.finish().ok()?;
    Some(base64::encode(&gzipped))
  }

  set_flame_graph_handler(Box::new(|traces: &FlameGraph| {
    println!(
      "Total tracing duration: {}ms",
      traces.total_duration().as_millis()
    );
    let now = Instant::now();
    let flamegraph = traces.as_flame_graph_input().unwrap();
    println!("FlameGraph size in bytes: {}", flamegraph.len());
    println!(
      "Time taken to construct flame graph: {}μs",
      now.elapsed().as_micros()
    );
    let now = Instant::now();
    let zipped = trace_to_b64_gzip(&flamegraph).unwrap();
    println!("Time taken to zip: {}μs", now.elapsed().as_micros());
    println!("Zipped len: {}", zipped.len()); // make sure it's actually computed
    None
  }));

  println!("Total num calls: {}", fanout(30));
  panic!("flamegraph::test_perf finished. Remember to disable it again.");
}
