use stencil::with_async_context;
use tokio::time::Duration;

const DELAY: Duration = Duration::from_millis(100);

async fn inner(name: &'static str) {
  stencil::outln!("inner");
  eprintln!("{}: inner before delay", name);
  tokio::time::delay_for(DELAY).await;
  eprintln!("{}: inner after delay", name);
}

async fn outer(name: &'static str) {
  stencil::outln!("outer");
  eprintln!("{}: outer before inner", name);
  tokio::time::delay_for(DELAY).await;
  inner(name).await;
  tokio::time::delay_for(DELAY).await;
  eprintln!("{}: outer after inner", name);
}

// Force single thread!
#[tokio::main(basic_scheduler)]
async fn main() {
  stencil::enable_flame_chart_collection(true);
  stencil::enable_flame_graph_collection(true);
  stencil::set_flame_chart_min_duration(Duration::from_millis(0));
  stencil::set_flame_chart_handler(Box::new(|chart| {
    eprintln!("--- chart ---");
    eprintln!("{:#?}", chart);
    eprintln!("--- end chart ---");
    None
  }));
  stencil::set_flame_graph_handler(Box::new(|graph| {
    if let Some(input) = graph.as_flame_graph_input() {
      eprintln!("--- graph ---");
      eprintln!("{}", input);
      eprintln!("--- end graph ---");
    }
    None
  }));
  let first = outer("first");
  let second = outer("second");
  // Comment out the next two lines to see the difference in behavior
  let first = with_async_context(first);
  let second = with_async_context(second);

  futures::join!(first, second);
}
